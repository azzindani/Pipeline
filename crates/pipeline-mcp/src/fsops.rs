//! File-system operations for the library: mkdir · rename · move · delete · upload.
//!
//! Ported from Folio's `library-manage.ts` — the "manager" half of the file manager.
//! [`crate::browse`] lets you SEE the record; this lets you organise it.
//!
//! # Two rules, both taken from Folio
//!
//! 1. **Delete is a MOVE, never an unlink.** Folio's `deleteDesign` renames into
//!    `.trash/` and says so in its own comment: *"soft-delete … never unlink"*. A CI
//!    record is evidence. The one moment you most want a report back is right after you
//!    decided you didn't need it. So `delete` moves to `trash/<timestamp>__<name>` and
//!    restoring is just a move back out.
//! 2. **A rename cannot change where a file lives.** Only the leaf name is writable, and
//!    it is validated as a *name*, not a path — see [`resolve_new`].
//!
//! # Why these are NOT behind `PIPELINE_REMOTE_MODE=full`
//!
//! That flag unlocks `pipeline_docker` (build · run), `pipeline_deploy`, commit and push
//! — remote code execution. Renaming a report is not remotely in that class. Gating file
//! ops behind it would force anyone who wants to tidy a directory to also hand out
//! container execution, which is a strictly worse trade than the one they asked for.
//!
//! So writes get their own switch, `PIPELINE_LIBRARY_WRITE=1`, **off by default**. The
//! endpoint stays read-only until someone says otherwise, and saying otherwise grants
//! exactly this: bounded file operations inside one directory tree.
//!
//! # Containment
//!
//! Every path — source AND destination — goes through the same rules the browser uses.
//! ! `browse::resolve` canonicalizes, so it only works for paths that ALREADY exist. A
//! destination does not exist yet, which is precisely where a naive `root.join(name)`
//! would let `../../etc/cron.d/x` through. [`resolve_new`] resolves the *parent* and
//! validates the leaf as a bare name.

use crate::browse::{is_hidden_or_denied, resolve};
use std::path::{Path, PathBuf};

/// Upload ceiling. Generous for a report or a digest, bounded so one POST cannot fill
/// the volume the record lives on.
pub const MAX_UPLOAD_BYTES: usize = 32 * 1024 * 1024;

/// Where `delete` puts things. ✗ dot-prefixed: a hidden trash is a trash you cannot open,
/// and the whole point of a soft delete is that you can walk back into it and pull the
/// file out. (Folio uses `.trash` because its trash is not browsable; ours is.)
pub const TRASH: &str = "trash";

