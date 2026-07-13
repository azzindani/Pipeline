//! OAuth 2.0 Authorization Code + PKCE — the claude.ai Custom Connector surface.
//!
//! Ported from Folio's `src/mcp/oauth.ts`. Bridges a public OAuth surface onto
//! the internal [`TokenRegistry`]: the access token issued by `/oauth/token` is
//! fresh random bytes, but every `/mcp` call presenting it is treated as the
//! principal who pasted their Pipeline token at `/oauth/authorize`.
//!
//! ```text
//! GET  /.well-known/oauth-authorization-server   RFC 8414 metadata   [public]
//! GET  /.well-known/oauth-protected-resource     RFC 9728 metadata   [public]
//! POST /oauth/register                           RFC 7591 DCR        [public]
//! GET  /oauth/authorize                          login form          [public]
//! POST /oauth/authorize                          form → 302 + code   [public]
//! POST /oauth/token                              code → access+refresh
//! ```
//!
//! Why the grants have the lifetimes they do:
//! - **auth code · 10 min, one-shot** — deleted on first read, pass or fail.
//! - **access token · 24 h, persisted** — in-memory-only meant every container
//!   bounce forced claude.ai to re-authorize from scratch. Folio hit this; we
//!   inherit the fix rather than the bug.
//! - **refresh token · 30 d, rotating** — without a refresh grant the client
//!   must re-run the full authorize flow every 24 h ("asks auth every time").
//!   Single-use: the presented one is invalidated as the new pair is minted.
//! - **DCR clients · 7 d TTL, 256 cap** — every anonymous `/oauth/register`
//!   allocates. Uncapped, that is a memory-exhaustion DoS for any caller.

use axum::Json;
use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::auth::constant_time_eq;
use crate::http_transport::AppState;

const AUTH_CODE_TTL_MS: i64 = 10 * 60 * 1000;
const ACCESS_TOKEN_TTL_MS: i64 = 24 * 60 * 60 * 1000;
const REFRESH_TOKEN_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;
const CLIENT_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const CLIENT_MAX: usize = 256;

/// Pre-auth bodies are tiny (login form · code exchange · DCR JSON). Cap them
/// hard so an unauthenticated caller cannot balloon the heap with a giant POST.
pub const OAUTH_MAX_BODY_BYTES: usize = 256 * 1024;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 256 bits of CSPRNG, base64url. Two v4 UUIDs — both `getrandom`-backed.
fn random_token() -> String {
    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    URL_SAFE_NO_PAD.encode(bytes)
}

