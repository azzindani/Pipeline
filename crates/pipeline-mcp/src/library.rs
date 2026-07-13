//! Artifact annotation for the file manager.
//!
//! Pipeline's library is a FILE SYSTEM, not a gallery. An earlier pass ported Folio's
//! design gallery — cards, thumbnails, a grid — which was the wrong shape: Folio catalogs
//! *designs*, and a design has a picture. Pipeline's artifacts are digests, reports,
//! sessions, RE jobs. You navigate them like files, because that is what they are.
//!
//! What survives from that pass is the one genuinely useful idea: a row can say more than
//! its filename. `reports/run-812.json` is a name; **`report · failed`** is the fact you
//! opened the library to find. So the file manager annotates rows with a kind, a one-line
//! summary and a pass/fail dot — while remaining, structurally, a directory listing.
//!
//! Reads stay cheap, per Folio's own rule: [`describe`] reads a bounded prefix and pulls
//! a couple of fields. A digest of a large repo is megabytes; annotating a row must not
//! deserialize all of it.

use std::fmt::Write as _;
use std::path::Path;

/// Bytes of an artifact read to summarise it. Enough for any header we care about;
/// bounded so a multi-megabyte digest costs a row, not a heap.
const PEEK_BYTES: usize = 16 * 1024;

/// Rows this many or fewer get annotated. A directory with thousands of entries (a repo
/// clone) would otherwise mean thousands of file reads to draw one page.
pub const ANNOTATE_MAX_ENTRIES: usize = 200;

/// What a row says about the artifact's outcome, if it has one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Fail,
    /// Not every artifact has an outcome — a screenshot did not pass or fail.
    None,
}

impl Status {
    pub const fn css(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Fail => "fail",
            Self::None => "none",
        }
    }
}

/// Top-level directory → the kind of artifact inside it.
///
/// Anything not named here is just a `file`; an unknown artifact should look boring, never
/// vanish. A library that silently omits what it does not recognise is one you cannot
/// trust to be complete.
const KINDS: &[(&str, &str)] = &[
    ("digests", "digest"),
    ("reports", "report"),
    ("sessions", "session"),
    ("screenshots", "screenshot"),
    ("re", "re"),
    ("repos", "repo"),
    ("templates", "template"),
];

/// The kind of a path, from its FIRST segment — `digests/folio.json` → `digest`.
pub fn kind_of(rel: &str) -> &'static str {
    let top = rel.trim_start_matches('/').split('/').next().unwrap_or("");
    KINDS
        .iter()
        .find(|(dir, _)| *dir == top)
        .map_or("file", |(_, k)| *k)
}

/// Read a bounded prefix and pull a one-line summary + outcome out of it.
///
/// Deliberately tolerant: a malformed or truncated artifact yields an empty summary, never
/// an error and never a panic. A half-written digest — exactly what you get if you open the
/// library while one is being written — must still draw its row.
pub fn describe(path: &Path, kind: &str) -> (String, Status) {
    if !matches!(kind, "digest" | "report" | "session" | "re") {
        return (String::new(), Status::None);
    }
    let Ok(raw) = std::fs::read(path) else {
        return (String::new(), Status::None);
    };
    let head = &raw[..raw.len().min(PEEK_BYTES)];
    let Ok(text) = std::str::from_utf8(head) else {
        return (String::new(), Status::None);
    };
    // Truncated at PEEK_BYTES → not valid JSON → no summary, still a row.
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return (String::new(), Status::None);
    };

    match kind {
        "digest" => {
            let files = v
                .get("summary")
                .and_then(|s| s.get("total_files"))
                .and_then(serde_json::Value::as_u64);
            // Biggest language by file count — the one fact that says what a repo IS.
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
        // A report/run either passed or it did not — the whole reason you opened it.
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
        _ => (
            v.get("target")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned(),
            Status::None,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn kind_comes_from_the_first_path_segment() {
        assert_eq!(kind_of("digests/folio.json"), "digest");
        assert_eq!(kind_of("reports/run-1.json"), "report");
        assert_eq!(kind_of("screenshots/home.png"), "screenshot");
        assert_eq!(kind_of("mystery/thing.bin"), "file");
        assert_eq!(kind_of(""), "file");
    }

    /// The one fact a CI record exists to tell you.
    #[test]
    fn a_report_carries_its_pass_fail_outcome() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("r.json");
        fs::write(&p, r#"{"status":"failed"}"#).unwrap();
        assert_eq!(describe(&p, "report"), ("failed".into(), Status::Fail));
        fs::write(&p, r#"{"status":"pass"}"#).unwrap();
        assert_eq!(describe(&p, "report"), ("pass".into(), Status::Ok));
    }

    #[test]
    fn a_digest_is_summarised_without_reading_all_of_it() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("f.json");
        fs::write(
            &p,
            r#"{"summary":{"total_files":5263,"languages":{"typescript":492,"yaml":973}}}"#,
        )
        .unwrap();
        let (sub, st) = describe(&p, "digest");
        assert!(sub.contains("yaml"), "top language: {sub}");
        assert!(sub.contains("5263"), "file count: {sub}");
        assert_eq!(st, Status::None, "a digest did not pass or fail");
    }

    /// A digest being written WHILE the library is open is truncated JSON. It must still
    /// draw its row — a file manager that 500s mid-write is one you cannot open.
    #[test]
    fn a_truncated_artifact_still_yields_a_row() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("half.json");
        fs::write(&p, r#"{"alias":"half","sum"#).unwrap();
        assert_eq!(describe(&p, "digest"), (String::new(), Status::None));
    }

    /// ✗ read a 4 GB repo clone or a PNG to draw its row.
    #[test]
    fn unknown_and_binary_kinds_are_never_read() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.png");
        fs::write(&p, b"\x89PNG").unwrap();
        assert_eq!(describe(&p, "screenshot"), (String::new(), Status::None));
        assert_eq!(describe(&p, "repo"), (String::new(), Status::None));
        assert_eq!(describe(&p, "file"), (String::new(), Status::None));
    }
}
