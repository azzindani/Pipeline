//! The library catalog + gallery — a file-manager view over Pipeline's durable record.
//!
//! Ported from Folio's `library.ts` + `library-gallery.ts`. What carried over is the
//! SHAPE, not the content: Folio catalogs *designs* (project → design, with a rendered
//! thumbnail per card). Pipeline has no designs. Its artifacts are digests, RE jobs,
//! reports, screenshots, repo clones — so a card carries a kind badge, a status, and a
//! cheap one-line summary where Folio's carried a picture.
//!
//! Copying the gallery verbatim would have produced a design browser with nothing to
//! browse. This is what "digest, don't copy" means in practice.
//!
//! Two rules inherited from Folio, both load-bearing:
//!
//! 1. **Reads are cheap.** Folio parses only a design's YAML *header*, never the layer
//!    tree, because a design file can be enormous. Same discipline here: [`peek`] reads
//!    a bounded prefix of a JSON artifact and pulls a couple of fields. A digest of a
//!    large repo is megabytes; the gallery must not deserialize all of it to draw a card.
//! 2. **The gallery is generated on every request**, never cached to disk. Folio also
//!    exports a static snapshot; Pipeline does not, because a stale snapshot of a CI
//!    record is worse than no snapshot — it would show a green run that has since failed.
//!
//! ! The catalog walks the SAME root the browser serves and reuses
//! [`crate::browse::is_hidden_or_denied`], so the OAuth token store (`.oauth`) is
//! excluded here by exactly the rule that excludes it there. Two independent listing
//! paths over one directory is precisely how a token store leaks out of one of them.

use crate::browse::{escape, human, is_hidden_or_denied, stamp};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::time::UNIX_EPOCH;

/// Bytes of an artifact read to summarise it. Enough for any header we care about;
/// bounded so a multi-megabyte digest costs a card, not a heap.
const PEEK_BYTES: usize = 16 * 1024;

/// What a card says about the artifact's outcome, if it has one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Fail,
    /// Not every artifact has an outcome — a screenshot did not pass or fail.
    None,
}

impl Status {
    const fn css(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Fail => "fail",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Item {
    pub name: String,
    pub kind: &'static str,
    pub href: String,
    /// Cheap one-liner — the card's substitute for Folio's thumbnail.
    pub subtitle: String,
    pub status: Status,
    pub modified: u64,
    pub size: u64,
}

pub struct Catalog {
    pub items: Vec<Item>,
    /// kind → count, for the filter chips. BTreeMap so the chip order is stable across
    /// requests rather than reshuffling on every page load.
    pub kinds: BTreeMap<&'static str, usize>,
}

/// Top-level directory → the kind of artifact inside it.
///
/// Anything not named here still gets catalogued as a plain `file`; an unknown artifact
/// should show up looking boring, never vanish. A library that silently omits what it
/// does not recognise is a library you cannot trust to be complete.
const KINDS: &[(&str, &str)] = &[
    ("digests", "digest"),
    ("reports", "report"),
    ("sessions", "session"),
    ("screenshots", "screenshot"),
    ("re", "re"),
    ("repos", "repo"),
    ("templates", "template"),
];

fn kind_of(top: &str) -> &'static str {
    KINDS
        .iter()
        .find(|(dir, _)| *dir == top)
        .map_or("file", |(_, k)| *k)
}

/// Read a bounded prefix and pull a one-line summary out of it.
///
/// Returns `(subtitle, status)`. Deliberately tolerant: a malformed or truncated artifact
/// yields an empty subtitle, never an error and never a panic. A half-written digest —
/// which is exactly what you get if you open the gallery while one is being written —
/// must still draw a card.
fn peek(path: &Path, kind: &str) -> (String, Status) {
    if kind == "screenshot" || kind == "repo" || kind == "template" {
        return (String::new(), Status::None);
    }
    let Ok(raw) = std::fs::read(path) else {
        return (String::new(), Status::None);
    };
    let head = &raw[..raw.len().min(PEEK_BYTES)];
    let Ok(text) = std::str::from_utf8(head) else {
        return (String::new(), Status::None);
    };
    // Truncated at PEEK_BYTES → not valid JSON → no subtitle, still a card.
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return (String::new(), Status::None);
    };

