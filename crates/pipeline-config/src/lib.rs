//! `pipeline.yaml` schema · serde-driven · validated on load.
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
    fn parses_minimal_config() {
        let cfg = PipelineConfig::parse(SAMPLE).expect("parse");
        assert_eq!(cfg.project, "pipeline");
        assert_eq!(cfg.stack.runtime, "rust");
        assert_eq!(cfg.stages.fast, vec!["static", "unit"]);
        assert_eq!(cfg.gates.coverage, Some(70));
    }
}
