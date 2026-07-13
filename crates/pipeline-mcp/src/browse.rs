//! A read-only web view of the library — Pipeline's durable record, made lookable-at.
//!
//! Ported from Sift's `shared/browse.py`, which is itself Folio's `/files` pattern with
//! the basic-auth removed. Folio's own comment explains why it went: a browser
//! username/password popup "could never be satisfied by an access token anyway", and
//! handing someone a SECOND credential to read their own record defeats the point of
//! one key everywhere.
//!
//! So: paste the key once as `?token=…`, get an HttpOnly cookie back, browse for 30
//! days. `Authorization: Bearer` works too, for scripts. The cookie holds a *minted
//! session token*, never the API key — see [`crate::oauth::mint_session`].
//!
//! Rendering lives here; the route bodies in `http_transport` stay thin.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Rendered inline rather than downloaded — the point is to *read* the record without a
/// round trip through an editor. Anything not listed here is not served at all.
pub fn inline_type(ext: &str) -> Option<&'static str> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "json" => "application/json",
        "yaml" | "yml" | "toml" | "md" | "txt" | "log" | "diff" | "patch" => {
            "text/plain; charset=utf-8"
        }
        "html" => "text/html; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        _ => return None,
    })
}

/// Machinery, not the record. ! `oauth` holds live access + refresh tokens — serving it
/// would hand every reader of the library a working credential. The dotfile rule below
/// already hides it (the dir is `.oauth`), but a deny-list is cheap and a leaked token
/// is not: belt and braces.
const DENY: &[&str] = &[
    "oauth",
    "oauth-state",
    "tokens.json",
    "memory.db-wal",
    "memory.db-shm",
];

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub href: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: u64,
}

/// Map a URL path to a file under `root`. `None` if it escapes, or does not exist.
///
/// ! `canonicalize` collapses `..` AND follows symlinks, so containment is checked on the
/// fully-resolved path. A symlink inside the library pointing at /etc/passwd cannot be
/// read through this — which a naive `root.join(rel)` would happily serve.
pub fn resolve(root: &Path, relative: &str) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    let target = root
        .join(relative.trim_start_matches('/'))
        .canonicalize()
        .ok()?;
    if !target.starts_with(&root) {
        return None;
    }
    // A denied component anywhere in the path, not just the leaf.
    let denied = target
        .strip_prefix(&root)
        .ok()?
        .components()
        .any(|c| is_hidden_or_denied(&c.as_os_str().to_string_lossy()));
    if denied {
        return None;
    }
    Some(target)
}

pub(crate) fn is_hidden_or_denied(name: &str) -> bool {
    name.starts_with('.') || DENY.contains(&name)
}

/// Directories first, then files; both alphabetical.
pub fn listing(target: &Path, url_path: &str) -> Vec<Entry> {
    let base = url_path.trim_end_matches('/');
    let Ok(rd) = std::fs::read_dir(target) else {
        return Vec::new();
    };

    let mut entries: Vec<Entry> = rd
        .filter_map(Result::ok)
        .filter_map(|child| {
            let name = child.file_name().to_string_lossy().into_owned();
            if is_hidden_or_denied(&name) {
                return None;
            }
            let meta = child.metadata().ok()?;
            let is_dir = meta.is_dir();
            let modified = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            Some(Entry {
                href: format!("{base}/{name}{}", if is_dir { "/" } else { "" }),
                name,
                is_dir,
                size: meta.len(),
                modified,
            })
        })
        .collect();

    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    entries
}

pub(crate) fn human(n: u64) -> String {
    #[allow(clippy::cast_precision_loss)] // display only
    let mut step = n as f64;
    for unit in ["B", "KB", "MB", "GB"] {
        if step < 1024.0 || unit == "GB" {
            return if unit == "B" {
                format!("{step:.0} B")
            } else {
                format!("{step:.1} {unit}")
            };
        }
        step /= 1024.0;
    }
    unreachable!()
}

/// Epoch seconds → `YYYY-MM-DD HH:MM` (UTC).
pub(crate) fn stamp(secs: u64) -> String {
    let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs);
    let dt: chrono::DateTime<chrono::Utc> = t.into();
    dt.format("%Y-%m-%d %H:%M").to_string()
}

