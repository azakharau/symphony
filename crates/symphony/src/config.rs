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
    pub opencode_storage: Option<OpenCodeStorageConfig>,
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
        if let Some(storage) = &self.opencode_storage {
            storage.validate()?;
        }

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
            if let Some(recall) = &project.recall {
                recall.validate(&project.id)?;
            }
            let mut seen_omp_provider_ids = HashSet::new();
            for provider in &project.omp_acp_providers {
                provider.validate(&project.id)?;
                if !seen_omp_provider_ids.insert(provider.id.as_str()) {
                    return Err(ConfigError::Validation(format!(
                        "project `{}` duplicate omp_acp_providers id `{}`",
                        project.id, provider.id
                    )));
                }
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
pub struct OpenCodeStorageConfig {
    pub database_path: PathBuf,
    pub archive_root: PathBuf,
}

impl OpenCodeStorageConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.database_path.as_os_str().is_empty() {
            return Err(ConfigError::Validation(
                "opencode_storage.database_path must not be empty".into(),
            ));
        }
        if self.archive_root.as_os_str().is_empty() {
            return Err(ConfigError::Validation(
                "opencode_storage.archive_root must not be empty".into(),
            ));
        }
        Ok(())
    }
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

const fn default_cleanup_enabled() -> bool {
    true
}

const fn default_cleanup_interval_secs() -> u64 {
    300
}

const fn default_cleanup_retention_secs() -> u64 {
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
    pub recall: Option<RecallProjectConfig>,
    pub branch: BranchPolicy,
    pub linear: LinearProjectConfig,
    pub opencode: OpenCodeRuntimeConfig,
    #[serde(default)]
    pub omp_acp_providers: Vec<OhMyPiAcpProviderConfig>,
    pub eval: EvalDefaults,
    pub concurrency: ConcurrencyConfig,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OhMyPiAcpProviderConfig {
    pub id: String,
    pub command: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: OhMyPiAcpCwdPolicy,
    #[serde(default)]
    pub env_allowlist: Vec<String>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    #[serde(default)]
    pub live_smoke: bool,
    pub capabilities: OhMyPiAcpProviderCapabilities,
}

impl OhMyPiAcpProviderConfig {
    fn validate(&self, project_id: &str) -> Result<(), ConfigError> {
        if self.id.trim().is_empty() {
            return Err(ConfigError::Validation(format!(
                "project `{project_id}` omp_acp_providers.id must not be empty"
            )));
        }
        if self.command.as_os_str().is_empty() {
            return Err(ConfigError::Validation(format!(
                "project `{project_id}` omp_acp_providers `{}` command must not be empty",
                self.id
            )));
        }
        if self
            .agent
            .as_deref()
            .is_some_and(|agent| agent.trim().is_empty())
        {
            return Err(ConfigError::Validation(format!(
                "project `{project_id}` omp_acp_providers `{}` agent must not be empty",
                self.id
            )));
        }
        if self
            .model
            .as_deref()
            .is_some_and(|model| model.trim().is_empty())
        {
            return Err(ConfigError::Validation(format!(
                "project `{project_id}` omp_acp_providers `{}` model must not be empty",
                self.id
            )));
        }
        if self
            .effort
            .as_deref()
            .is_some_and(|effort| effort.trim().is_empty())
        {
            return Err(ConfigError::Validation(format!(
                "project `{project_id}` omp_acp_providers `{}` effort must not be empty",
                self.id
            )));
        }
        if self.env_allowlist.iter().any(|name| name.trim().is_empty()) {
            return Err(ConfigError::Validation(format!(
                "project `{project_id}` omp_acp_providers `{}` env_allowlist entries must not be empty",
                self.id
            )));
        }
        self.capabilities.validate(project_id, &self.id)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OhMyPiAcpCwdPolicy {
    IssueWorktree,
    ProjectRepo,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OhMyPiAcpProviderCapabilities {
    pub acp_stdio: bool,
    pub hook_evidence: bool,
    pub sdk_session_evidence: bool,
    pub rpc_secondary_mode: bool,
    pub inverse_bridge_reference: bool,
}

impl OhMyPiAcpProviderCapabilities {
    fn validate(&self, project_id: &str, provider_id: &str) -> Result<(), ConfigError> {
        if !self.acp_stdio {
            return Err(ConfigError::Validation(format!(
                "project `{project_id}` omp_acp_providers `{provider_id}` capabilities.acp_stdio must be true"
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecallProjectConfig {
    pub workspace_root: PathBuf,
}

impl RecallProjectConfig {
    fn validate(&self, project_id: &str) -> Result<(), ConfigError> {
        if self.workspace_root.as_os_str().is_empty() {
            return Err(ConfigError::Validation(format!(
                "project `{project_id}` recall.workspace_root must not be empty"
            )));
        }
        if !self.workspace_root.is_absolute() {
            return Err(ConfigError::Validation(format!(
                "project `{project_id}` recall.workspace_root must be absolute"
            )));
        }
        Ok(())
    }
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

const fn default_max_identical_failure_fingerprints() -> u32 {
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
