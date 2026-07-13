//! Bearer-token registry for the HTTP transport.
//!
//! Ported from Folio's `src/mcp/auth.ts` (TypeScript → Rust), with one
//! deliberate divergence: **Folio permits an `open` (unauthenticated) mode when
//! no token env is set; Pipeline does not.** Pipeline's `/mcp` is a documented
//! remote-code-execution surface — it refuses to start without auth rather than
//! silently serving an open one.
//!
//! Sources, in priority order — first one that yields a usable token wins:
//!
//! | # | Env | Shape |
//! |---|---|---|
//! | 1 | `PIPELINE_TOKENS_FILE` | path → JSON `{"alice": "sk-…", "ci": "sk-…"}` |
//! | 2 | `PIPELINE_TOKENS` | `alice:sk-…,ci:sk-…` — inline, good for compose |
//! | 3 | `PIPELINE_TOKEN` | single shared bearer, registered as `default` |
//!
//! Each token maps to a **principal** — a human-readable name carried into logs
//! and OAuth grants, so a revoked contractor is one line out of a JSON file
//! rather than a rotation for everyone.

use std::collections::BTreeMap;

/// How the registry was configured — surfaced in the startup banner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    /// One shared bearer from `PIPELINE_TOKEN`.
    Single,
    /// Named tokens from `PIPELINE_TOKENS_FILE` or `PIPELINE_TOKENS`.
    Multi,
}

#[derive(Debug, Clone)]
pub struct TokenRegistry {
    pub mode: AuthMode,
    /// principal → token value. Small (< 256) — linear scan is fine and lets us
    /// compare every entry in constant time.
    tokens: BTreeMap<String, String>,
}

impl TokenRegistry {
    /// Build from env. `Err` when no source yields a token — Pipeline then
    /// refuses to start rather than exposing an unauthenticated RCE endpoint.
    pub fn from_env() -> Result<Self, String> {
        if let Some(path) = non_empty("PIPELINE_TOKENS_FILE") {
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| format!("PIPELINE_TOKENS_FILE {path}: {e}"))?;
            let parsed: BTreeMap<String, String> = serde_json::from_str(&raw)
                .map_err(|e| format!("PIPELINE_TOKENS_FILE {path} is not a JSON object: {e}"))?;
            let tokens: BTreeMap<String, String> =
                parsed.into_iter().filter(|(_, v)| !v.is_empty()).collect();
            if tokens.is_empty() {
                return Err(format!("PIPELINE_TOKENS_FILE {path} has no usable entries"));
            }
            return Ok(Self {
                mode: AuthMode::Multi,
                tokens,
            });
        }

        if let Some(inline) = non_empty("PIPELINE_TOKENS") {
            let tokens = parse_inline(&inline);
            if tokens.is_empty() {
                return Err(
                    "PIPELINE_TOKENS set but produced no usable entries · expected \
                     \"name:value,name:value\""
                        .into(),
                );
            }
            return Ok(Self {
                mode: AuthMode::Multi,
                tokens,
            });
        }

        if let Some(single) = non_empty("PIPELINE_TOKEN") {
            return Ok(Self {
                mode: AuthMode::Single,
                tokens: BTreeMap::from([("default".to_owned(), single)]),
            });
        }

        Err(
            "no auth configured · set PIPELINE_TOKENS_FILE | PIPELINE_TOKENS | PIPELINE_TOKEN · \
             refusing to expose remote code execution without a token"
                .into(),
        )
    }

    /// Resolve a presented bearer to its principal.
    ///
    /// ! Compares against EVERY entry with no early exit — an early `return` on
    /// first match would leak, by timing, how far down the list a guess got.
    pub fn lookup(&self, presented: &str) -> Option<&str> {
        let mut found: Option<&str> = None;
        for (name, value) in &self.tokens {
            if constant_time_eq(presented.as_bytes(), value.as_bytes()) {
                found = Some(name.as_str());
            }
        }
        found
    }

    pub fn principals(&self) -> Vec<&str> {
        self.tokens.keys().map(String::as_str).collect()
    }

    /// One-line summary for the startup banner. ✗ ever print token values.
    pub fn describe(&self) -> String {
        match self.mode {
            AuthMode::Single => "single shared bearer (PIPELINE_TOKEN) as \"default\"".to_owned(),
            AuthMode::Multi => format!(
                "{} named token(s): {}",
                self.tokens.len(),
                self.principals().join(", ")
            ),
        }
    }

    #[cfg(test)]
    pub fn for_tests(pairs: &[(&str, &str)]) -> Self {
        Self {
            mode: if pairs.len() > 1 {
                AuthMode::Multi
            } else {
                AuthMode::Single
            },
            tokens: pairs
                .iter()
                .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }
}

fn non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// `alice:sk-1,ci:sk-2` → {alice: sk-1, ci: sk-2}.
/// A token value may itself contain `:` — only the FIRST colon splits.
fn parse_inline(s: &str) -> BTreeMap<String, String> {
    s.split(',')
        .filter_map(|pair| {
            let (name, value) = pair.split_once(':')?;
            let (name, value) = (name.trim(), value.trim());
            (!name.is_empty() && !value.is_empty()).then(|| (name.to_owned(), value.to_owned()))
        })
        .collect()
}

/// Length-independent byte compare. Unequal lengths short-circuit — the length
/// of a bearer token is not a secret, its content is.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Extract a `Authorization: Bearer <token>` value.
pub fn bearer(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_parses_named_pairs() {
        let m = parse_inline("alice:sk-1,ci:sk-2");
        assert_eq!(m.get("alice").unwrap(), "sk-1");
        assert_eq!(m.get("ci").unwrap(), "sk-2");
    }

    #[test]
    fn inline_keeps_colons_inside_the_token_value() {
        // Only the first colon separates — a token containing ':' must survive.
        let m = parse_inline("bot:sk-a:b:c");
        assert_eq!(m.get("bot").unwrap(), "sk-a:b:c");
    }

    #[test]
    fn inline_drops_malformed_entries() {
        let m = parse_inline("good:sk-1,,noseparator,:novalue,name:");
        assert_eq!(m.len(), 1);
        assert!(m.contains_key("good"));
    }

    #[test]
    fn lookup_resolves_principal_and_rejects_impostors() {
        let r = TokenRegistry::for_tests(&[("alice", "sk-1"), ("ci", "sk-2")]);
        assert_eq!(r.lookup("sk-1"), Some("alice"));
        assert_eq!(r.lookup("sk-2"), Some("ci"));
        assert_eq!(r.lookup("sk-3"), None);
        assert_eq!(r.lookup(""), None);
    }

    #[test]
    fn describe_never_leaks_a_token_value() {
        let r = TokenRegistry::for_tests(&[("alice", "sk-secret"), ("ci", "sk-other")]);
        let d = r.describe();
        assert!(d.contains("alice") && d.contains("ci"));
        assert!(!d.contains("sk-secret"), "token value leaked: {d}");
    }

    #[test]
    fn constant_time_eq_behaves_like_eq() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(constant_time_eq(b"", b""));
    }
}