pub fn escape(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '&' => "&amp;".into(),
            '<' => "&lt;".into(),
            '>' => "&gt;".into(),
            '"' => "&quot;".into(),
            '\'' => "&#39;".into(),
            _ => c.to_string(),
        })
        .collect()
}

fn crumbs(url_path: &str) -> String {
    let mut out = vec![r#"<a href="/library/">library</a>"#.to_owned()];
    let mut acc = String::from("/library");
    for p in url_path
        .trim_matches('/')
        .split('/')
        .filter(|p| !p.is_empty() && *p != "library")
    {
        acc.push('/');
        acc.push_str(p);
        out.push(format!(r#"<a href="{}/">{}</a>"#, escape(&acc), escape(p)));
    }
    out.join(r#"<span class="sep">/</span>"#)
}

const CSS: &str = r"
  :root{color-scheme:dark}
  body{font:15px/1.5 ui-monospace,SFMono-Regular,Menlo,monospace;background:#0d1117;color:#e6edf3;
       margin:0;padding:32px 20px;max-width:900px;margin-inline:auto}
  h1{font-size:14px;font-weight:600;letter-spacing:.08em;text-transform:uppercase;color:#8b949e;margin:0 0 4px}
  .crumbs{font-size:16px;margin:0 0 24px;word-break:break-all}
  .crumbs a{color:#3fb950;text-decoration:none}
  .crumbs a:hover{text-decoration:underline}
  .sep{color:#30363d;padding:0 6px}
  table{width:100%;border-collapse:collapse}
  th{text-align:left;font-weight:500;color:#8b949e;font-size:12px;text-transform:uppercase;
     letter-spacing:.06em;border-bottom:1px solid #30363d;padding:0 8px 8px}
  td{padding:7px 8px;border-bottom:1px solid #21262d}
  td a{color:#e6edf3;text-decoration:none}
  td a:hover{color:#3fb950}
  .n{text-align:right;color:#8b949e;font-size:13px;white-space:nowrap}
  .empty{color:#8b949e}
  footer{margin-top:28px;color:#484f58;font-size:12px}
";

/// The listing page. Plain, fast, readable on a phone.
pub fn render(url_path: &str, entries: &[Entry], empty_hint: &str) -> String {
    let body = if entries.is_empty() {
        let hint = if empty_hint.is_empty() {
            "Empty."
        } else {
            empty_hint
        };
        format!(r#"<p class="empty">{}</p>"#, escape(hint))
    } else {
        use std::fmt::Write as _;
        let mut rows = String::new();
        for e in entries {
            let _ = write!(
                rows,
                r#"<tr><td><a href="{}">{}{}</a></td><td class="n">{}</td><td class="n">{}</td></tr>"#,
                escape(&e.href),
                if e.is_dir { "📁 " } else { "📄 " },
                escape(&e.name),
                if e.is_dir {
                    "—".to_owned()
                } else {
                    human(e.size)
                },
                stamp(e.modified),
            );
        }
        format!(
            r#"<table><tr><th>name</th><th class="n">size</th><th class="n">modified</th></tr>{rows}</table>"#
        )
    };

    format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Pipeline · library · {}</title>
<style>{CSS}</style></head>
<body>
<h1>Pipeline library</h1>
<div class="crumbs">{}</div>
{body}
<footer>Read-only. The record is on disk under <code>.pipeline/</code> — run history, reports,
digests, sessions. Secrets and the OAuth token store are never served.</footer>
</body></html>"#,
        escape(url_path),
        crumbs(url_path),
    )
}

/// Shown when there is no token and no session. ✗ a WWW-Authenticate header — a browser
/// basic-auth popup can never be satisfied by an access token.
pub fn gate_page(base: &str) -> String {
    format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Pipeline · library</title>
<style>
  :root{{color-scheme:dark}}
  body{{font:15px/1.6 ui-monospace,SFMono-Regular,Menlo,monospace;background:#0d1117;color:#e6edf3;
       display:flex;align-items:center;justify-content:center;min-height:100vh;margin:0;padding:20px}}
  .card{{background:#161b22;border:1px solid #30363d;border-radius:12px;padding:32px;max-width:520px}}
  h1{{margin:0 0 12px;font-size:18px}}
  p{{color:#8b949e;margin:0 0 16px}}
  code{{background:#0d1117;border:1px solid #30363d;border-radius:4px;padding:2px 6px;
        color:#e6edf3;word-break:break-all}}
  input{{width:100%;padding:10px 12px;background:#0d1117;border:1px solid #30363d;border-radius:6px;
        color:#e6edf3;font:14px ui-monospace,monospace;box-sizing:border-box}}
  button{{margin-top:12px;width:100%;padding:10px;background:#238636;color:#fff;border:0;
         border-radius:6px;font-weight:600;cursor:pointer;font-size:14px}}
</style></head>
<body><div class="card">
<h1>Access token required</h1>
<p>The library uses the <strong>same key</strong> as the MCP endpoint — no separate login.
Paste any value from <code>PIPELINE_TOKENS</code> / your tokens file. It becomes a 30-day
session cookie; the key itself is never stored in the browser.</p>
<form method="GET" action="{}">
<input name="token" type="password" placeholder="sk-…" autocomplete="off" required autofocus>
<button type="submit">Open library</button>
</form>
</div></body></html>"#,
        escape(base)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().expect("tempdir");
        let r = d.path();
        fs::create_dir_all(r.join("reports")).unwrap();
        fs::create_dir_all(r.join(".oauth")).unwrap();
        fs::create_dir_all(r.join("oauth")).unwrap();
        fs::write(r.join("reports/run.json"), b"{}").unwrap();
        fs::write(r.join(".oauth/access-tokens.json"), b"{\"sk-secret\":1}").unwrap();
        fs::write(r.join("oauth/access-tokens.json"), b"{\"sk-secret\":1}").unwrap();
        fs::write(r.join("memory.db"), b"binary").unwrap();
        d
    }

    #[test]
    fn resolves_a_normal_path() {
        let d = fixture();
        assert!(resolve(d.path(), "reports/run.json").is_some());
        assert!(resolve(d.path(), "reports").is_some());
    }

    #[test]
    fn traversal_out_of_the_root_is_refused() {
        let d = fixture();
        assert!(resolve(d.path(), "../../../etc/passwd").is_none());
        assert!(resolve(d.path(), "reports/../../etc/passwd").is_none());
    }

    #[test]
    fn symlink_escaping_the_root_is_refused() {
        // ! a naive root.join(rel) would follow this straight out of the library
        let d = fixture();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc", d.path().join("escape")).unwrap();
            assert!(
                resolve(d.path(), "escape/passwd").is_none(),
                "symlink out of the library must not resolve"
            );
        }
    }

    #[test]
    fn the_oauth_token_store_is_never_reachable() {
        // ! serving this would hand every library reader a live credential
        let d = fixture();
        assert!(resolve(d.path(), ".oauth/access-tokens.json").is_none());
        assert!(resolve(d.path(), "oauth/access-tokens.json").is_none());
        assert!(resolve(d.path(), ".oauth").is_none());
        assert!(resolve(d.path(), "oauth").is_none());
    }

    #[test]
    fn listing_hides_machinery_and_sorts_dirs_first() {
        let d = fixture();
        let entries = listing(d.path(), "/library");
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(!names.contains(&".oauth"), "dotfile leaked: {names:?}");
        assert!(!names.contains(&"oauth"), "token store leaked: {names:?}");
        assert!(names.contains(&"reports"));
        // dirs before files
        assert_eq!(names.first(), Some(&"reports"));
    }

    #[test]
    fn only_whitelisted_types_are_servable() {
        assert_eq!(inline_type("json"), Some("application/json"));
        assert!(inline_type("md").is_some());
        // ✗ hand out the SQLite memory db or an arbitrary binary
        assert!(inline_type("db").is_none());
        assert!(inline_type("so").is_none());
        assert!(inline_type("").is_none());
    }

    #[test]
    fn escape_blocks_html_injection_from_a_filename() {
        let out = escape("<script>alert(1)</script>");
        assert!(!out.contains('<') && !out.contains('>'));
    }

    #[test]
    fn crumbs_link_each_ancestor() {
        let c = crumbs("/library/reports/2026/");
        assert!(c.contains(r#"href="/library/""#));
        assert!(c.contains(r#"href="/library/reports/""#));
        assert!(c.contains(r#"href="/library/reports/2026/""#));
    }

    #[test]
    fn human_sizes_read_sensibly() {
        assert_eq!(human(512), "512 B");
        assert_eq!(human(2048), "2.0 KB");
    }
}
