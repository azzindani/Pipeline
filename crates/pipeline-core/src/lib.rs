//! Pipeline stage runner · result types · context.
//!
//! Defines the `Stage` trait every executor implements (static · unit · container ·
//! integration · security) and the result envelope every super tool returns.
//! Implementations land in `pipeline-stages`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageKind {
    Static,
    Unit,
    Container,
    Integration,
    Security,
}

impl StageKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Unit => "unit",
            Self::Container => "container",
            Self::Integration => "integration",
            Self::Security => "security",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageProfile {
    /// Static + unit · agent inner loop · seconds.
    Fast,
    /// Static + unit + container + integration · pre-commit.
    Full,
    /// Full + security · pre-push · clean Docker.
    Preflight,
    /// Static + unit · GitHub Actions confirmation.
    Confirm,
}

impl StageProfile {
    pub const fn stages(self) -> &'static [StageKind] {
        match self {
            Self::Fast | Self::Confirm => &[StageKind::Static, StageKind::Unit],
            Self::Full => &[
                StageKind::Static,
                StageKind::Unit,
                StageKind::Container,
                StageKind::Integration,
            ],
            Self::Preflight => &[
                StageKind::Static,
                StageKind::Unit,
                StageKind::Container,
                StageKind::Integration,
                StageKind::Security,
            ],
        }
    }

    /// Whether a skipped stage fails the run.
    ///
    /// ! `Preflight` is the documented pre-push gate — "all green → push
    /// allowed". That promise only holds if every stage in the profile
    /// genuinely executed, so a skip is a failure there. `Fast`/`Full`/
    /// `Confirm` are inner-loop profiles: they tolerate a skip (no Dockerfile,
    /// no daemon) but the runner still surfaces it in the summary.
    pub const fn is_strict(self) -> bool {
        matches!(self, Self::Preflight)
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "fast" => Some(Self::Fast),
            "full" => Some(Self::Full),
            "preflight" => Some(Self::Preflight),
            "confirm" => Some(Self::Confirm),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    Pass,
    Fail,
    Skipped,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageResult {
    pub stage: StageKind,
    pub status: StageStatus,
    #[serde(with = "serde_duration_millis")]
    pub duration: Duration,
    pub stdout: String,
    pub stderr: String,
    #[serde(default)]
    pub failure: Option<FailureDetail>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureDetail {
    pub message: String,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub line: Option<u32>,
}

/// Shared runtime context handed to every stage executor.
#[derive(Debug, Clone)]
pub struct StageContext {
    pub project_root: PathBuf,
    pub config: pipeline_config::PipelineConfig,
}

#[async_trait]
pub trait Stage: Send + Sync {
    fn kind(&self) -> StageKind;
    async fn run(&self, ctx: &StageContext) -> Result<StageResult, StageError>;
}

#[derive(Debug, thiserror::Error)]
pub enum StageError {
    #[error("stage io: {0}")]
    Io(#[from] std::io::Error),
    #[error("stage config: {0}")]
    Config(#[from] pipeline_config::ConfigError),
    #[error("stage runner: {0}")]
    Other(String),
}

mod serde_duration_millis {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let ms = u64::deserialize(d)?;
        Ok(Duration::from_millis(ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_profile_is_static_plus_unit() {
        assert_eq!(StageProfile::Fast.stages().len(), 2);
        assert_eq!(StageProfile::Fast.stages()[0], StageKind::Static);
        assert_eq!(StageProfile::Fast.stages()[1], StageKind::Unit);
    }

    #[test]
    fn preflight_profile_includes_security() {
        let stages = StageProfile::Preflight.stages();
        assert!(stages.contains(&StageKind::Security));
    }

    #[test]
    fn only_preflight_is_strict() {
        // ! preflight is the pre-push gate · a skipped stage there means the
        // gate never checked the thing it claims to check.
        assert!(StageProfile::Preflight.is_strict());
        assert!(!StageProfile::Fast.is_strict());
        assert!(!StageProfile::Full.is_strict());
        assert!(!StageProfile::Confirm.is_strict());
    }

    #[test]
    fn parses_known_profiles() {
        assert_eq!(StageProfile::parse("fast"), Some(StageProfile::Fast));
        assert_eq!(StageProfile::parse("full"), Some(StageProfile::Full));
        assert_eq!(StageProfile::parse("nope"), None);
    }
}