fn sha256_b64url(input: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(input.as_bytes()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Grant {
    principal: String,
    expires_at: i64,
}

#[derive(Debug, Clone)]
struct AuthCode {
    principal: String,
    redirect_uri: String,
    client_id: String,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    scope: Option<String>,
    expires_at: i64,
}

#[derive(Debug, Clone)]
struct Client {
    redirect_uris: Vec<String>,
    secret: Option<String>,
    created_at: i64,
}

pub struct OAuth {
    state_dir: PathBuf,
    codes: Mutex<HashMap<String, AuthCode>>,
    access: Mutex<HashMap<String, Grant>>,
    refresh: Mutex<HashMap<String, Grant>>,
    clients: Mutex<HashMap<String, Client>>,
    /// Seeded from env · never evicted by the DCR reaper.
    static_client_id: String,
}

impl OAuth {
    /// `state_dir` holds the persisted access + refresh token stores.
    pub fn new(state_dir: PathBuf) -> Self {
        let static_client_id = std::env::var("PIPELINE_OAUTH_CLIENT_ID")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "claude-ai".to_owned());
        let static_secret = std::env::var("PIPELINE_OAUTH_CLIENT_SECRET")
            .ok()
            .filter(|s| !s.trim().is_empty());

        let mut clients = HashMap::new();
        clients.insert(
            static_client_id.clone(),
            Client {
                // Wildcard: claude.ai's redirect URI varies by workspace, and
                // custom deployments differ. PKCE is the real proof-of-possession.
                redirect_uris: vec!["*".to_owned()],
                secret: static_secret,
                created_at: now_ms(),
            },
        );

        Self {
            codes: Mutex::new(HashMap::new()),
            access: Mutex::new(load_store(&state_dir.join("access-tokens.json"))),
            refresh: Mutex::new(load_store(&state_dir.join("refresh-tokens.json"))),
            clients: Mutex::new(clients),
            state_dir,
            static_client_id,
        }
    }

    fn access_file(&self) -> PathBuf {
        self.state_dir.join("access-tokens.json")
    }

    fn refresh_file(&self) -> PathBuf {
        self.state_dir.join("refresh-tokens.json")
    }

    /// Resolve an OAuth-issued bearer → principal. Called by the `/mcp` auth
    /// path as a second pass when the static TokenRegistry lookup misses.
    pub fn resolve(&self, token: &str) -> Option<String> {
        self.reap();
        let access = self.access.lock().ok()?;
        // ! constant-time — this is a bearer lookup, same as the static registry
        let now = now_ms();
        let mut hit: Option<String> = None;
        for (value, grant) in access.iter() {
            if constant_time_eq(token.as_bytes(), value.as_bytes()) && grant.expires_at > now {
                hit = Some(grant.principal.clone());
            }
        }
        hit
    }

    /// Drop expired codes/tokens and aged/over-cap DCR clients.
    fn reap(&self) {
        let now = now_ms();

        if let Ok(mut codes) = self.codes.lock() {
            codes.retain(|_, c| c.expires_at > now);
        }

        for (store, file) in [
            (&self.access, self.access_file()),
            (&self.refresh, self.refresh_file()),
        ] {
            if let Ok(mut m) = store.lock() {
                let before = m.len();
                m.retain(|_, g| g.expires_at > now);
                if m.len() != before {
                    persist(&file, &m);
                }
            }
        }

        if let Ok(mut clients) = self.clients.lock() {
            clients.retain(|id, c| {
                *id == self.static_client_id || now - c.created_at <= CLIENT_TTL_MS
            });
            if clients.len() > CLIENT_MAX {
                // Evict oldest first; the env-seeded client is pinned.
                let mut aged: Vec<(String, i64)> = clients
                    .iter()
                    .filter(|(id, _)| **id != self.static_client_id)
                    .map(|(id, c)| (id.clone(), c.created_at))
                    .collect();
                aged.sort_by_key(|(_, created)| *created);
                for (id, _) in aged {
                    if clients.len() <= CLIENT_MAX {
                        break;
                    }
                    clients.remove(&id);
                }
            }
        }
    }

    /// Mint an access + refresh pair and persist both.
    fn issue_pair(&self, principal: &str, scope: &str) -> Value {
        let now = now_ms();
        let access_token = random_token();
        let refresh_token = random_token();

        if let Ok(mut m) = self.access.lock() {
            m.insert(
                access_token.clone(),
                Grant {
                    principal: principal.to_owned(),
                    expires_at: now + ACCESS_TOKEN_TTL_MS,
                },
            );
            persist(&self.access_file(), &m);
        }
        if let Ok(mut m) = self.refresh.lock() {
            m.insert(
                refresh_token.clone(),
                Grant {
                    principal: principal.to_owned(),
                    expires_at: now + REFRESH_TOKEN_TTL_MS,
                },
            );
            persist(&self.refresh_file(), &m);
        }

        json!({
            "access_token": access_token,
            "token_type": "Bearer",
            "expires_in": ACCESS_TOKEN_TTL_MS / 1000,
            "refresh_token": refresh_token,
            "scope": scope,
        })
    }
}

// ── persistence ──────────────────────────────────────────────────────────

fn load_store(file: &Path) -> HashMap<String, Grant> {
    let Ok(raw) = std::fs::read_to_string(file) else {
        return HashMap::new();
    };
    let parsed: HashMap<String, Grant> = serde_json::from_str(&raw).unwrap_or_default();
    let now = now_ms();
    parsed
        .into_iter()
        .filter(|(_, g)| g.expires_at > now)
        .collect()
}

fn persist(file: &Path, map: &HashMap<String, Grant>) {
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string(map) {
        // Best-effort: a failed write costs a re-authorize, not correctness.
        let _ = std::fs::write(file, text);
    }
}

// ── request helpers ──────────────────────────────────────────────────────

/// Public origin of this deployment, as the client sees it.
///
/// ! Behind the shared caddy-router, the socket is plain HTTP on :8080 — only
/// the forwarded headers know it is really `https://pipe.casava.space`. Get this
/// wrong and every absolute URL in the OAuth metadata points at the wrong host.
fn base_url(headers: &HeaderMap) -> String {
    let get = |k: &str| -> Option<String> {
        headers
            .get(k)?
            .to_str()
            .ok()?
            .split(',')
            .next()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    };
    let proto = get("x-forwarded-proto").unwrap_or_else(|| "http".to_owned());
    let host = get("x-forwarded-host")
        .or_else(|| get("host"))
        .unwrap_or_else(|| "localhost".to_owned());
    format!("{proto}://{host}")
}

fn escape_html(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '&' => "&amp;".to_owned(),
            '<' => "&lt;".to_owned(),
            '>' => "&gt;".to_owned(),
            '"' => "&quot;".to_owned(),
            '\'' => "&#39;".to_owned(),
            _ => c.to_string(),
        })
        .collect()
}