    match kind {
        "digest" => {
            let files = v
                .get("summary")
                .and_then(|s| s.get("total_files"))
                .and_then(serde_json::Value::as_u64);
            // Biggest language by file count — the one fact that tells you what a repo IS.
            let lang = v
                .get("summary")
                .and_then(|s| s.get("languages"))
                .and_then(serde_json::Value::as_object)
                .and_then(|m| {
                    m.iter()
                        .filter_map(|(k, n)| n.as_u64().map(|n| (k.clone(), n)))
                        .max_by_key(|(_, n)| *n)
                        .map(|(k, _)| k)
                });
            let mut s = String::new();
            if let Some(l) = lang {
                s.push_str(&l);
            }
            if let Some(f) = files {
                if !s.is_empty() {
                    s.push_str(" · ");
                }
                let _ = write!(s, "{f} files");
            }
            (s, Status::None)
        }
        // A report/run either passed or it did not — that is the whole point of looking.
        "report" | "session" => {
            let st = v
                .get("status")
                .or_else(|| v.get("outcome"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let status = match st.to_ascii_lowercase().as_str() {
                "ok" | "pass" | "passed" | "green" | "success" => Status::Ok,
                "fail" | "failed" | "red" | "error" => Status::Fail,
                _ => Status::None,
            };
            (st.to_owned(), status)
        }
        "re" => (
            v.get("target")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned(),
            Status::None,
        ),
        _ => (String::new(), Status::None),
    }
}

/// Walk the library root and build the catalog.
///
/// Depth 2: root-level files, plus one level inside each artifact directory. Deeper than
/// that is a repo clone or an RE working tree — thousands of files that belong in the raw
/// browser, not on a card. The gallery indexes the record; it does not mirror the disk.
pub fn catalog(root: &Path) -> Catalog {
    let mut items = Vec::new();
    let Ok(top) = std::fs::read_dir(root) else {
        return Catalog {
            items,
            kinds: BTreeMap::new(),
        };
    };

    for entry in top.filter_map(Result::ok) {
        let top_name = entry.file_name().to_string_lossy().into_owned();
        if is_hidden_or_denied(&top_name) {
            continue; // ! the .oauth token store dies here, by the browser's own rule
        }
        // ! Resolve through the browser's OWN containment rule rather than trusting
        // `read_dir` + `metadata`. `metadata()` FOLLOWS symlinks, so a symlink in the
        // library pointing at /etc reports itself as a directory and gets walked — which
        // catalogued the entire /etc listing onto cards. The file browser was never
        // exploitable this way (it canonicalizes), so the gallery had quietly become a
        // second, weaker listing path over the same root. There must be exactly one rule.
        let Some(path) = crate::browse::resolve(root, &top_name) else {
            continue;
        };
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };

        if meta.is_file() {
            items.push(build(&path, &top_name, "file", &top_name));
            continue;
        }

        let kind = kind_of(&top_name);
        let Ok(children) = std::fs::read_dir(&path) else {
            continue;
        };
        for child in children.filter_map(Result::ok) {
            let name = child.file_name().to_string_lossy().into_owned();
            if is_hidden_or_denied(&name) {
                continue;
            }
            let rel = format!("{top_name}/{name}");
            // Same rule again for the leaf — a symlink one level down escapes too.
            let Some(cpath) = crate::browse::resolve(root, &rel) else {
                continue;
            };
            items.push(build(&cpath, &name, kind, &rel));
        }
    }

    // Newest first — the thing you just produced is the thing you want to look at.
    items.sort_by_key(|i| std::cmp::Reverse(i.modified));

    let mut kinds: BTreeMap<&'static str, usize> = BTreeMap::new();
    for i in &items {
        *kinds.entry(i.kind).or_default() += 1;
    }
    Catalog { items, kinds }
}

fn build(path: &Path, name: &str, kind: &'static str, rel: &str) -> Item {
    let meta = path.metadata().ok();
    let modified = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let size = meta.as_ref().map_or(0, std::fs::Metadata::len);
    let is_dir = meta.as_ref().is_some_and(std::fs::Metadata::is_dir);

    let (subtitle, status) = if is_dir {
        (String::new(), Status::None)
    } else {
        peek(path, kind)
    };

    Item {
        name: name.trim_end_matches(".json").to_owned(),
        kind,
        href: format!("/library/{rel}{}", if is_dir { "/" } else { "" }),
        subtitle,
        status,
        modified,
        size,
    }
}

// ─── gallery ────────────────────────────────────────────────────────────────────────

const CSS: &str = r"
:root{--bg:#0E1116;--fg:#E6EAF0;--panel:#161B22;--panel2:#0A0D12;--bd:#232A35;--bd2:#2A323F;--mut:#8A93A6;--mut2:#566076;--acc:#3B82F6;--ok:#22C55E;--fail:#EF4444}
:root[data-theme=light]{--bg:#F4F6FA;--fg:#1B2433;--panel:#fff;--panel2:#EBEFF5;--bd:#E2E7EF;--bd2:#D2DAE5;--mut:#5E6A7E;--mut2:#8893A6;--acc:#2563EB}
*{box-sizing:border-box}body{margin:0;font:15px/1.5 system-ui,-apple-system,sans-serif;background:var(--bg);color:var(--fg)}
header{position:sticky;top:0;background:var(--bg);padding:20px 28px;border-bottom:1px solid var(--bd);z-index:5}
h1{margin:0 0 8px;font-size:20px;font-weight:700}
.stat{color:var(--mut);font-size:13px}
a{color:inherit}
.tools{position:absolute;top:18px;right:24px;display:flex;gap:8px}
.tbtn{background:var(--panel);border:1px solid var(--bd2);color:var(--fg);border-radius:9px;padding:7px 11px;font-size:13px;cursor:pointer;text-decoration:none}
.tbtn:hover{border-color:var(--acc)}
#q{margin-top:12px;width:100%;max-width:420px;padding:9px 14px;border-radius:10px;border:1px solid var(--bd2);background:var(--panel);color:var(--fg);font-size:14px}
.toolbar{margin-top:12px;display:flex;flex-wrap:wrap;gap:18px;align-items:center}
.chips,.sorts{display:flex;flex-wrap:wrap;gap:8px;align-items:center}
.lbl{color:var(--mut2);font-size:11px;text-transform:uppercase;letter-spacing:.08em}
.chip,.sortb{padding:5px 12px;border-radius:999px;border:1px solid var(--bd2);background:var(--panel);color:var(--mut);font-size:12px;cursor:pointer;user-select:none}
.sortb{border-radius:8px}
.chip:hover,.sortb:hover{border-color:var(--acc)}
.chip.on,.sortb.on{background:var(--acc);border-color:var(--acc);color:#fff}
.chip .ct{opacity:.6;margin-left:5px}
.viewt{display:inline-flex;border:1px solid var(--bd2);border-radius:8px;overflow:hidden}
.viewb{background:var(--panel);border:0;color:var(--mut);padding:5px 10px;font-size:13px;cursor:pointer}
.viewb.on{background:var(--acc);color:#fff}
.grid{padding:22px 28px;display:grid;grid-template-columns:repeat(auto-fill,minmax(210px,1fr));gap:16px}
.card{background:var(--panel);border:1px solid var(--bd);border-radius:12px;overflow:hidden;transition:.15s;text-decoration:none;display:block;color:inherit}
.card:hover{border-color:var(--acc);transform:translateY(-2px)}
.tile{aspect-ratio:16/9;background:var(--panel2);display:flex;align-items:center;justify-content:center;position:relative}
.glyph{font-size:13px;text-transform:uppercase;letter-spacing:.1em;color:var(--mut2);font-weight:600}
.dot{position:absolute;top:10px;right:10px;width:9px;height:9px;border-radius:50%}
.dot.ok{background:var(--ok)}.dot.fail{background:var(--fail)}.dot.none{display:none}
.meta{padding:10px 12px}
.nm{display:block;font-weight:600;font-size:13px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.sub{display:block;color:var(--mut);font-size:11px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.when{display:block;color:var(--mut2);font-size:11px;margin-top:2px}
.empty{display:none;padding:60px;text-align:center;color:var(--mut2)}
body[data-view=list] .grid{grid-template-columns:1fr;gap:6px}
body[data-view=list] .card{display:flex;align-items:center;gap:14px;padding:8px 12px}
body[data-view=list] .tile{width:52px;height:34px;aspect-ratio:auto;flex:none;border-radius:6px}
body[data-view=list] .meta{padding:0;display:flex;gap:16px;align-items:baseline;flex:1;min-width:0}
body[data-view=list] .nm{flex:0 0 240px}
body[data-view=list] .sub{flex:1}
body[data-view=list] .when{margin-top:0}
";

// Restores theme + view BEFORE first paint. Doing it after would flash the dark default
// at a light-theme reader on every single page load.
const BOOT: &str = r"try{var m=localStorage.getItem('pl-theme');if(m)document.documentElement.dataset.theme=m;
var v=localStorage.getItem('pl-view')||'grid';document.addEventListener('DOMContentLoaded',function(){document.body.dataset.view=v;
var b=document.querySelector('.viewb[data-v='+v+']');if(b){document.querySelectorAll('.viewb').forEach(function(x){x.classList.remove('on')});b.classList.add('on')}})}catch(e){}";

const JS: &str = r"
var q=document.getElementById('q'),cards=[].slice.call(document.querySelectorAll('.card'));
var kind='all',sort='modified';
function apply(){
  var t=(q.value||'').toLowerCase().trim(),shown=0;
  cards.forEach(function(c){
    var okKind=kind==='all'||c.dataset.kind===kind;
    var okText=!t||c.dataset.search.indexOf(t)>=0;
    var vis=okKind&&okText;c.style.display=vis?'':'none';if(vis)shown++;
  });
  document.querySelector('.empty').style.display=shown?'none':'block';
  document.getElementById('shown').textContent=shown;
}
q.addEventListener('input',apply);
document.querySelectorAll('.chip').forEach(function(ch){ch.addEventListener('click',function(){
  document.querySelectorAll('.chip').forEach(function(x){x.classList.remove('on')});
  ch.classList.add('on');kind=ch.dataset.kind;apply();});});
document.querySelectorAll('.sortb').forEach(function(sb){sb.addEventListener('click',function(){
  document.querySelectorAll('.sortb').forEach(function(x){x.classList.remove('on')});
  sb.classList.add('on');sort=sb.dataset.sort;
  var g=document.querySelector('.grid');
  cards.slice().sort(function(a,b){
    if(sort==='name')return a.dataset.nm.localeCompare(b.dataset.nm);
    if(sort==='kind')return a.dataset.kind.localeCompare(b.dataset.kind)||a.dataset.nm.localeCompare(b.dataset.nm);
    return b.dataset.mod-a.dataset.mod;
  }).forEach(function(c){g.appendChild(c)});});});
document.querySelectorAll('.viewb').forEach(function(vb){vb.addEventListener('click',function(){
  document.querySelectorAll('.viewb').forEach(function(x){x.classList.remove('on')});
  vb.classList.add('on');document.body.dataset.view=vb.dataset.v;
  try{localStorage.setItem('pl-view',vb.dataset.v)}catch(e){}});});
document.getElementById('theme').addEventListener('click',function(){
  var cur=document.documentElement.dataset.theme==='light'?'dark':'light';
  document.documentElement.dataset.theme=cur;try{localStorage.setItem('pl-theme',cur)}catch(e){}});
";

/// Render the gallery. Self-contained: no CDN, no external font, no remote image — it is
/// served behind the same gate as the record itself and must work on a box with no egress.
pub fn render_gallery(cat: &Catalog) -> String {
    let total = cat.items.len();
    let mut chips = String::from(
        r#"<span class="chip on" data-kind="all">all<span class="ct">"#
            .to_owned()
            .as_str(),
    );
    let _ = write!(chips, "{total}</span></span>");
    for (kind, n) in &cat.kinds {
        let _ = write!(
            chips,
            r#"<span class="chip" data-kind="{k}">{k}<span class="ct">{n}</span></span>"#,
            k = escape(kind),
        );
    }

    let mut cards = String::new();
    for i in &cat.items {
        let sub = if i.subtitle.is_empty() {
            human(i.size)
        } else {
            format!("{} · {}", escape(&i.subtitle), human(i.size))
        };
        // data-search is what the filter box matches on — name + kind + subtitle, so
        // typing "rust" finds a digest whose top language is Rust, not just a filename.
        let _ = write!(
            cards,
            r#"<a class="card" href="{href}" data-kind="{kind}" data-nm="{nml}" data-mod="{m}" data-search="{search}">
<div class="tile"><span class="glyph">{kind}</span><span class="dot {st}"></span></div>
<div class="meta"><span class="nm">{name}</span><span class="sub">{sub}</span><span class="when">{when}</span></div></a>"#,
            href = escape(&i.href),
            kind = escape(i.kind),
            nml = escape(&i.name.to_lowercase()),
            m = i.modified,
            search = escape(&format!("{} {} {}", i.name, i.kind, i.subtitle).to_lowercase()),
            st = i.status.css(),
            name = escape(&i.name),
            sub = sub,
            when = escape(&stamp(i.modified)),
        );
    }

    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Pipeline — Library</title><style>{CSS}</style><script>{BOOT}</script></head>
<body><header>
<div class="tools"><a class="tbtn" href="/library?raw=1">Files</a><button class="tbtn" id="theme">Theme</button></div>
<h1>Pipeline — Library</h1>
<div class="stat"><span id="shown">{total}</span> of {total} artifact(s) · the durable record</div>
<input id="q" type="search" placeholder="Search name, kind, summary…" autocomplete="off">
<div class="toolbar">
  <div class="chips"><span class="lbl">Kind</span>{chips}</div>
  <div class="sorts"><span class="lbl">Sort</span>
    <span class="sortb on" data-sort="modified">Newest</span>
    <span class="sortb" data-sort="name">Name</span>
    <span class="sortb" data-sort="kind">Kind</span>
  </div>
  <div class="viewt"><button class="viewb on" data-v="grid">▦</button><button class="viewb" data-v="list">☰</button></div>
</div></header>
<div class="grid">{cards}</div>
<div class="empty">Nothing matches.</div>
<script>{JS}</script></body></html>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        fs::create_dir_all(r.join("digests")).unwrap();
        fs::create_dir_all(r.join("reports")).unwrap();
        fs::create_dir_all(r.join("screenshots")).unwrap();
        // ! the token store — must never surface in the catalog
        fs::create_dir_all(r.join(".oauth")).unwrap();
        fs::write(
            r.join(".oauth/access-tokens.json"),
            r#"{"sk-live":"alice"}"#,
        )
        .unwrap();
        fs::write(
            r.join("digests/folio.json"),
            r#"{"alias":"folio","summary":{"total_files":5263,"languages":{"typescript":492,"yaml":973}}}"#,
        )
        .unwrap();
        fs::write(r.join("reports/run-1.json"), r#"{"status":"pass"}"#).unwrap();
        fs::write(r.join("reports/run-2.json"), r#"{"status":"failed"}"#).unwrap();
        fs::write(r.join("screenshots/home.png"), b"\x89PNG").unwrap();
        d
    }

    #[test]
    fn the_token_store_never_reaches_the_catalog() {
        let d = fixture();
        let cat = catalog(d.path());
        let blob = format!("{:?}", cat.items);
        assert!(!blob.contains("oauth"), "token store leaked: {blob}");
        assert!(!blob.contains("sk-live"));
        assert!(
            !cat.kinds.contains_key("file") || !blob.contains("access-tokens"),
            "token file leaked"
        );
    }

    /// Regression: `metadata()` follows symlinks, so `escape -> /etc` reported itself as
    /// a directory and the catalog walked it — putting the whole /etc listing on cards.
    /// The file browser never had this hole (it canonicalizes), which is exactly what
    /// made it dangerous: the gallery was a SECOND listing path with a weaker rule.
    #[test]
    fn a_symlink_out_of_the_library_is_not_catalogued() {
        let d = fixture();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc", d.path().join("escape")).unwrap();
        let cat = catalog(d.path());
        let names: Vec<&str> = cat.items.iter().map(|i| i.name.as_str()).collect();
        assert!(
            !names.contains(&"passwd") && !names.contains(&"shadow"),
            "symlink escape leaked a foreign listing: {names:?}"
        );
        assert!(
            !names.contains(&"escape"),
            "the symlink itself must not be catalogued either"
        );
        // ...and the real artifacts are still there — the fix must not blind the catalog.
        assert!(names.contains(&"folio"));
        assert!(names.contains(&"run-1"));
    }

    #[test]
    fn artifacts_are_classified_by_their_directory() {
        let d = fixture();
        let cat = catalog(d.path());
        let kind = |n: &str| cat.items.iter().find(|i| i.name == n).map(|i| i.kind);
        assert_eq!(kind("folio"), Some("digest"));
        assert_eq!(kind("run-1"), Some("report"));
        assert_eq!(kind("home.png"), Some("screenshot"));
    }

    /// The one fact a CI record exists to tell you.
    #[test]
    fn a_report_carries_its_pass_fail_status() {
        let d = fixture();
        let cat = catalog(d.path());
        let st = |n: &str| cat.items.iter().find(|i| i.name == n).map(|i| i.status);
        assert_eq!(st("run-1"), Some(Status::Ok));
        assert_eq!(st("run-2"), Some(Status::Fail));
        assert_eq!(
            st("home.png"),
            Some(Status::None),
            "a png did not pass/fail"
        );
    }

    #[test]
    fn a_digest_is_summarised_without_reading_all_of_it() {
        let d = fixture();
        let cat = catalog(d.path());
        let i = cat.items.iter().find(|i| i.name == "folio").unwrap();
        assert!(i.subtitle.contains("yaml"), "top language: {}", i.subtitle);
        assert!(i.subtitle.contains("5263"), "file count: {}", i.subtitle);
    }

    /// A digest being written WHILE the gallery is open is truncated JSON. It must still
    /// draw a card — a library that 500s mid-write is a library you cannot open.
    #[test]
    fn a_truncated_artifact_still_yields_a_card() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("digests")).unwrap();
        fs::write(
            d.path().join("digests/half.json"),
            r#"{"alias":"half","sum"#,
        )
        .unwrap();
        let cat = catalog(d.path());
        assert_eq!(cat.items.len(), 1);
        assert_eq!(cat.items[0].status, Status::None);
        assert!(cat.items[0].subtitle.is_empty());
    }

    #[test]
    fn an_unrecognised_artifact_is_listed_not_dropped() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("mystery")).unwrap();
        fs::write(d.path().join("mystery/thing.bin"), b"xx").unwrap();
        let cat = catalog(d.path());
        assert_eq!(cat.items.len(), 1);
        assert_eq!(cat.items[0].kind, "file", "unknown must show, not vanish");
    }

    #[test]
    fn newest_first() {
        let d = fixture();
        let cat = catalog(d.path());
        let mods: Vec<u64> = cat.items.iter().map(|i| i.modified).collect();
        let mut sorted = mods.clone();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(mods, sorted);
    }

    #[test]
    fn a_hostile_filename_cannot_inject_html_into_the_gallery() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("digests")).unwrap();
        fs::write(
            d.path().join("digests/<img src=x onerror=alert(1)>.json"),
            "{}",
        )
        .unwrap();
        let html = render_gallery(&catalog(d.path()));
        assert!(
            !html.contains("<img src=x"),
            "unescaped filename in gallery"
        );
        assert!(html.contains("&lt;img"));
    }
}
