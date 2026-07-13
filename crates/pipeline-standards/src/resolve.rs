//! Resolution cascade + version pin.
//!
//! Standards is a separate repo → treat it as a dependency: a resolvable origin
//! and a locked version. Not a monorepo path, not a vendored copy.
//!
//! ```text
//! 1. pipeline.yaml  standards.source   → local path | git URL
//! 2. $PIPELINE_STANDARDS_DIR
//! 3. ~/.pipeline/standards             → shared cache, all projects
//! 4. git clone from DEFAULT_URL        → populates the cache
//! ```
//!
//! ! Ownership rule: a source the *user* supplied (1 · 2) is READ-ONLY. Pipeline
//! reports its SHA and whether it matches the pin, but ✗ fetch · ✗ checkout —
//! that would detach the working clone under the user's feet. Only the cache
//! (3 · 4) is Pipeline-owned and writable.

use std::path::{Path, PathBuf};
use tokio::process::Command;

use crate::StandardsError;

pub const DEFAULT_URL: &str = "https://github.com/azzindani/Standards.git";
pub const ENV_DIR: &str = "PIPELINE_STANDARDS_DIR";

/// Where a resolved corpus came from — decides whether Pipeline may write to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// `standards.source` in pipeline.yaml. User-owned → read-only.
    Config,
    /// `$PIPELINE_STANDARDS_DIR`. User-owned → read-only.
    Env,
    /// `~/.pipeline/standards`. Pipeline-owned → writable.
    Cache,
}

impl Origin {
    /// May Pipeline fetch/checkout in this directory?
    pub fn is_pipeline_owned(&self) -> bool {
        matches!(self, Self::Cache)
    }
}

#[derive(Debug, Clone)]
pub struct Resolved {
    pub root: PathBuf,
    pub origin: Origin,
    /// HEAD commit of the resolved corpus.
    pub sha: String,
    /// `standards.pin` from pipeline.yaml, if set.
    pub pin: Option<String>,
}

impl Resolved {
    /// Pin set and HEAD has drifted from it → gates may have moved.
    pub fn is_drifted(&self) -> bool {
        self.pin.as_ref().is_some_and(|p| !sha_eq(p, &self.sha))
    }

    /// No pin recorded yet → first resolve, caller should write one.
    pub fn is_unpinned(&self) -> bool {
        self.pin.is_none()
    }

    pub fn short_sha(&self) -> &str {
        &self.sha[..self.sha.len().min(7)]
    }
}

/// Compare a (possibly abbreviated) pin against a full SHA.
fn sha_eq(pin: &str, sha: &str) -> bool {
    let n = pin.len().min(sha.len());
    n >= 7 && pin[..n].eq_ignore_ascii_case(&sha[..n])
}

pub fn cache_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(".pipeline").join("standards")
}

fn looks_like_url(s: &str) -> bool {
    s.starts_with("http://")
        || s.starts_with("https://")
        || s.starts_with("git@")
        || s.starts_with("ssh://")
}

/// Run the cascade. `allow_clone = false` → never touch the network (offline).
pub async fn resolve(
    cfg: &pipeline_config::Standards,
    allow_clone: bool,
) -> Result<Resolved, StandardsError> {
    let (root, origin) = locate(cfg, allow_clone).await?;

    if !root.join(crate::index::INDEX_FILE).exists() {
        return Err(StandardsError::NotAStandardsRepo {
            path: root.display().to_string(),
        });
    }

    let sha = head_sha(&root).await?;
    Ok(Resolved {
        root,
        origin,
        sha,
        pin: cfg.pin.clone(),
    })
}

async fn locate(
    cfg: &pipeline_config::Standards,
    allow_clone: bool,
) -> Result<(PathBuf, Origin), StandardsError> {
    // 1. Explicit source.
    if let Some(src) = cfg.source.as_deref().filter(|s| !s.trim().is_empty()) {
        if looks_like_url(src) {
            let cache = cache_dir();
            ensure_cache(&cache, src, allow_clone).await?;
            return Ok((cache, Origin::Cache));
        }
        let path = PathBuf::from(src);
        if !path.is_dir() {
            return Err(StandardsError::SourceNotFound {
                path: path.display().to_string(),
            });
        }
        return Ok((path, Origin::Config));
    }

    // 2. Env override.
    if let Some(dir) = std::env::var_os(ENV_DIR) {
        let path = PathBuf::from(dir);
        if !path.is_dir() {
            return Err(StandardsError::SourceNotFound {
                path: path.display().to_string(),
            });
        }
        return Ok((path, Origin::Env));
    }

    // 3 + 4. Shared cache, cloning if absent.
    let cache = cache_dir();
    ensure_cache(&cache, DEFAULT_URL, allow_clone).await?;
    Ok((cache, Origin::Cache))
}

