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

use std::fmt::Write as _;
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
            // ! Skip symlinks. `file_type()` is an lstat — it does NOT follow, which is the
            // point: `metadata()` below does, so a symlink to /etc would otherwise list as a
            // directory. Following it was never possible (`resolve` canonicalizes and would
            // 404 the click), but listing it still advertises that the link exists and hands
            // a reader a row that goes nowhere. The record is real files; a symlink in it is
            // either noise or an escape attempt, and neither belongs in the listing.
            if child.file_type().ok()?.is_symlink() {
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

// ─── the file manager ───────────────────────────────────────────────────────────────
//
// A directory listing, not a gallery. Pipeline's artifacts are files: you navigate them
// with a tree, breadcrumbs and a sortable table, the way you navigate any file system.
//
// Server-rendered, one page per directory. No SPA, no client-side routing: the record is
// read over a token'd link on a phone as often as a laptop, and a page that works with
// JS off is worth more here than one that animates.

const CSS: &str = r"
:root{--bg:#0d1117;--fg:#e6edf3;--panel:#161b22;--bd:#30363d;--bd2:#21262d;--mut:#8b949e;
      --mut2:#484f58;--acc:#3fb950;--ok:#3fb950;--fail:#f85149;color-scheme:dark}
:root[data-theme=light]{--bg:#ffffff;--fg:#1f2328;--panel:#f6f8fa;--bd:#d0d7de;--bd2:#e7ecf0;
      --mut:#636c76;--mut2:#8c959f;--acc:#1a7f37;--ok:#1a7f37;--fail:#cf222e;color-scheme:light}
*{box-sizing:border-box}
body{font:14px/1.55 ui-monospace,SFMono-Regular,Menlo,monospace;background:var(--bg);color:var(--fg);margin:0}
header{display:flex;align-items:center;gap:14px;padding:12px 18px;border-bottom:1px solid var(--bd);
       background:var(--panel);position:sticky;top:0;z-index:5;flex-wrap:wrap}
.brand{font-weight:600;letter-spacing:.06em;text-transform:uppercase;font-size:12px;color:var(--mut)}
.crumbs{flex:1;min-width:200px;word-break:break-all}
.crumbs a{color:var(--acc);text-decoration:none}
.crumbs a:hover{text-decoration:underline}
.sep{color:var(--mut2);padding:0 5px}
#q{padding:6px 10px;border-radius:6px;border:1px solid var(--bd);background:var(--bg);color:var(--fg);
   font:inherit;font-size:13px;width:200px}
.tbtn{background:var(--bg);border:1px solid var(--bd);color:var(--fg);border-radius:6px;padding:6px 10px;
      font:inherit;font-size:13px;cursor:pointer}
.tbtn:hover{border-color:var(--acc)}
.wrap{display:flex;align-items:flex-start}
nav{width:210px;flex:none;border-right:1px solid var(--bd);padding:14px 0;min-height:calc(100vh - 49px)}
nav ul{list-style:none;margin:0;padding:0}
nav li{margin:0}
nav a{display:block;padding:5px 10px 5px 18px;color:var(--mut);text-decoration:none;font-size:13px;
      white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
nav a:hover{background:var(--panel);color:var(--fg)}
nav a.on{color:var(--fg);background:var(--panel);border-left:2px solid var(--acc);padding-left:16px}
nav ul ul a{padding-left:32px}
nav ul ul ul a{padding-left:46px}
main{flex:1;min-width:0;padding:14px 18px 40px}
table{width:100%;border-collapse:collapse}
th{text-align:left;font-weight:500;color:var(--mut);font-size:11px;text-transform:uppercase;
   letter-spacing:.06em;border-bottom:1px solid var(--bd);padding:0 8px 8px;white-space:nowrap}
th.s{cursor:pointer;user-select:none}
th.s:hover{color:var(--fg)}
th .ar{opacity:.35;font-size:9px}
td{padding:6px 8px;border-bottom:1px solid var(--bd2);vertical-align:middle}
tr:hover td{background:var(--panel)}
td a{color:var(--fg);text-decoration:none}
td a:hover{color:var(--acc)}
.ic{color:var(--mut);margin-right:7px}
.kind{color:var(--mut);font-size:12px}
.sum{color:var(--mut);font-size:12px;max-width:280px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.n{text-align:right;color:var(--mut);font-size:12px;white-space:nowrap}
.dot{display:inline-block;width:7px;height:7px;border-radius:50%;margin-right:6px;vertical-align:middle}
.dot.ok{background:var(--ok)}.dot.fail{background:var(--fail)}.dot.none{display:none}
.empty{color:var(--mut);padding:24px 8px}
td.act{text-align:right;white-space:nowrap;width:1%}
th.act{width:1%}
.op{background:none;border:1px solid transparent;color:var(--mut);border-radius:5px;padding:2px 6px;
    cursor:pointer;font:inherit;font-size:13px;text-decoration:none;display:inline-block;opacity:0}
tr:hover .op{opacity:1}
.op:hover{border-color:var(--bd);color:var(--fg)}
.op[data-op=delete]:hover{border-color:var(--fail);color:var(--fail)}
.err{background:var(--fail);color:#fff;padding:8px 12px;border-radius:6px;margin:0 0 12px;font-size:13px}
footer{color:var(--mut2);font-size:11px;padding:18px 8px 0;line-height:1.6}
@media(max-width:680px){nav{display:none}.sum,.kind{display:none}.op{opacity:1}}
";

// Restores the theme BEFORE first paint — after would flash dark at a light-theme reader
// on every page load.
const BOOT: &str = r"try{var m=localStorage.getItem('pl-theme');if(m)document.documentElement.dataset.theme=m}catch(e){}";

const JS: &str = r"
var q=document.getElementById('q'),tb=document.getElementById('rows');
// An empty directory renders no table. Guard, or every handler below dies on a null and
// the theme button stops working in exactly the directories you just emptied.
var rows=tb?[].slice.call(tb.querySelectorAll('tr')):[];
function apply(){if(!q)return;var t=(q.value||'').toLowerCase().trim(),n=0;
  rows.forEach(function(r){var up=r.dataset.up==='1';
    var vis=up||!t||r.dataset.s.indexOf(t)>=0;r.style.display=vis?'':'none';if(vis&&!up)n++;});
  var e=document.getElementById('none');if(e)e.style.display=n?'none':'';}
if(q)q.addEventListener('input',apply);
var dir={};
document.querySelectorAll('th.s').forEach(function(th){th.addEventListener('click',function(){
  var k=th.dataset.k;dir[k]=!dir[k];var sgn=dir[k]?1:-1;
  rows.filter(function(r){return r.dataset.up!=='1';}).sort(function(a,b){
    // Directories always lead, whatever the sort — a folder is not a big or small file.
    if(a.dataset.d!==b.dataset.d)return b.dataset.d-a.dataset.d;
    var x=a.dataset[k],y=b.dataset[k];
    if(k==='sz'||k==='mt')return sgn*(x-y);
    return sgn*String(x).localeCompare(String(y));
  }).forEach(function(r){tb.appendChild(r)});
  document.querySelectorAll('th.s .ar').forEach(function(a){a.textContent=''});
  th.querySelector('.ar').textContent=dir[k]?'▲':'▼';});});
document.getElementById('theme').addEventListener('click',function(){
  var cur=document.documentElement.dataset.theme==='light'?'dark':'light';
  document.documentElement.dataset.theme=cur;try{localStorage.setItem('pl-theme',cur)}catch(e){}});

// ── file operations ───────────────────────────────────────────────────────────────────
var HERE=document.body.dataset.rel||'';
function fail(msg){var d=document.createElement('div');d.className='err';d.textContent=msg;
  var m=document.querySelector('main');m.insertBefore(d,m.firstChild);
  setTimeout(function(){d.remove()},6000);}
function op(payload){
  return fetch('/library/op',{method:'POST',headers:{'Content-Type':'application/json'},
    // same-origin: send the session cookie, and nothing else — the token never touches JS.
    credentials:'same-origin',body:JSON.stringify(payload)})
    .then(function(r){return r.json().then(function(j){
      if(!r.ok||!j.ok){fail(j.error||('HTTP '+r.status));return false}
      return true;});})
    .catch(function(e){fail(String(e));return false});
}
function reloadIfOk(ok){if(ok)location.reload()}
document.querySelectorAll('.op[data-op]').forEach(function(b){b.addEventListener('click',function(){
  var tr=b.closest('tr'),path=tr.dataset.p,name=path.split('/').pop(),o=b.dataset.op;
  if(o==='rename'){var n=prompt('Rename to:',name);if(!n||n===name)return;
    op({op:'rename',path:path,name:n}).then(reloadIfOk);}
  else if(o==='move'){var d=prompt('Move into which directory? (path under the library, blank = root)',HERE);
    if(d===null)return;op({op:'move',path:path,dest:d}).then(reloadIfOk);}
  else if(o==='delete'){
    // Say where it GOES, not just that it goes. 'Delete' that silently keeps the file is
    // as confusing as one that silently destroys it.
    if(!confirm('Move to trash/ ?\n\n'+name+'\n\nIt is not destroyed — you can move it back out.'))return;
    op({op:'delete',path:path}).then(reloadIfOk);}
});});
var mk=document.getElementById('mkdir');
if(mk)mk.addEventListener('click',function(){var n=prompt('New folder name:');if(!n)return;
  op({op:'mkdir',path:HERE,name:n}).then(reloadIfOk);});
var upb=document.getElementById('up'),fi=document.getElementById('file');
if(upb)upb.addEventListener('click',function(){fi.click()});
if(fi)fi.addEventListener('change',function(){
  var f=fi.files[0];if(!f)return;
  fetch('/library/upload?dir='+encodeURIComponent(HERE)+'&name='+encodeURIComponent(f.name),
    {method:'POST',credentials:'same-origin',body:f})
    .then(function(r){return r.json().then(function(j){
      if(!r.ok||!j.ok){fail(j.error||('HTTP '+r.status));return}
      location.reload();});})
    .catch(function(e){fail(String(e))});
});
";

/// Depth the sidebar tree expands to. Deep enough to see the shape of the record, shallow
/// enough that a repo clone under `repos/<alias>/` cannot turn one page load into a full
/// recursive walk of somebody else's source tree.
const TREE_DEPTH: usize = 2;
/// A directory with more children than this is a clone or a dump, not navigation. List it,
/// ✗ expand it.
const TREE_FANOUT: usize = 60;

/// Directory-only sidebar tree, bounded in both depth and fan-out.
fn tree(dir: &Path, url_base: &str, current: &str, depth: usize, out: &mut String) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut dirs: Vec<String> = rd
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !is_hidden_or_denied(n))
        .collect();
    if dirs.is_empty() || dirs.len() > TREE_FANOUT {
        return;
    }
    dirs.sort_by_key(|a| a.to_lowercase());

    out.push_str("<ul>");
    for name in dirs {
        let href = format!("{url_base}/{name}");
        // Mark the branch we are inside, not merely the exact page — so a file's parent
        // still reads as "you are here".
        let on = current == href || current.starts_with(&format!("{href}/"));
        let _ = write!(
            out,
            r#"<li><a class="{}" href="{}/">{}</a>"#,
            if on { "on" } else { "" },
            escape(&href),
            escape(&name),
        );
        if depth > 0 {
            tree(&dir.join(&name), &href, current, depth - 1, out);
        }
        out.push_str("</li>");
    }
    out.push_str("</ul>");
}

/// The file-manager page for one directory.
///
/// `rel` is the path under the library root ("" = root). `root` is needed for the sidebar
/// tree, which is always rendered from the top so you can jump anywhere in one click.
#[allow(clippy::too_many_lines)] // one page template · splitting it scatters the markup
pub fn render(root: &Path, rel: &str, url_path: &str, entries: &[Entry], hint: &str) -> String {
    let writable = crate::fsops::writes_enabled();
    let here = format!("/library/{}", rel.trim_matches('/'))
        .trim_end_matches('/')
        .to_owned();
    let mut nav = String::new();
    tree(root, "/library", &here, TREE_DEPTH, &mut nav);

    let mut rows = String::new();

    // Up-one-level. Kept out of the filter and the sort (data-up=1) — a search that hides
    // your way back out of a directory is a trap.
    if !rel.trim_matches('/').is_empty() {
        let parent = rel
            .trim_matches('/')
            .rsplit_once('/')
            .map_or("", |(p, _)| p);
        let _ = write!(
            rows,
            r#"<tr data-up="1"><td colspan="6"><a href="/library/{}"><span class="ic">↰</span>..</a></td></tr>"#,
            escape(parent),
        );
    }

    // Annotate only when the directory is small enough that reading each file is sane.
    let annotate = entries.len() <= crate::library::ANNOTATE_MAX_ENTRIES;
    for e in entries {
        let child_rel = if rel.trim_matches('/').is_empty() {
            e.name.clone()
        } else {
            format!("{}/{}", rel.trim_matches('/'), e.name)
        };
        let kind = crate::library::kind_of(&child_rel);
        let (summary, status) = if e.is_dir || !annotate {
            (String::new(), crate::library::Status::None)
        } else {
            crate::library::describe(&root.join(&child_rel), kind)
        };

        let _ = write!(
            rows,
            r#"<tr data-d="{d}" data-nm="{nml}" data-kd="{kind}" data-sz="{sz}" data-mt="{mt}" data-s="{search}" data-p="{crel}">
<td><a href="{href}"><span class="dot {st}"></span><span class="ic">{ic}</span>{name}</a></td>
<td class="kind">{kindshow}</td><td class="sum">{sum}</td>
<td class="n">{size}</td><td class="n">{when}</td>
<td class="act">{dl}{ops}</td></tr>"#,
            d = u8::from(e.is_dir),
            nml = escape(&e.name.to_lowercase()),
            kind = escape(kind),
            sz = e.size,
            mt = e.modified,
            search = escape(&format!("{} {} {}", e.name, kind, summary).to_lowercase()),
            href = escape(&e.href),
            st = status.css(),
            ic = if e.is_dir { "▸" } else { "·" },
            name = escape(&e.name),
            kindshow = if e.is_dir {
                String::new()
            } else {
                escape(kind)
            },
            sum = escape(&summary),
            size = if e.is_dir {
                "—".to_owned()
            } else {
                human(e.size)
            },
            when = escape(&stamp(e.modified)),
            crel = escape(&child_rel),
            // Download only what we would render anyway — the same whitelist. A "download"
            // button on memory.db would hand over the database the whitelist exists to
            // withhold.
            dl = if e.is_dir || inline_type(&ext_of(&e.name)).is_none() {
                String::new()
            } else {
                format!(
                    r#"<a class="op" title="Download" href="{}?download=1">⇩</a>"#,
                    escape(&e.href)
                )
            },
            ops = if writable {
                r#"<button class="op" data-op="rename" title="Rename">✎</button><button class="op" data-op="move" title="Move">→</button><button class="op" data-op="delete" title="Delete (to trash)">🗑</button>"#
            } else {
                ""
            },
        );
    }

    let body = if entries.is_empty() {
        format!(
            r#"<p class="empty">{}</p>"#,
            escape(if hint.is_empty() { "Empty." } else { hint })
        )
    } else {
        format!(
            r#"<table>
<tr><th class="s" data-k="nm">name <span class="ar"></span></th>
<th class="s" data-k="kd">kind <span class="ar"></span></th>
<th>summary</th>
<th class="s n" data-k="sz">size <span class="ar"></span></th>
<th class="s n" data-k="mt">modified <span class="ar"></span></th><th class="act"></th></tr>
<tbody id="rows">{rows}</tbody></table>
<p class="empty" id="none" style="display:none">Nothing matches.</p>"#
        )
    };

    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Pipeline · library · {title}</title>
<style>{CSS}</style><script>{BOOT}</script></head><body data-rel="{relattr}">
<header>
  <span class="brand">Pipeline</span>
  <span class="crumbs">{crumbs}</span>
  <input id="q" type="search" placeholder="Filter…" autocomplete="off">
  {tools}
  <button class="tbtn" id="theme">Theme</button>
</header>
<div class="wrap">
  <nav>{nav}</nav>
  <main>
    {body}
    <footer>{note} The record on disk under <code>.pipeline/</code> — runs, reports,
    digests, sessions. Secrets and the OAuth token store are never listed or served.</footer>
  </main>
</div>
<script>{JS}</script></body></html>"#,
        title = escape(url_path),
        crumbs = crumbs(url_path),
        relattr = escape(rel.trim_matches('/')),
        tools = if writable {
            r#"<button class="tbtn" id="mkdir">New folder</button><button class="tbtn" id="up">Upload</button><input type="file" id="file" hidden>"#
        } else {
            ""
        },
        note = if writable {
            "Writable. Delete moves to <code>trash/</code> — it never unlinks, so a delete is always reversible."
        } else {
            "Read-only. Set <code>PIPELINE_LIBRARY_WRITE=1</code> to enable rename, move, delete and upload."
        },
    )
}

/// Lowercase extension of a file name, or "" — used to decide if a download link is even
/// offered.
fn ext_of(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
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

    /// A symlink out of the root must not even be LISTED. Following it was already
    /// impossible (`resolve` canonicalizes), but a row that 404s on click still tells the
    /// reader the link is there.
    #[test]
    fn a_symlink_is_not_listed_at_all() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("reports")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc", d.path().join("escape")).unwrap();
        let names: Vec<String> = listing(d.path(), "/library")
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert!(
            !names.contains(&"escape".to_owned()),
            "symlink listed: {names:?}"
        );
        assert!(
            names.contains(&"reports".to_owned()),
            "real dir must remain"
        );
    }

    /// The sidebar walks the tree independently of the listing — so it needs its own proof
    /// that it neither follows a symlink nor shows the token store.
    #[test]
    fn the_sidebar_tree_leaks_neither_the_token_store_nor_a_symlink() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("digests")).unwrap();
        std::fs::create_dir(d.path().join(".oauth")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc", d.path().join("escape")).unwrap();
        let mut out = String::new();
        tree(d.path(), "/library", "/library", TREE_DEPTH, &mut out);
        assert!(out.contains("digests"), "real dir missing from tree: {out}");
        assert!(!out.contains("oauth"), "token store in tree: {out}");
        assert!(!out.contains("escape"), "symlink in tree: {out}");
        assert!(!out.contains("passwd"), "followed a symlink: {out}");
    }

    /// A repo clone under `repos/<alias>/` must not turn one page load into a recursive
    /// walk of someone else's source tree.
    #[test]
    fn the_tree_does_not_expand_a_wide_directory() {
        let d = tempfile::tempdir().unwrap();
        let clone = d.path().join("repos");
        std::fs::create_dir(&clone).unwrap();
        for i in 0..(TREE_FANOUT + 5) {
            std::fs::create_dir(clone.join(format!("pkg{i}"))).unwrap();
        }
        let mut out = String::new();
        tree(d.path(), "/library", "/library", TREE_DEPTH, &mut out);
        assert!(
            out.contains("repos"),
            "the dir itself must still be navigable"
        );
        assert!(
            !out.contains("pkg0"),
            "wide dir must not be expanded: {out}"
        );
    }

    #[test]
    fn the_file_manager_renders_rows_and_annotates_a_failing_report() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("reports")).unwrap();
        std::fs::write(
            d.path().join("reports/run-9.json"),
            r#"{"status":"failed"}"#,
        )
        .unwrap();
        let entries = listing(&d.path().join("reports"), "/library/reports");
        let html = render(d.path(), "reports", "/library/reports", &entries, "");
        assert!(html.contains("run-9.json"));
        assert!(html.contains(r#"class="dot fail""#), "red dot missing");
        assert!(html.contains(">report<"), "kind column missing");
        // Up-one-level must exist and be exempt from the filter (data-up=1).
        assert!(html.contains(r#"<tr data-up="1""#), "no way back out");
    }

    #[test]
    fn a_hostile_filename_cannot_inject_html_into_the_manager() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("<img src=x onerror=alert(1)>.json"), "{}").unwrap();
        let entries = listing(d.path(), "/library");
        let html = render(d.path(), "", "/library", &entries, "");
        assert!(!html.contains("<img src=x"), "unescaped filename");
        assert!(html.contains("&lt;img"));
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