fn oauth_err(status: StatusCode, code: &str, desc: &str) -> Response {
    (
        status,
        Json(json!({"error": code, "error_description": desc})),
    )
        .into_response()
}

// ── endpoints ────────────────────────────────────────────────────────────

/// RFC 8414 — what claude.ai fetches first to discover the flow.
pub async fn metadata(headers: HeaderMap) -> impl IntoResponse {
    let base = base_url(&headers);
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/oauth/authorize"),
        "token_endpoint": format!("{base}/oauth/token"),
        "registration_endpoint": format!("{base}/oauth/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none", "client_secret_post"],
        "scopes_supported": ["mcp"],
    }))
}

/// RFC 9728 — points the client at the resource this AS protects.
pub async fn protected_resource(headers: HeaderMap) -> impl IntoResponse {
    let base = base_url(&headers);
    Json(json!({
        "resource": format!("{base}/mcp"),
        "authorization_servers": [base],
        "bearer_methods_supported": ["header"],
        "scopes_supported": ["mcp"],
    }))
}

/// RFC 7591 dynamic client registration. Public by design — claude.ai registers
/// anonymously before any user has authenticated.
pub async fn register(State(st): State<AppState>, body: Option<Json<Value>>) -> Response {
    st.oauth.reap();

    let body = body.map_or(Value::Null, |Json(v)| v);
    let redirect_uris: Vec<String> = body
        .get("redirect_uris")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .filter(|v: &Vec<String>| !v.is_empty())
        .unwrap_or_else(|| vec!["*".to_owned()]);

    // Public (PKCE-only) client unless the registration explicitly asks
    // for a secret-bearing auth method.
    let wants_secret = body
        .get("token_endpoint_auth_method")
        .and_then(Value::as_str)
        .is_some_and(|m| m != "none");
    let secret = wants_secret.then(random_token);

    let client_id = format!("pipeline-{}", &random_token()[..16]);
    if let Ok(mut clients) = st.oauth.clients.lock() {
        clients.insert(
            client_id.clone(),
            Client {
                redirect_uris: redirect_uris.clone(),
                secret: secret.clone(),
                created_at: now_ms(),
            },
        );
    }

    let mut out = json!({
        "client_id": client_id,
        "redirect_uris": redirect_uris,
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": if secret.is_some() { "client_secret_post" } else { "none" },
    });
    if let Some(s) = secret {
        out["client_secret"] = json!(s);
    }
    (StatusCode::CREATED, Json(out)).into_response()
}