/// Cache must exist. Absent → clone (if permitted).
async fn ensure_cache(cache: &Path, url: &str, allow_clone: bool) -> Result<(), StandardsError> {
    if cache.join(".git").is_dir() {
        return Ok(());
    }
    if !allow_clone {
        return Err(StandardsError::CacheMissing {
            path: cache.display().to_string(),
        });
    }
    if let Some(parent) = cache.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    git(
        std::env::current_dir()?.as_path(),
        &["clone", "--depth", "1", url, &cache.display().to_string()],
    )
    .await?;
    Ok(())
}

/// Move the cache to latest upstream. ! Cache-only — refuses a user-owned source.
pub async fn update(resolved: &Resolved) -> Result<String, StandardsError> {
    if !resolved.origin.is_pipeline_owned() {
        return Err(StandardsError::SourceReadOnly {
            path: resolved.root.display().to_string(),
        });
    }
    git(&resolved.root, &["fetch", "--depth", "1", "origin"]).await?;
    git(&resolved.root, &["reset", "--hard", "origin/HEAD"]).await?;
    head_sha(&resolved.root).await
}

pub async fn head_sha(root: &Path) -> Result<String, StandardsError> {
    let out = git(root, &["rev-parse", "HEAD"]).await?;
    Ok(out.trim().to_owned())
}

/// Commits touching any of `paths` in `pin..HEAD` — "did MY standards change?".
pub async fn changed_since(
    root: &Path,
    pin: &str,
    paths: &[String],
) -> Result<Vec<String>, StandardsError> {
    let range = format!("{pin}..HEAD");
    let mut args = vec!["log", "--oneline", "--no-decorate", &range, "--"];
    args.extend(paths.iter().map(String::as_str));

    // A pin absent from this clone (shallow/unfetched) is not fatal — report empty.
    let Ok(out) = git(root, &args).await else {
        return Ok(Vec::new());
    };
    Ok(out.lines().map(str::to_owned).collect())
}

async fn git(cwd: &Path, args: &[&str]) -> Result<String, StandardsError> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .await?;
    if !out.status.success() {
        return Err(StandardsError::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abbreviated_pin_matches_full_sha() {
        assert!(sha_eq("0828bd8", "0828bd8ffff1111222233334444555566667777"));
        assert!(sha_eq("0828BD8", "0828bd8ffff1111222233334444555566667777"));
    }

    #[test]
    fn different_pin_does_not_match() {
        assert!(!sha_eq(
            "deadbee",
            "0828bd8ffff1111222233334444555566667777"
        ));
    }

    #[test]
    fn too_short_pin_never_matches() {
        // Guard against a truncated/garbage pin silently matching everything.
        assert!(!sha_eq("082", "0828bd8ffff1111222233334444555566667777"));
        assert!(!sha_eq("", "0828bd8ffff"));
    }

    #[test]
    fn drift_detection() {
        let r = Resolved {
            root: PathBuf::from("/tmp"),
            origin: Origin::Config,
            sha: "0828bd8ffff1111222233334444555566667777".into(),
            pin: Some("0828bd8".into()),
        };
        assert!(!r.is_drifted());
        assert!(!r.is_unpinned());

        let drifted = Resolved {
            pin: Some("deadbeef".into()),
            ..r.clone()
        };
        assert!(drifted.is_drifted());

        let fresh = Resolved { pin: None, ..r };
        assert!(fresh.is_unpinned());
        assert!(!fresh.is_drifted()); // no pin → nothing to drift from
    }

    #[test]
    fn only_cache_is_writable() {
        assert!(Origin::Cache.is_pipeline_owned());
        // ! user clones must never be fetched/checked-out by Pipeline
        assert!(!Origin::Config.is_pipeline_owned());
        assert!(!Origin::Env.is_pipeline_owned());
    }

    #[test]
    fn url_detection() {
        assert!(looks_like_url("https://github.com/azzindani/Standards.git"));
        assert!(looks_like_url("git@github.com:azzindani/Standards.git"));
        assert!(!looks_like_url("/root/Standards"));
        assert!(!looks_like_url("../Standards"));
    }
}