#[derive(Debug, PartialEq, Eq)]
pub enum FsError {
    Disabled,
    BadName(&'static str),
    NotFound,
    Exists,
    Io(String),
}

impl FsError {
    pub fn message(&self) -> String {
        match self {
            Self::Disabled => {
                "Library is read-only. Set PIPELINE_LIBRARY_WRITE=1 to enable file operations."
                    .to_owned()
            }
            Self::BadName(why) => (*why).to_owned(),
            Self::NotFound => "Not found.".to_owned(),
            Self::Exists => "A file with that name already exists.".to_owned(),
            Self::Io(e) => format!("Failed: {e}"),
        }
    }
    pub const fn status(&self) -> u16 {
        match self {
            Self::Disabled => 403,
            Self::BadName(_) => 400,
            Self::NotFound => 404,
            Self::Exists => 409,
            Self::Io(_) => 500,
        }
    }
}

/// Writes are off unless explicitly switched on. A record you can accidentally delete
/// over the internet is worse than one you have to SSH in to tidy.
pub fn writes_enabled() -> bool {
    std::env::var("PIPELINE_LIBRARY_WRITE").is_ok_and(|v| matches!(v.trim(), "1" | "true" | "yes"))
}

/// Proof that writes are enabled.
///
/// Every mutating op demands one, and the only way to obtain it is [`Writable::from_env`].
/// So "did we check the switch?" stops being a question you can get wrong: a new operation
/// that forgets the check does not compile. A plain `if !writes_enabled() { return }` at the
/// top of each function would have been one forgotten line away from a public delete button.
#[derive(Debug, Clone, Copy)]
pub struct Writable(());

impl Writable {
    pub fn from_env() -> Option<Self> {
        writes_enabled().then_some(Self(()))
    }
    #[cfg(test)]
    const fn granted() -> Self {
        Self(())
    }
}

/// Validate a LEAF name — a name, not a path.
///
/// ! This is the whole defence for destinations. `..`, any separator, a dotfile, or a
/// deny-listed name (`oauth`, `tokens.json`) is refused, so a rename can never relocate a
/// file, escape the root, shadow the token store, or hide itself from the listing.
pub fn valid_leaf(name: &str) -> Result<(), FsError> {
    let n = name.trim();
    if n.is_empty() {
        return Err(FsError::BadName("Name cannot be empty."));
    }
    if n.len() > 255 {
        return Err(FsError::BadName("Name is too long."));
    }
    if n.contains('/') || n.contains('\\') {
        return Err(FsError::BadName(
            "Name cannot contain a path separator — rename cannot move a file.",
        ));
    }
    if n == "." || n == ".." {
        return Err(FsError::BadName("Name cannot be `.` or `..`."));
    }
    if n.contains('\0') {
        return Err(FsError::BadName("Name contains a NUL byte."));
    }
    if is_hidden_or_denied(n) {
        return Err(FsError::BadName(
            "Name is reserved — dotfiles and the token store are not writable.",
        ));
    }
    Ok(())
}

/// Resolve a destination that does not exist yet.
///
/// `browse::resolve` canonicalizes and therefore only works on existing paths — useless
/// for a destination. Here the PARENT is resolved (so it is real, contained, and not the
/// token store) and the leaf is validated as a bare name, then joined.
pub fn resolve_new(root: &Path, parent_rel: &str, leaf: &str) -> Result<PathBuf, FsError> {
    valid_leaf(leaf)?;
    let parent = if parent_rel.trim_matches('/').is_empty() {
        root.canonicalize()
            .map_err(|e| FsError::Io(e.to_string()))?
    } else {
        resolve(root, parent_rel).ok_or(FsError::NotFound)?
    };
    if !parent.is_dir() {
        return Err(FsError::NotFound);
    }
    Ok(parent.join(leaf.trim()))
}

/// Split `a/b/c.json` → (`a/b`, `c.json`).
pub fn split_rel(rel: &str) -> (String, String) {
    let r = rel.trim_matches('/');
    r.rsplit_once('/').map_or_else(
        || (String::new(), r.to_owned()),
        |(p, l)| (p.to_owned(), l.to_owned()),
    )
}

pub fn mkdir(_w: Writable, root: &Path, parent_rel: &str, name: &str) -> Result<(), FsError> {
    let target = resolve_new(root, parent_rel, name)?;
    if target.exists() {
        return Err(FsError::Exists);
    }
    std::fs::create_dir(&target).map_err(|e| FsError::Io(e.to_string()))
}

/// Rename in place. The file does not move — only its leaf changes.
pub fn rename(_w: Writable, root: &Path, rel: &str, new_name: &str) -> Result<(), FsError> {
    let src = resolve(root, rel).ok_or(FsError::NotFound)?;
    let (parent, _) = split_rel(rel);
    let dst = resolve_new(root, &parent, new_name)?;
    if dst == src {
        return Ok(());
    }
    if dst.exists() {
        return Err(FsError::Exists);
    }
    std::fs::rename(&src, &dst).map_err(|e| FsError::Io(e.to_string()))
}

/// Move into another directory, keeping the name.
pub fn move_to(_w: Writable, root: &Path, rel: &str, dest_dir_rel: &str) -> Result<(), FsError> {
    let src = resolve(root, rel).ok_or(FsError::NotFound)?;
    let (_, leaf) = split_rel(rel);
    let dst = resolve_new(root, dest_dir_rel, &leaf)?;
    if dst == src {
        return Ok(());
    }
    if dst.exists() {
        return Err(FsError::Exists);
    }
    // ! Refuse to move a directory inside itself — `mv a a/b` orphans the subtree.
    if src.is_dir() && dst.starts_with(&src) {
        return Err(FsError::BadName("Cannot move a directory into itself."));
    }
    std::fs::rename(&src, &dst).map_err(|e| FsError::Io(e.to_string()))
}

/// Soft delete → `trash/<stamp>__<name>`. ! Never unlinks.
///
/// Returns the trash path, so the caller can tell the user where it went — a delete you
/// cannot find afterwards is indistinguishable from one that destroyed the file.
pub fn delete(_w: Writable, root: &Path, rel: &str, now_stamp: &str) -> Result<String, FsError> {
    let src = resolve(root, rel).ok_or(FsError::NotFound)?;
    let (_, leaf) = split_rel(rel);

    let canonical_root = root
        .canonicalize()
        .map_err(|e| FsError::Io(e.to_string()))?;
    let trash = canonical_root.join(TRASH);
    std::fs::create_dir_all(&trash).map_err(|e| FsError::Io(e.to_string()))?;

    // ✗ delete the trash itself, and ✗ re-trash something already in it.
    if src == trash || src.starts_with(&trash) {
        return Err(FsError::BadName(
            "That is the trash — delete from inside it is not a thing; move it out or leave it.",
        ));
    }

    let name = format!("{now_stamp}__{leaf}");
    valid_leaf(&name)?;
    let dst = trash.join(&name);
    std::fs::rename(&src, &dst).map_err(|e| FsError::Io(e.to_string()))?;
    Ok(format!("{TRASH}/{name}"))
}

pub fn upload(
    _w: Writable,
    root: &Path,
    dir_rel: &str,
    name: &str,
    bytes: &[u8],
) -> Result<(), FsError> {
    if bytes.len() > MAX_UPLOAD_BYTES {
        return Err(FsError::BadName("File is too large."));
    }
    let target = resolve_new(root, dir_rel, name)?;
    if target.exists() {
        return Err(FsError::Exists);
    }
    std::fs::write(&target, bytes).map_err(|e| FsError::Io(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// The capability, granted directly. ✗ touch process env — that is racy across parallel
    /// tests, and `unsafe` is forbidden in this crate anyway. That the switch is what mints
    /// this in production is `Writable::from_env`'s job, not each op's.
    fn w() -> Writable {
        Writable::granted()
    }

    fn fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("reports")).unwrap();
        fs::create_dir_all(d.path().join(".oauth")).unwrap();
        fs::write(d.path().join(".oauth/access-tokens.json"), "{}").unwrap();
        fs::write(d.path().join("reports/run.json"), r#"{"status":"pass"}"#).unwrap();
        d
    }

    // ── the destination is the dangerous half ───────────────────────────────────────

    /// A rename must not be able to relocate a file — that is a move, and a move that
    /// escapes the root is an arbitrary file write.
    #[test]
    fn a_rename_cannot_escape_the_root() {
        for evil in [
            "../../etc/cron.d/x",
            "..",
            "a/b",
            "..\\windows",
            "/etc/passwd",
        ] {
            assert!(
                valid_leaf(evil).is_err(),
                "leaf validation let through: {evil}"
            );
        }
    }

    /// ✗ shadow the token store, ✗ create something the listing would then hide.
    #[test]
    fn a_name_cannot_shadow_the_token_store_or_hide_itself() {
        for reserved in [".oauth", "oauth", "tokens.json", ".hidden", "oauth-state"] {
            assert!(
                valid_leaf(reserved).is_err(),
                "reserved name allowed: {reserved}"
            );
        }
    }

    #[test]
    fn resolve_new_refuses_a_parent_outside_the_root() {
        let d = fixture();
        assert!(resolve_new(d.path(), "../..", "x.json").is_err());
        assert!(resolve_new(d.path(), ".oauth", "x.json").is_err());
        assert!(resolve_new(d.path(), "reports", "ok.json").is_ok());
    }

    // ── delete is a move, never an unlink ───────────────────────────────────────────

    #[test]
    fn delete_moves_to_trash_and_the_bytes_survive() {
        let d = fixture();
        let where_to = delete(w(), d.path(), "reports/run.json", "2026-07-13T12-00-00").unwrap();
        assert!(
            !d.path().join("reports/run.json").exists(),
            "must leave the original location"
        );
        let landed = d.path().join(&where_to);
        assert!(landed.exists(), "file vanished — delete unlinked it!");
        assert_eq!(
            fs::read_to_string(&landed).unwrap(),
            r#"{"status":"pass"}"#,
            "bytes must survive a delete"
        );
        assert!(where_to.starts_with("trash/"));
    }

    /// Restore = move it back out. The whole reason delete is a move.
    #[test]
    fn a_deleted_file_can_be_restored() {
        let d = fixture();
        let where_to = delete(w(), d.path(), "reports/run.json", "2026-07-13T12-00-00").unwrap();
        move_to(w(), d.path(), &where_to, "reports").unwrap();
        let (_, leaf) = split_rel(&where_to);
        assert!(
            d.path().join("reports").join(&leaf).exists(),
            "restore failed"
        );
    }

    #[test]
    fn the_token_store_cannot_be_deleted_or_renamed() {
        let d = fixture();
        assert_eq!(
            delete(w(), d.path(), ".oauth", "ts").unwrap_err(),
            FsError::NotFound
        );
        assert_eq!(
            delete(w(), d.path(), ".oauth/access-tokens.json", "ts").unwrap_err(),
            FsError::NotFound
        );
        assert!(rename(w(), d.path(), ".oauth", "x").is_err());
        assert!(
            d.path().join(".oauth/access-tokens.json").exists(),
            "token store was touched"
        );
    }

    // ── ordinary operations ─────────────────────────────────────────────────────────

    #[test]
    fn rename_keeps_the_file_in_place() {
        let d = fixture();
        rename(w(), d.path(), "reports/run.json", "renamed.json").unwrap();
        assert!(d.path().join("reports/renamed.json").exists());
        assert!(!d.path().join("reports/run.json").exists());
    }

    #[test]
    fn rename_refuses_to_clobber_an_existing_file() {
        let d = fixture();
        fs::write(d.path().join("reports/taken.json"), "x").unwrap();
        assert_eq!(
            rename(w(), d.path(), "reports/run.json", "taken.json").unwrap_err(),
            FsError::Exists,
            "a rename must never silently overwrite"
        );
        assert_eq!(
            fs::read_to_string(d.path().join("reports/taken.json")).unwrap(),
            "x"
        );
    }

    #[test]
    fn mkdir_then_move_into_it() {
        let d = fixture();
        mkdir(w(), d.path(), "", "archive").unwrap();
        move_to(w(), d.path(), "reports/run.json", "archive").unwrap();
        assert!(d.path().join("archive/run.json").exists());
    }

    #[test]
    fn a_directory_cannot_be_moved_into_itself() {
        let d = fixture();
        mkdir(w(), d.path(), "reports", "sub").unwrap();
        assert!(
            move_to(w(), d.path(), "reports", "reports/sub").is_err(),
            "moving a dir into its own child orphans the subtree"
        );
    }

    #[test]
    fn upload_is_capped_and_will_not_clobber() {
        let d = fixture();
        upload(w(), d.path(), "reports", "new.json", b"{}").unwrap();
        assert!(d.path().join("reports/new.json").exists());
        assert_eq!(
            upload(w(), d.path(), "reports", "new.json", b"{}").unwrap_err(),
            FsError::Exists
        );
        let huge = vec![0u8; MAX_UPLOAD_BYTES + 1];
        assert!(upload(w(), d.path(), "reports", "big.json", &huge).is_err());
    }

    #[test]
    fn split_rel_splits_parent_from_leaf() {
        assert_eq!(split_rel("a/b/c.json"), ("a/b".into(), "c.json".into()));
        assert_eq!(split_rel("c.json"), (String::new(), "c.json".into()));
        assert_eq!(split_rel("/a/b/"), ("a".into(), "b".into()));
    }
}