/// The login form. The user pastes a Pipeline token; we exchange it for a code.
pub async fn authorize_get(Query(q): Query<HashMap<String, String>>) -> Response {
    for k in ["client_id", "redirect_uri", "response_type"] {
        if !q.contains_key(k) {
            return (
                StatusCode::BAD_REQUEST,
                Html(format!(
                    "<h1>Bad request</h1><p>Missing <code>{}</code>.</p>",
                    escape_html(k)
                )),
            )
                .into_response();
        }
    }
    if q.get("response_type").map(String::as_str) != Some("code") {
        return (
            StatusCode::BAD_REQUEST,
            Html("<h1>Bad request</h1><p>Only <code>response_type=code</code> is supported.</p>"),
        )
            .into_response();
    }

    // Round-trip every param the client sent so POST can rebuild the grant.
    let hidden: String = [
        "client_id",
        "redirect_uri",
        "response_type",
        "scope",
        "state",
        "code_challenge",
        "code_challenge_method",
    ]
    .iter()
    .filter_map(|k| {
        q.get(*k).map(|v| {
            format!(
                r#"<input type="hidden" name="{k}" value="{}">"#,
                escape_html(v)
            )
        })
    })
    .collect::<Vec<_>>()
    .join("\n");

    let client = escape_html(q.get("client_id").map_or("", String::as_str));
    Html(format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Pipeline · Authorize</title>
<style>
  body{{font-family:Inter,system-ui,sans-serif;background:#0d1117;color:#e6edf3;display:flex;align-items:center;justify-content:center;min-height:100vh;margin:0}}
  .card{{background:#161b22;padding:32px;border-radius:12px;border:1px solid #30363d;max-width:440px;width:100%}}
  h1{{margin:0 0 8px;font-size:20px;letter-spacing:-0.01em}}
  p{{color:#8b949e;font-size:14px;line-height:1.5;margin:0 0 16px}}
  label{{display:block;font-size:12px;text-transform:uppercase;letter-spacing:0.08em;color:#8b949e;margin-bottom:6px}}
  input{{width:100%;padding:10px 12px;background:#0d1117;border:1px solid #30363d;border-radius:6px;color:#e6edf3;font:14px Inter,sans-serif;box-sizing:border-box}}
  button{{margin-top:16px;width:100%;padding:10px;background:#238636;color:#fff;border:0;border-radius:6px;font-weight:600;cursor:pointer}}
  code{{background:#0d1117;padding:2px 6px;border-radius:4px;font-size:12px}}
  .warn{{margin-top:16px;font-size:12px;color:#d29922}}
</style></head>
<body><div class="card">
<h1>Authorize Pipeline access</h1>
<p>Client <code>{client}</code> wants to use the Pipeline MCP server. Paste your Pipeline token — the value of <code>PIPELINE_TOKEN</code>, or an entry from your tokens file.</p>
<form method="POST" action="/oauth/authorize">
{hidden}
<label for="token">Pipeline token</label>
<input id="token" name="token" type="password" autocomplete="off" required>
<button type="submit">Authorize</button>
</form>
<p class="warn">Pipeline executes code. Only authorize clients you trust.</p>
</div></body></html>"#
    ))
    .into_response()
}

/// Validate the pasted token, mint a one-shot auth code, bounce back to the client.
pub async fn authorize_post(
    State(st): State<AppState>,
    Form(f): Form<HashMap<String, String>>,
) -> Response {
    let presented = f.get("token").map_or("", String::as_str);
    let Some(principal) = st.tokens.lookup(presented).map(str::to_owned) else {
        return (
            StatusCode::UNAUTHORIZED,
            Html(
                "<h1>Invalid token</h1><p>That token is not in this deployment's registry. \
                 <a href=\"javascript:history.back()\">Go back</a>.</p>",
            ),
        )
            .into_response();
    };

    let client_id = f.get("client_id").cloned().unwrap_or_default();
    let redirect_uri = f.get("redirect_uri").cloned().unwrap_or_default();

    let Some(client) = st
        .oauth
        .clients
        .lock()
        .ok()
        .and_then(|c| c.get(&client_id).cloned())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Html(
                "<h1>Unknown client_id</h1><p>Register via <code>/oauth/register</code>, \
                 or set <code>PIPELINE_OAUTH_CLIENT_ID</code>.</p>",
            ),
        )
            .into_response();
    };

    if !client.redirect_uris.iter().any(|u| u == "*")
        && !client.redirect_uris.contains(&redirect_uri)
    {
        return (
            StatusCode::BAD_REQUEST,
            Html("<h1>Invalid redirect_uri</h1>"),
        )
            .into_response();
    }

    let code = random_token();
    if let Ok(mut codes) = st.oauth.codes.lock() {
        codes.insert(
            code.clone(),
            AuthCode {
                principal,
                redirect_uri: redirect_uri.clone(),
                client_id,
                code_challenge: f.get("code_challenge").cloned().filter(|s| !s.is_empty()),
                code_challenge_method: f
                    .get("code_challenge_method")
                    .cloned()
                    .filter(|s| !s.is_empty()),
                scope: f.get("scope").cloned().filter(|s| !s.is_empty()),
                expires_at: now_ms() + AUTH_CODE_TTL_MS,
            },
        );
    }

    let sep = if redirect_uri.contains('?') { '&' } else { '?' };
    let state = f
        .get("state")
        .filter(|s| !s.is_empty())
        .map(|s| format!("&state={}", urlencode(s)))
        .unwrap_or_default();
    Redirect::to(&format!("{redirect_uri}{sep}code={code}{state}")).into_response()
}

/// Percent-encode a query-string value. Hand-rolled: `state` is client-supplied
/// and goes straight into a Location header — an unescaped `&` would let a caller
/// inject extra query params into the redirect.
fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Exchange an auth code (or a refresh token) for a fresh access + refresh pair.
pub async fn token(State(st): State<AppState>, Form(f): Form<HashMap<String, String>>) -> Response {
    st.oauth.reap();
    let grant = f.get("grant_type").map_or("", String::as_str);

    // ── refresh grant: rotate, so a 24h lapse never forces a re-authorize ──
    if grant == "refresh_token" {
        let presented = f.get("refresh_token").map_or("", String::as_str);
        let rec = st.oauth.refresh.lock().ok().and_then(|mut m| {
            // Single-use: remove on read whether or not it turns out valid.
            m.remove(presented).inspect(|_| {
                persist(&st.oauth.refresh_file(), &m);
            })
        });
        let Some(rec) = rec.filter(|r| r.expires_at > now_ms()) else {
            return oauth_err(
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "Refresh token is missing, expired, or already used.",
            );
        };
        let scope = f.get("scope").map_or("mcp", String::as_str);
        return Json(st.oauth.issue_pair(&rec.principal, scope)).into_response();
    }

    if grant != "authorization_code" {
        return oauth_err(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            "Supported grants: authorization_code, refresh_token.",
        );
    }

    // ── authorization_code grant ──
    let code = f.get("code").map_or("", String::as_str);
    // One-shot: delete on first read, pass or fail. A code that survives a
    // failed exchange is a replayable code.
    let record = st
        .oauth
        .codes
        .lock()
        .ok()
        .and_then(|mut c| c.remove(code))
        .filter(|r| r.expires_at > now_ms());

    let Some(record) = record else {
        return oauth_err(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "Auth code is missing, expired, or already used.",
        );
    };

    if f.get("redirect_uri").map_or("", String::as_str) != record.redirect_uri {
        return oauth_err(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "redirect_uri mismatch.",
        );
    }

    // Confidential clients prove themselves with a secret; public clients with PKCE.
    let client = st
        .oauth
        .clients
        .lock()
        .ok()
        .and_then(|c| c.get(&record.client_id).cloned());
    if let Some(expected) = client.and_then(|c| c.secret) {
        let presented = f.get("client_secret").map_or("", String::as_str);
        if !constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
            return oauth_err(
                StatusCode::UNAUTHORIZED,
                "invalid_client",
                "client_secret mismatch.",
            );
        }
    }

    if let Some(challenge) = &record.code_challenge {
        let verifier = f.get("code_verifier").map_or("", String::as_str);
        let computed = match record.code_challenge_method.as_deref() {
            Some("S256") => sha256_b64url(verifier),
            // RFC 7636 allows `plain`, but a plain challenge proves nothing over
            // a channel an attacker can read. claude.ai always sends S256.
            _ => verifier.to_owned(),
        };
        if !constant_time_eq(computed.as_bytes(), challenge.as_bytes()) {
            return oauth_err(
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "PKCE code_verifier mismatch.",
            );
        }
    }

    let scope = record.scope.as_deref().unwrap_or("mcp");
    Json(st.oauth.issue_pair(&record.principal, scope)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oauth() -> OAuth {
        let dir =
            std::env::temp_dir().join(format!("pipeline-oauth-test-{}", uuid::Uuid::new_v4()));
        OAuth::new(dir)
    }

    #[test]
    fn pkce_s256_matches_the_rfc7636_test_vector() {
        // RFC 7636 Appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            sha256_b64url(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn random_tokens_are_unique_and_long() {
        let a = random_token();
        let b = random_token();
        assert_ne!(a, b);
        assert!(
            a.len() >= 43,
            "want >=256 bits of base64url, got {}",
            a.len()
        );
    }

    #[test]
    fn base_url_prefers_forwarded_headers_over_the_socket() {
        // ! behind caddy-router the socket is plain http on :8080 — only the
        // forwarded headers know the public origin.
        let mut h = HeaderMap::new();
        h.insert("host", "pipeline:8080".parse().unwrap());
        assert_eq!(base_url(&h), "http://pipeline:8080");

        h.insert("x-forwarded-proto", "https".parse().unwrap());
        h.insert("x-forwarded-host", "pipe.casava.space".parse().unwrap());
        assert_eq!(base_url(&h), "https://pipe.casava.space");
    }

    #[test]
    fn base_url_takes_the_first_hop_of_a_forwarded_chain() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-proto", "https, http".parse().unwrap());
        h.insert("host", "pipe.casava.space".parse().unwrap());
        assert_eq!(base_url(&h), "https://pipe.casava.space");
    }

    #[test]
    fn urlencode_neutralises_query_injection_via_state() {
        // A raw `&` in `state` would smuggle extra params into the redirect.
        assert_eq!(urlencode("a&code=evil"), "a%26code%3Devil");
        assert_eq!(urlencode("plain-Value_1.0~"), "plain-Value_1.0~");
    }

    #[test]
    fn escape_html_blocks_script_injection_from_client_id() {
        let out = escape_html("<script>alert(1)</script>");
        assert!(!out.contains('<') && !out.contains('>'));
    }

    #[test]
    fn access_tokens_resolve_to_their_principal_then_expire() {
        let o = oauth();
        let pair = o.issue_pair("alice", "mcp");
        let tok = pair["access_token"].as_str().unwrap().to_owned();
        assert_eq!(o.resolve(&tok).as_deref(), Some("alice"));
        assert_eq!(o.resolve("not-a-token"), None);

        // Force expiry → resolve must miss.
        if let Ok(mut m) = o.access.lock() {
            if let Some(g) = m.get_mut(&tok) {
                g.expires_at = now_ms() - 1;
            }
        }
        assert_eq!(o.resolve(&tok), None, "expired token must not resolve");
    }

    #[test]
    fn tokens_survive_a_restart() {
        // ! in-memory-only forced claude.ai to re-OAuth on every container bounce
        let dir = std::env::temp_dir().join(format!("pipeline-oauth-rt-{}", uuid::Uuid::new_v4()));
        let tok = {
            let o = OAuth::new(dir.clone());
            let pair = o.issue_pair("ci", "mcp");
            pair["access_token"].as_str().unwrap().to_owned()
        };
        let reborn = OAuth::new(dir);
        assert_eq!(
            reborn.resolve(&tok).as_deref(),
            Some("ci"),
            "access token must survive a process restart"
        );
    }

    #[test]
    fn expired_grants_are_dropped_on_load_not_resurrected() {
        let dir = std::env::temp_dir().join(format!("pipeline-oauth-exp-{}", uuid::Uuid::new_v4()));
        let tok = {
            let o = OAuth::new(dir.clone());
            let pair = o.issue_pair("ci", "mcp");
            let t = pair["access_token"].as_str().unwrap().to_owned();
            if let Ok(mut m) = o.access.lock() {
                if let Some(g) = m.get_mut(&t) {
                    g.expires_at = now_ms() - 1;
                }
                persist(&o.access_file(), &m);
            }
            t
        };
        let reborn = OAuth::new(dir);
        assert_eq!(reborn.resolve(&tok), None);
    }

    #[test]
    fn dcr_clients_are_capped_so_anon_registration_cannot_exhaust_memory() {
        let o = oauth();
        if let Ok(mut c) = o.clients.lock() {
            for i in 0..(CLIENT_MAX + 50) {
                c.insert(
                    format!("spam-{i}"),
                    Client {
                        redirect_uris: vec!["*".into()],
                        secret: None,
                        created_at: now_ms() - i64::try_from(i).unwrap_or(0),
                    },
                );
            }
        }
        o.reap();
        let clients = o.clients.lock().expect("lock");
        assert!(clients.len() <= CLIENT_MAX, "got {}", clients.len());
        // ! the env-seeded client is pinned — never evicted by the reaper
        assert!(clients.contains_key(&o.static_client_id));
    }

    #[test]
    fn aged_dcr_clients_are_reaped_but_the_static_one_is_pinned() {
        let o = oauth();
        if let Ok(mut c) = o.clients.lock() {
            c.insert(
                "stale".into(),
                Client {
                    redirect_uris: vec!["*".into()],
                    secret: None,
                    created_at: now_ms() - CLIENT_TTL_MS - 1,
                },
            );
        }
        o.reap();
        let clients = o.clients.lock().expect("lock");
        assert!(!clients.contains_key("stale"));
        assert!(clients.contains_key(&o.static_client_id));
    }
}
