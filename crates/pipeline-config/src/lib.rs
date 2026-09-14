//! `pipeline.yaml` schema · serde-driven · validated on load.
//!
//! kind: part
//!
//! See `CLAUDE.md` §"pipeline.yaml schema" for the canonical shape.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("read config: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse config: {0}")]
    Parse(#[from] serde_yaml::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineConfig {
    pub project: String,
    pub version: String,
    pub stack: Stack,
    #[serde(default)]
    pub stages: Stages,
    #[serde(default)]
    pub gates: Gates,
    #[serde(default)]
    pub deploy: Option<Deploy>,
    #[serde(default)]
    pub maintenance: Option<Maintenance>,
    #[serde(default)]
    pub standards: Standards,
    /// Promotion ladder. Omitted → [`Environments::default`] — dev · staging ·
    /// production with the standard gates, ✗ an empty map.
    #[serde(default)]
    pub environments: Environments,
}

/// Binding to the external Standards repo — a dependency, not a vendored copy.
///
/// `source` + `pin` give it dependency semantics: a resolvable origin and a
/// locked version. Upstream ✗ silently move a project's gates; `pin` moves only
/// on an explicit `pipeline standards update`.
///
/// `project_type` · `surfaces` are keys into the Standards repo's own routing
/// tables (ROUTER.md §5 · §6). Pipeline ✗ define routes — it selects them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Standards {
    /// Local path | git URL. Unset → resolution cascade (env → cache → clone).
    #[serde(default)]
    pub source: Option<String>,
    /// Resolved commit SHA. Written on first resolve, moved on `standards update`.
    #[serde(default)]
    pub pin: Option<String>,
    /// ROUTER.md §5 key, e.g. `MCP server`. Unset → always-on + language only.
    #[serde(default)]
    pub project_type: Option<String>,
    /// ROUTER.md §6 keys, e.g. `Command line` · `Deployed service`.
    #[serde(default)]
    pub surfaces: Vec<String>,
    /// Language routes. Empty → derived from `stack.runtime`.
    #[serde(default)]
    pub languages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stack {
    pub runtime: String,
    #[serde(default)]
    pub services: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Stages {
    #[serde(default)]
    pub fast: Vec<String>,
    #[serde(default)]
    pub full: Vec<String>,
    #[serde(default)]
    pub preflight: Vec<String>,
}

/// The promotion ladder · `dev` → `staging` → `production`.
///
/// Environment is configuration, ✗ a deploy argument. A target named at the
/// call site cannot carry entry gates, so nothing can refuse a promotion that
/// skipped a stage — which is the whole point of having a ladder.
///
/// ! Order is the promotion order. `production` is never entered from `dev`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Environments(pub Vec<Environment>);

impl Default for Environments {
    /// Every project has these three whether or not it declares them.
    fn default() -> Self {
        Self(vec![
            Environment {
                name: "dev".to_owned(),
                requires: vec!["static".to_owned(), "unit".to_owned()],
                tunnel: false,
                approval: false,
            },
            Environment {
                name: "staging".to_owned(),
                requires: vec![
                    "static".to_owned(),
                    "unit".to_owned(),
                    "container".to_owned(),
                    "integration".to_owned(),
                    "e2e".to_owned(),
                    "motion_baseline".to_owned(),
                ],
                tunnel: true,
                approval: false,
            },
            Environment {
                name: "production".to_owned(),
                requires: vec![
                    "preflight".to_owned(),
                    "security".to_owned(),
                    "motion_compare".to_owned(),
                ],
                tunnel: false,
                approval: true,
            },
        ])
    }
}

impl Environments {
    pub fn get(&self, name: &str) -> Option<&Environment> {
        self.0.iter().find(|e| e.name == name)
    }

    /// Environment immediately below `name` in the ladder · `None` for the first.
    pub fn predecessor(&self, name: &str) -> Option<&Environment> {
        let idx = self.0.iter().position(|e| e.name == name)?;
        idx.checked_sub(1).map(|i| &self.0[i])
    }

    pub fn names(&self) -> Vec<&str> {
        self.0.iter().map(|e| e.name.as_str()).collect()
    }
}

/// One rung of the ladder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Environment {
    pub name: String,
    /// Stage | check names that must have passed to enter. Empty → no gate.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Tunnel-bound for human review. ✗ true on production (§10.3).
    #[serde(default)]
    pub tunnel: bool,
    /// Entry blocks on a human decision.
    #[serde(default)]
    pub approval: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Gates {
    pub coverage: Option<u8>,
    pub image_size_mb: Option<u64>,
    pub critical_vulns: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deploy {
    pub registry: String,
    #[serde(default)]
    pub targets: BTreeMap<String, DeployTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployTarget {
    #[serde(rename = "type")]
    pub kind: String,
    pub host: String,
    #[serde(default)]
    pub requires: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Maintenance {
    pub schedule: Option<String>,
    pub auto_merge: Option<bool>,
    pub notify_on_fail: Option<bool>,
}

impl PipelineConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path.as_ref())?;
        Ok(serde_yaml::from_str(&text)?)
    }

    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        Ok(serde_yaml::from_str(text)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r"
project: pipeline
version: 0.0.1
stack:
  runtime: rust
  services: []
stages:
  fast: [static, unit]
  full: [static, unit, container, integration]
gates:
  coverage: 70
";

    #[test]
    fn ladder_defaults_when_unstated() {
        // ! A project that says nothing about environments still has all three.
        // An empty map here would mean "no gates", which is the opposite of the
        // intent and would let anything promote straight to production.
        let cfg = PipelineConfig::parse(SAMPLE).expect("parse");
        assert_eq!(
            cfg.environments.names(),
            vec!["dev", "staging", "production"]
        );
        assert!(cfg.environments.get("production").expect("prod").approval);
        assert!(!cfg.environments.get("production").expect("prod").tunnel);
        assert!(cfg.environments.get("staging").expect("staging").tunnel);
    }

    #[test]
    fn ladder_order_is_promotion_order() {
        let cfg = PipelineConfig::parse(SAMPLE).expect("parse");
        let envs = &cfg.environments;
        assert_eq!(
            envs.predecessor("production").map(|e| e.name.as_str()),
            Some("staging")
        );
        assert_eq!(
            envs.predecessor("staging").map(|e| e.name.as_str()),
            Some("dev")
        );
        assert!(envs.predecessor("dev").is_none(), "dev is the first rung");
    }

    #[test]
    fn declared_ladder_replaces_the_default() {
        let text = format!(
            "{SAMPLE}
environments:
  - name: dev
    requires: [static]
  - name: prod
    requires: [preflight]
    approval: true
"
        );
        let cfg = PipelineConfig::parse(&text).expect("parse");
        assert_eq!(cfg.environments.names(), vec!["dev", "prod"]);
        assert!(cfg.environments.get("prod").expect("prod").approval);
        assert!(cfg.environments.get("staging").is_none());
    }

    #[test]
    fn parses_minimal_config() {
        let cfg = PipelineConfig::parse(SAMPLE).expect("parse");
        assert_eq!(cfg.project, "pipeline");
        assert_eq!(cfg.stack.runtime, "rust");
        assert_eq!(cfg.stages.fast, vec!["static", "unit"]);
        assert_eq!(cfg.gates.coverage, Some(70));
    }
}
