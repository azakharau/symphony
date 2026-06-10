use std::{collections::HashSet, path::PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{linear::LinearProjectConfig, opencode::OpenCodeRuntimeConfig};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RootConfig {
    pub server: Option<ServerConfig>,
    #[serde(default)]
    pub cleanup: CleanupConfig,
    projects: Vec<ProjectConfig>,
}

impl RootConfig {
    pub fn from_toml_str(input: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(input)?;
        config.validate()?;
        Ok(config)
    }

    pub fn projects(&self) -> &[ProjectConfig] {
        &self.projects
    }

    pub fn project(&self, id: &str) -> Option<&ProjectConfig> {
        self.projects.iter().find(|project| project.id == id)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.projects.is_empty() {
            return Err(ConfigError::Validation("projects must not be empty".into()));
        }
        self.cleanup.validate()?;

        let mut seen_ids = HashSet::new();
        for project in &self.projects {
            if project.id.trim().is_empty() {
                return Err(ConfigError::Validation(
                    "project id must not be empty".into(),
                ));
            }
            if !seen_ids.insert(project.id.as_str()) {
                return Err(ConfigError::Validation(format!(
                    "duplicate project id `{}`",
                    project.id
                )));
            }
            if project.name.trim().is_empty() {
                return Err(ConfigError::Validation(format!(
                    "project `{}` name must not be empty",
                    project.id
                )));
            }
            if project.linear.team_key.trim().is_empty() {
                return Err(ConfigError::Validation(format!(
                    "project `{}` linear.team_key must not be empty",
                    project.id
                )));
            }
            if project.opencode.agent.trim().is_empty() {
                return Err(ConfigError::Validation(format!(
                    "project `{}` opencode.agent must not be empty",
                    project.id
                )));
            }
            if project.concurrency.max_sessions == 0 {
                return Err(ConfigError::Validation(format!(
                    "project `{}` concurrency.max_sessions must be greater than zero",
                    project.id
                )));
            }
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CleanupConfig {
    #[serde(default = "default_cleanup_enabled")]
    pub enabled: bool,
    #[serde(default = "default_cleanup_interval_secs")]
    pub interval_secs: u64,
    #[serde(default = "default_cleanup_retention_secs")]
    pub retention_secs: u64,
}

impl Default for CleanupConfig {
    fn default() -> Self {
        Self {
            enabled: default_cleanup_enabled(),
            interval_secs: default_cleanup_interval_secs(),
            retention_secs: default_cleanup_retention_secs(),
        }
    }
}

impl CleanupConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        if !self.enabled {
            return Ok(());
        }
        if self.interval_secs == 0 {
            return Err(ConfigError::Validation(
                "cleanup.interval_secs must be greater than zero".into(),
            ));
        }
        if self.retention_secs == 0 {
            return Err(ConfigError::Validation(
                "cleanup.retention_secs must be greater than zero".into(),
            ));
        }
        Ok(())
    }
}

fn default_cleanup_enabled() -> bool {
    true
}

fn default_cleanup_interval_secs() -> u64 {
    300
}

fn default_cleanup_retention_secs() -> u64 {
    86_400
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub workflow_path: PathBuf,
    pub repo_path: PathBuf,
    pub branch: BranchPolicy,
    pub linear: LinearProjectConfig,
    pub opencode: OpenCodeRuntimeConfig,
    pub eval: EvalDefaults,
    pub concurrency: ConcurrencyConfig,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BranchPolicy {
    pub base: String,
    pub worktree_root: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvalDefaults {
    pub default_suite: String,
    #[serde(default = "default_max_identical_failure_fingerprints")]
    pub max_identical_failure_fingerprints: u32,
}

fn default_max_identical_failure_fingerprints() -> u32 {
    2
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConcurrencyConfig {
    pub max_sessions: u32,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("invalid root config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid root config: {0}")]
    Validation(String),
}
