use std::{
    collections::{BTreeSet, HashSet},
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{linear::LinearProjectConfig, runner::RunnerRuntimeConfig, state::RuntimeProviderMode};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RootConfig {
    pub server: Option<ServerConfig>,
    #[serde(default)]
    pub cleanup: CleanupConfig,
    #[serde(default)]
    pub workflow: ProjectWorkflow,
    pub runner_archive: Option<RunnerArchiveConfig>,
    projects: Vec<ProjectConfig>,
}

impl RootConfig {
    pub fn from_toml_str(input: &str) -> Result<Self, ConfigError> {
        let mut config: Self = toml::from_str(input)?;
        config.apply_root_workflow_defaults()?;
        config.validate()?;
        Ok(config)
    }

    pub fn from_toml_file(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let input = fs::read_to_string(path).map_err(|source| ConfigError::RootIo {
            path: path.to_path_buf(),
            source,
        })?;
        let mut config: Self = toml::from_str(&input)?;
        config.load_project_workflows()?;
        config.validate()?;
        Ok(config)
    }

    fn load_project_workflows(&mut self) -> Result<(), ConfigError> {
        for project in &mut self.projects {
            let mut workflow = self.workflow.clone();
            let path = project.resolved_workflow_path();
            if !path.exists() {
                workflow.validate(&project.id)?;
                project.workflow = workflow;
                continue;
            }
            let workflow_override = load_workflow_override_file(&project.id, &path)?;
            workflow.apply_override(workflow_override);
            workflow.validate(&project.id)?;
            project.workflow = workflow;
        }
        Ok(())
    }

    fn apply_root_workflow_defaults(&mut self) -> Result<(), ConfigError> {
        for project in &mut self.projects {
            let workflow = self.workflow.clone();
            workflow.validate(&project.id)?;
            project.workflow = workflow;
        }
        Ok(())
    }

    fn validate_root_workflow(&self) -> Result<(), ConfigError> {
        self.workflow.validate("root")
    }

    pub fn projects(&self) -> &[ProjectConfig] {
        &self.projects
    }

    pub fn project(&self, id: &str) -> Option<&ProjectConfig> {
        self.projects.iter().find(|project| project.id == id)
    }

    #[cfg(test)]
    pub(crate) fn project_mut_for_test(&mut self, id: &str) -> &mut ProjectConfig {
        self.projects
            .iter_mut()
            .find(|project| project.id == id)
            .expect("test project")
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.projects.is_empty() {
            return Err(ConfigError::Validation("projects must not be empty".into()));
        }
        self.validate_root_workflow()?;
        self.cleanup.validate()?;
        if let Some(storage) = &self.runner_archive {
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
            if project.runner.agent.trim().is_empty() {
                return Err(ConfigError::Validation(format!(
                    "project `{}` runner.agent must not be empty",
                    project.id
                )));
            }
            if project.runner.provider_mode == RuntimeProviderMode::OmpAcp
                && project.omp_acp_providers.is_empty()
            {
                return Err(ConfigError::Validation(format!(
                    "project `{}` runner.provider_mode `omp_acp` requires at least one omp_acp_providers entry",
                    project.id
                )));
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
            project.workflow.validate(&project.id)?;
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
pub struct RunnerArchiveConfig {
    pub database_path: PathBuf,
    pub archive_root: PathBuf,
}

impl RunnerArchiveConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.database_path.as_os_str().is_empty() {
            return Err(ConfigError::Validation(
                "runner_archive.database_path must not be empty".into(),
            ));
        }
        if self.archive_root.as_os_str().is_empty() {
            return Err(ConfigError::Validation(
                "runner_archive.archive_root must not be empty".into(),
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
    #[serde(default = "default_workflow_path")]
    pub workflow_path: PathBuf,
    pub repo_path: PathBuf,
    pub branch: BranchPolicy,
    pub linear: LinearProjectConfig,
    pub runner: RunnerRuntimeConfig,
    #[serde(default)]
    pub omp_acp_providers: Vec<OhMyPiAcpProviderConfig>,
    pub eval: EvalDefaults,
    pub concurrency: ConcurrencyConfig,
    #[serde(skip, default)]
    pub workflow: ProjectWorkflow,
}

impl ProjectConfig {
    pub fn resolved_workflow_path(&self) -> PathBuf {
        if self.workflow_path.is_absolute() {
            self.workflow_path.clone()
        } else {
            self.repo_path.join(&self.workflow_path)
        }
    }
}

fn default_workflow_path() -> PathBuf {
    PathBuf::from("workflow.toml")
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStage {
    Backlog,
    Todo,
    InProgress,
    InReview,
    NeedOwnerInput,
    Done,
    Canceled,
}

impl WorkflowStage {
    const REQUIRED: [Self; 6] = [
        Self::Todo,
        Self::InProgress,
        Self::InReview,
        Self::NeedOwnerInput,
        Self::Done,
        Self::Canceled,
    ];

    const ALL: [Self; 7] = [
        Self::Backlog,
        Self::Todo,
        Self::InProgress,
        Self::InReview,
        Self::NeedOwnerInput,
        Self::Done,
        Self::Canceled,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Backlog => "backlog",
            Self::Todo => "todo",
            Self::InProgress => "in_progress",
            Self::InReview => "in_review",
            Self::NeedOwnerInput => "need_owner_input",
            Self::Done => "done",
            Self::Canceled => "canceled",
        }
    }

    const fn default_linear_state(self) -> &'static str {
        match self {
            Self::Backlog => "Backlog",
            Self::Todo => "Todo",
            Self::InProgress => "In Progress",
            Self::InReview => "In Review",
            Self::NeedOwnerInput => "Need Owner Input",
            Self::Done => "Done",
            Self::Canceled => "Canceled",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectWorkflow {
    pub states: WorkflowStates,
    #[serde(default)]
    pub processed_states: Vec<String>,
    pub agents: WorkflowAgents,
    pub owner_input: OwnerInputPolicy,
    #[serde(default)]
    pub self_defects: WorkflowSelfDefectPolicy,
}

impl Default for ProjectWorkflow {
    fn default() -> Self {
        Self {
            states: WorkflowStates::default(),
            processed_states: Vec::new(),
            agents: WorkflowAgents::default(),
            owner_input: OwnerInputPolicy::default(),
            self_defects: WorkflowSelfDefectPolicy::default(),
        }
    }
}

impl ProjectWorkflow {
    fn apply_override(&mut self, workflow_override: ProjectWorkflowOverride) {
        if let Some(states) = workflow_override.states {
            self.states.apply_override(states);
        }
        if let Some(processed_states) = workflow_override.processed_states {
            self.processed_states = processed_states;
        }
        if let Some(agents) = workflow_override.agents {
            self.agents.apply_override(agents);
        }
        if let Some(owner_input) = workflow_override.owner_input {
            self.owner_input.apply_override(owner_input);
        }
        if let Some(self_defects) = workflow_override.self_defects {
            self.self_defects.apply_override(self_defects);
        }
    }

    fn validate(&self, project_id: &str) -> Result<(), ConfigError> {
        self.states.validate(project_id)?;
        self.agents.validate(project_id)?;
        self.owner_input.validate(project_id)?;
        self.self_defects.validate(project_id)?;
        for state in &self.processed_states {
            if state.trim().is_empty() {
                return Err(ConfigError::Validation(format!(
                    "project `{project_id}` workflow.processed_states entries must not be empty"
                )));
            }
        }
        Ok(())
    }

    pub fn linear_state(&self, stage: WorkflowStage) -> Option<&str> {
        self.states.linear_state(stage)
    }

    pub fn required_linear_state(&self, stage: WorkflowStage) -> &str {
        self.linear_state(stage)
            .expect("required workflow state validated")
    }

    pub fn stage_for_linear_state(&self, state: &str) -> Option<WorkflowStage> {
        WorkflowStage::ALL
            .into_iter()
            .find(|stage| self.linear_state(*stage) == Some(state))
    }

    pub fn is_stage(&self, state: &str, stage: WorkflowStage) -> bool {
        self.linear_state(stage) == Some(state)
    }

    pub fn is_terminal_stage(&self, stage: WorkflowStage) -> bool {
        matches!(stage, WorkflowStage::Done | WorkflowStage::Canceled)
    }

    pub fn is_open_linear_state(&self, state: &str) -> bool {
        !self
            .stage_for_linear_state(state)
            .is_some_and(|stage| self.is_terminal_stage(stage))
    }

    pub fn processed_state_names(&self) -> Vec<&str> {
        let mut states = Vec::new();
        for stage in WorkflowStage::ALL {
            if let Some(state) = self.linear_state(stage)
                && !states.contains(&state)
            {
                states.push(state);
            }
        }
        for state in &self.processed_states {
            let state = state.as_str();
            if !states.contains(&state) {
                states.push(state);
            }
        }
        states
    }

    pub fn agent_for_stage(&self, stage: WorkflowStage, labels: &[String]) -> Option<&str> {
        self.agents.agent_for_stage(stage, labels)
    }

    pub fn agent_route_for_stage(
        &self,
        stage: WorkflowStage,
        labels: &[String],
    ) -> Option<AgentRoutingDecision> {
        self.agents.route_for_stage(stage, labels)
    }

    pub fn block_project_dispatch_for_owner_input(&self) -> bool {
        self.owner_input.block_project_dispatch
    }

    pub fn owner_input_return_stage(&self) -> WorkflowStage {
        self.owner_input.return_stage
    }

    pub fn self_defect_execution_promoted(&self, labels: &[String]) -> bool {
        self.self_defects
            .executable_label
            .as_deref()
            .is_some_and(|expected| labels.iter().any(|label| label == expected))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ProjectWorkflowOverride {
    pub states: Option<WorkflowStatesOverride>,
    pub processed_states: Option<Vec<String>>,
    pub agents: Option<WorkflowAgentsOverride>,
    pub owner_input: Option<OwnerInputPolicyOverride>,
    pub self_defects: Option<WorkflowSelfDefectPolicyOverride>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowStates {
    pub todo: String,
    pub in_progress: String,
    pub in_review: String,
    pub need_owner_input: String,
    pub done: String,
    pub canceled: String,
    pub backlog: Option<String>,
}

impl Default for WorkflowStates {
    fn default() -> Self {
        Self {
            todo: WorkflowStage::Todo.default_linear_state().into(),
            in_progress: WorkflowStage::InProgress.default_linear_state().into(),
            in_review: WorkflowStage::InReview.default_linear_state().into(),
            need_owner_input: WorkflowStage::NeedOwnerInput.default_linear_state().into(),
            done: WorkflowStage::Done.default_linear_state().into(),
            canceled: WorkflowStage::Canceled.default_linear_state().into(),
            backlog: Some(WorkflowStage::Backlog.default_linear_state().into()),
        }
    }
}

impl WorkflowStates {
    fn apply_override(&mut self, workflow_override: WorkflowStatesOverride) {
        if let Some(todo) = workflow_override.todo {
            self.todo = todo;
        }
        if let Some(in_progress) = workflow_override.in_progress {
            self.in_progress = in_progress;
        }
        if let Some(in_review) = workflow_override.in_review {
            self.in_review = in_review;
        }
        if let Some(need_owner_input) = workflow_override.need_owner_input {
            self.need_owner_input = need_owner_input;
        }
        if let Some(done) = workflow_override.done {
            self.done = done;
        }
        if let Some(canceled) = workflow_override.canceled {
            self.canceled = canceled;
        }
        if let Some(backlog) = workflow_override.backlog {
            self.backlog = backlog;
        }
    }

    fn validate(&self, project_id: &str) -> Result<(), ConfigError> {
        for stage in WorkflowStage::REQUIRED {
            let state = self.linear_state(stage).unwrap_or_default();
            if state.trim().is_empty() {
                return Err(ConfigError::Validation(format!(
                    "project `{project_id}` workflow.states.{} must not be empty",
                    stage.as_str()
                )));
            }
        }
        if self
            .backlog
            .as_deref()
            .is_some_and(|state| state.trim().is_empty())
        {
            return Err(ConfigError::Validation(format!(
                "project `{project_id}` workflow.states.backlog must not be empty when configured"
            )));
        }
        Ok(())
    }

    fn linear_state(&self, stage: WorkflowStage) -> Option<&str> {
        match stage {
            WorkflowStage::Backlog => self.backlog.as_deref(),
            WorkflowStage::Todo => Some(&self.todo),
            WorkflowStage::InProgress => Some(&self.in_progress),
            WorkflowStage::InReview => Some(&self.in_review),
            WorkflowStage::NeedOwnerInput => Some(&self.need_owner_input),
            WorkflowStage::Done => Some(&self.done),
            WorkflowStage::Canceled => Some(&self.canceled),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkflowStatesOverride {
    pub todo: Option<String>,
    pub in_progress: Option<String>,
    pub in_review: Option<String>,
    pub need_owner_input: Option<String>,
    pub done: Option<String>,
    pub canceled: Option<String>,
    pub backlog: Option<Option<String>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowAgents {
    pub default: WorkflowDefaultAgents,
    #[serde(default)]
    pub labels: Vec<LabelAgentMapping>,
}

impl Default for WorkflowAgents {
    fn default() -> Self {
        Self {
            default: WorkflowDefaultAgents::default(),
            labels: default_label_agent_mappings(),
        }
    }
}

impl WorkflowAgents {
    fn apply_override(&mut self, workflow_override: WorkflowAgentsOverride) {
        if let Some(default) = workflow_override.default {
            self.default.apply_override(default);
        }
        if let Some(labels) = workflow_override.labels {
            self.merge_label_overrides(labels);
        }
    }

    fn merge_label_overrides(&mut self, overrides: Vec<LabelAgentMapping>) {
        for workflow_override in overrides {
            let override_stages = workflow_override.effective_stages().collect::<Vec<_>>();
            self.labels.retain(|existing| {
                if existing.label != workflow_override.label {
                    return true;
                }
                let existing_stages = existing.effective_stages().collect::<Vec<_>>();
                !existing_stages
                    .iter()
                    .any(|stage| override_stages.contains(stage))
            });
            self.labels.push(workflow_override);
        }
    }

    fn validate(&self, project_id: &str) -> Result<(), ConfigError> {
        self.default.validate(project_id)?;
        let mut seen = BTreeSet::new();
        for mapping in &self.labels {
            mapping.validate(project_id)?;
            for stage in mapping.effective_stages() {
                let key = (mapping.label.as_str(), stage);
                if !seen.insert(key) {
                    return Err(ConfigError::Validation(format!(
                        "project `{project_id}` workflow.agents.labels has duplicate label mapping `{}` for stage `{}`",
                        mapping.label,
                        stage.as_str()
                    )));
                }
            }
        }
        Ok(())
    }

    fn agent_for_stage(&self, stage: WorkflowStage, labels: &[String]) -> Option<&str> {
        self.selected_label_mapping(stage, labels)
            .map(|mapping| mapping.agent.as_str())
            .or_else(|| self.default.agent_for_stage(stage))
    }

    fn route_for_stage(
        &self,
        stage: WorkflowStage,
        labels: &[String],
    ) -> Option<AgentRoutingDecision> {
        self.selected_label_mapping(stage, labels)
            .map(|mapping| AgentRoutingDecision {
                stage,
                selected_agent: mapping.agent.clone(),
                selected_label: Some(mapping.label.clone()),
                reason: AgentRoutingReason::Label,
            })
            .or_else(|| {
                self.default
                    .agent_for_stage(stage)
                    .map(|agent| AgentRoutingDecision {
                        stage,
                        selected_agent: agent.to_owned(),
                        selected_label: None,
                        reason: AgentRoutingReason::Fallback,
                    })
            })
    }

    fn selected_label_mapping(
        &self,
        stage: WorkflowStage,
        labels: &[String],
    ) -> Option<&LabelAgentMapping> {
        self.labels
            .iter()
            .filter(|mapping| {
                mapping.matches_stage(stage)
                    && labels
                        .iter()
                        .any(|label| label.as_str() == mapping.label.as_str())
            })
            .max_by(|left, right| {
                left.precedence
                    .cmp(&right.precedence)
                    .then_with(|| right.label.cmp(&left.label))
            })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkflowAgentsOverride {
    pub default: Option<WorkflowDefaultAgentsOverride>,
    pub labels: Option<Vec<LabelAgentMapping>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentRoutingDecision {
    pub stage: WorkflowStage,
    pub selected_agent: String,
    pub selected_label: Option<String>,
    pub reason: AgentRoutingReason,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentRoutingReason {
    Label,
    Fallback,
}

impl AgentRoutingReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Label => "label",
            Self::Fallback => "fallback",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefaultAgents {
    pub todo: String,
    pub in_progress: String,
    pub in_review: String,
    pub need_owner_input: String,
    pub done: String,
    pub canceled: String,
    pub backlog: Option<String>,
}

impl Default for WorkflowDefaultAgents {
    fn default() -> Self {
        Self {
            todo: "build".into(),
            in_progress: "build".into(),
            in_review: "code-reviewer".into(),
            need_owner_input: "build".into(),
            done: "build".into(),
            canceled: "build".into(),
            backlog: Some("build".into()),
        }
    }
}

impl WorkflowDefaultAgents {
    fn apply_override(&mut self, workflow_override: WorkflowDefaultAgentsOverride) {
        if let Some(todo) = workflow_override.todo {
            self.todo = todo;
        }
        if let Some(in_progress) = workflow_override.in_progress {
            self.in_progress = in_progress;
        }
        if let Some(in_review) = workflow_override.in_review {
            self.in_review = in_review;
        }
        if let Some(need_owner_input) = workflow_override.need_owner_input {
            self.need_owner_input = need_owner_input;
        }
        if let Some(done) = workflow_override.done {
            self.done = done;
        }
        if let Some(canceled) = workflow_override.canceled {
            self.canceled = canceled;
        }
        if let Some(backlog) = workflow_override.backlog {
            self.backlog = backlog;
        }
    }

    fn validate(&self, project_id: &str) -> Result<(), ConfigError> {
        for stage in WorkflowStage::REQUIRED {
            let agent = self.agent_for_stage(stage).unwrap_or_default();
            if agent.trim().is_empty() {
                return Err(ConfigError::Validation(format!(
                    "project `{project_id}` workflow.agents.default.{} must not be empty",
                    stage.as_str()
                )));
            }
        }
        if self
            .backlog
            .as_deref()
            .is_some_and(|agent| agent.trim().is_empty())
        {
            return Err(ConfigError::Validation(format!(
                "project `{project_id}` workflow.agents.default.backlog must not be empty when configured"
            )));
        }
        Ok(())
    }

    fn agent_for_stage(&self, stage: WorkflowStage) -> Option<&str> {
        match stage {
            WorkflowStage::Backlog => self.backlog.as_deref(),
            WorkflowStage::Todo => Some(&self.todo),
            WorkflowStage::InProgress => Some(&self.in_progress),
            WorkflowStage::InReview => Some(&self.in_review),
            WorkflowStage::NeedOwnerInput => Some(&self.need_owner_input),
            WorkflowStage::Done => Some(&self.done),
            WorkflowStage::Canceled => Some(&self.canceled),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkflowDefaultAgentsOverride {
    pub todo: Option<String>,
    pub in_progress: Option<String>,
    pub in_review: Option<String>,
    pub need_owner_input: Option<String>,
    pub done: Option<String>,
    pub canceled: Option<String>,
    pub backlog: Option<Option<String>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LabelAgentMapping {
    pub label: String,
    pub agent: String,
    #[serde(default)]
    pub precedence: i32,
    #[serde(default)]
    pub stages: Vec<WorkflowStage>,
}

impl LabelAgentMapping {
    fn validate(&self, project_id: &str) -> Result<(), ConfigError> {
        if self.label.trim().is_empty() {
            return Err(ConfigError::Validation(format!(
                "project `{project_id}` workflow.agents.labels.label must not be empty"
            )));
        }
        if self.agent.trim().is_empty() {
            return Err(ConfigError::Validation(format!(
                "project `{project_id}` workflow.agents.labels `{}` agent must not be empty",
                self.label
            )));
        }
        Ok(())
    }

    fn effective_stages(&self) -> impl Iterator<Item = WorkflowStage> + '_ {
        let fallback = self.stages.is_empty().then_some(WorkflowStage::InProgress);
        self.stages.iter().copied().chain(fallback)
    }

    fn matches_stage(&self, stage: WorkflowStage) -> bool {
        self.effective_stages().any(|candidate| candidate == stage)
    }
}

fn default_label_agent_mappings() -> Vec<LabelAgentMapping> {
    [
        ("rust", WorkflowStage::InProgress, "rust-engineer"),
        ("rust", WorkflowStage::InReview, "rust-reviewer"),
        (
            "typescript",
            WorkflowStage::InProgress,
            "typescript-engineer",
        ),
        ("typescript", WorkflowStage::InReview, "typescript-reviewer"),
        ("frontend", WorkflowStage::InProgress, "typescript-engineer"),
        ("frontend", WorkflowStage::InReview, "typescript-reviewer"),
        ("ui", WorkflowStage::InProgress, "typescript-engineer"),
        ("ui", WorkflowStage::InReview, "ux-ui-reviewer"),
        ("python", WorkflowStage::InProgress, "python-engineer"),
        ("python", WorkflowStage::InReview, "python-reviewer"),
        ("contract", WorkflowStage::InProgress, "build"),
        ("contract", WorkflowStage::InReview, "contract-reviewer"),
        ("api", WorkflowStage::InProgress, "build"),
        ("api", WorkflowStage::InReview, "contract-reviewer"),
    ]
    .into_iter()
    .map(|(label, stage, agent)| LabelAgentMapping {
        label: label.into(),
        agent: agent.into(),
        precedence: 100,
        stages: vec![stage],
    })
    .collect()
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerInputPolicy {
    #[serde(default = "default_owner_input_blocks_project")]
    pub block_project_dispatch: bool,
    #[serde(default = "default_owner_input_return_stage")]
    pub return_stage: WorkflowStage,
}

impl Default for OwnerInputPolicy {
    fn default() -> Self {
        Self {
            block_project_dispatch: default_owner_input_blocks_project(),
            return_stage: default_owner_input_return_stage(),
        }
    }
}

impl OwnerInputPolicy {
    fn apply_override(&mut self, workflow_override: OwnerInputPolicyOverride) {
        if let Some(block_project_dispatch) = workflow_override.block_project_dispatch {
            self.block_project_dispatch = block_project_dispatch;
        }
        if let Some(return_stage) = workflow_override.return_stage {
            self.return_stage = return_stage;
        }
    }

    fn validate(&self, project_id: &str) -> Result<(), ConfigError> {
        if !matches!(
            self.return_stage,
            WorkflowStage::Todo | WorkflowStage::Backlog
        ) {
            return Err(ConfigError::Validation(format!(
                "project `{project_id}` workflow.owner_input.return_stage must be todo or backlog"
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct OwnerInputPolicyOverride {
    pub block_project_dispatch: Option<bool>,
    pub return_stage: Option<WorkflowStage>,
}

const fn default_owner_input_blocks_project() -> bool {
    true
}

const fn default_owner_input_return_stage() -> WorkflowStage {
    WorkflowStage::Todo
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowSelfDefectPolicy {
    pub executable_label: Option<String>,
}

impl Default for WorkflowSelfDefectPolicy {
    fn default() -> Self {
        Self {
            executable_label: Some("self-defect-executable".into()),
        }
    }
}

impl WorkflowSelfDefectPolicy {
    fn apply_override(&mut self, workflow_override: WorkflowSelfDefectPolicyOverride) {
        if let Some(executable_label) = workflow_override.executable_label {
            self.executable_label = executable_label;
        }
    }

    fn validate(&self, project_id: &str) -> Result<(), ConfigError> {
        if self
            .executable_label
            .as_deref()
            .is_some_and(|label| label.trim().is_empty())
        {
            return Err(ConfigError::Validation(format!(
                "project `{project_id}` workflow.self_defects.executable_label must not be empty when configured"
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkflowSelfDefectPolicyOverride {
    pub executable_label: Option<Option<String>>,
}

fn load_workflow_override_file(
    project_id: &str,
    path: &Path,
) -> Result<ProjectWorkflowOverride, ConfigError> {
    let input = fs::read_to_string(path).map_err(|source| ConfigError::WorkflowIo {
        project_id: project_id.to_owned(),
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str::<ProjectWorkflowOverride>(&input).map_err(|source| ConfigError::WorkflowParse {
        project_id: project_id.to_owned(),
        path: path.to_path_buf(),
        source,
    })
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("read root config {path}: {source}")]
    RootIo { path: PathBuf, source: io::Error },
    #[error("invalid root config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("read workflow config for project `{project_id}` at {path}: {source}")]
    WorkflowIo {
        project_id: String,
        path: PathBuf,
        source: io::Error,
    },
    #[error("invalid workflow config for project `{project_id}` at {path}: {source}")]
    WorkflowParse {
        project_id: String,
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("invalid root config: {0}")]
    Validation(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root_config_toml(repo_path: &Path) -> String {
        format!(
            r#"
[[projects]]
id = "symphony"
name = "Symphony"
enabled = true
repo_path = "{}"

[projects.branch]
base = "main"
worktree_root = "/tmp/worktrees"

[projects.linear]
team_key = "SYM"
project_id = "linear-project"

[projects.runner]
command = "/usr/local/bin/omp"
args = ["acp"]
agent = "build"
model = "openai/gpt-5.5"
effort = "high"
permission_policy = "reject"

[projects.eval]
default_suite = "symphony-validation"

[projects.concurrency]
max_sessions = 1
"#,
            repo_path.display()
        )
    }

    fn valid_workflow_toml() -> &'static str {
        r#"
processed_states = ["Backlog"]

[states]
todo = "Todo"
in_progress = "In Progress"
in_review = "In Review"
need_owner_input = "Need Owner Input"
done = "Done"
canceled = "Canceled"
backlog = "Backlog"

[agents.default]
todo = "build"
in_progress = "build"
in_review = "code-reviewer"
need_owner_input = "build"
done = "build"
canceled = "build"
backlog = "build"

[[agents.labels]]
label = "rust"
agent = "rust-engineer"
precedence = 50
stages = ["in_progress"]

[[agents.labels]]
label = "rust"
agent = "rust-reviewer"
precedence = 50
stages = ["in_review"]

[[agents.labels]]
label = "urgent"
agent = "integrator"
precedence = 100
stages = ["in_progress"]

[self_defects]
executable_label = "self-defect-executable"
[owner_input]
block_project_dispatch = true
return_stage = "todo"
"#
    }

    #[test]
    fn config_loads_valid_project_workflow_contract() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("workflow.toml"), valid_workflow_toml()).expect("workflow");
        let root = dir.path().join("symphony.projects.toml");
        fs::write(&root, root_config_toml(dir.path())).expect("root config");

        let config = RootConfig::from_toml_file(&root).expect("config");
        let project = config.project("symphony").expect("project");

        assert_eq!(
            project.workflow.processed_state_names(),
            vec![
                "Backlog",
                "Todo",
                "In Progress",
                "In Review",
                "Need Owner Input",
                "Done",
                "Canceled"
            ]
        );
        assert_eq!(
            project
                .workflow
                .agent_route_for_stage(WorkflowStage::InProgress, &["rust".into(), "urgent".into()])
                .expect("route")
                .selected_agent,
            "integrator"
        );
        let review_route = project
            .workflow
            .agent_route_for_stage(WorkflowStage::InReview, &["rust".into()])
            .expect("review route");
        assert_eq!(review_route.selected_agent, "rust-reviewer");
        assert_eq!(review_route.selected_label.as_deref(), Some("rust"));
        let fallback_route = project
            .workflow
            .agent_route_for_stage(WorkflowStage::InProgress, &[])
            .expect("fallback route");
        assert_eq!(fallback_route.selected_agent, "build");
        assert_eq!(fallback_route.reason, AgentRoutingReason::Fallback);
        let contract_review = ProjectWorkflow::default()
            .agent_route_for_stage(WorkflowStage::InReview, &["contract".into()])
            .expect("contract review route");
        assert_eq!(contract_review.selected_agent, "contract-reviewer");
        assert_eq!(contract_review.selected_label.as_deref(), Some("contract"));
    }

    #[test]
    fn config_rejects_missing_required_workflow_state() {
        let workflow = valid_workflow_toml().replace("in_review = \"In Review\"\n", "");

        let err =
            toml::from_str::<ProjectWorkflow>(&workflow).expect_err("missing in_review must fail");

        assert!(err.to_string().contains("in_review"), "{err}");
    }

    #[test]
    fn config_rejects_unknown_workflow_stage() {
        let workflow =
            valid_workflow_toml().replace("stages = [\"in_progress\"]", "stages = [\"triage\"]");

        let err =
            toml::from_str::<ProjectWorkflow>(&workflow).expect_err("unknown stage must fail");

        assert!(err.to_string().contains("triage"), "{err}");
    }

    #[test]
    fn config_rejects_duplicate_label_mapping_for_stage() {
        let duplicate = format!(
            "{}\n[[agents.labels]]\nlabel = \"rust\"\nagent = \"integrator\"\nprecedence = 60\nstages = [\"in_progress\"]\n",
            valid_workflow_toml()
        );
        let workflow =
            toml::from_str::<ProjectWorkflow>(&duplicate).expect("workflow parse succeeds");

        let err = workflow.validate("symphony").expect_err("duplicate label");

        assert!(err.to_string().contains("duplicate label mapping"), "{err}");
    }

    #[test]
    fn config_uses_root_workflow_when_project_workflow_file_is_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("symphony.projects.toml");
        fs::write(&root, root_config_toml(dir.path())).expect("root config");

        let config = RootConfig::from_toml_file(&root).expect("config");
        let project = config.project("symphony").expect("project");

        assert_eq!(
            project
                .workflow
                .required_linear_state(WorkflowStage::InReview),
            "In Review"
        );
        assert_eq!(
            project
                .workflow
                .agent_route_for_stage(WorkflowStage::InProgress, &["rust".into()])
                .expect("route")
                .selected_agent,
            "rust-engineer"
        );
    }

    #[test]
    fn config_merges_project_workflow_override_over_root_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(
            dir.path().join("workflow.toml"),
            r#"
[agents.default]
in_review = "strict-reviewer"

[[agents.labels]]
label = "ops"
agent = "incident-investigator"
precedence = 200
stages = ["in_progress"]
"#,
        )
        .expect("workflow override");
        let root = dir.path().join("symphony.projects.toml");
        fs::write(&root, root_config_toml(dir.path())).expect("root config");

        let config = RootConfig::from_toml_file(&root).expect("config");
        let project = config.project("symphony").expect("project");

        assert_eq!(
            project
                .workflow
                .agent_route_for_stage(WorkflowStage::InReview, &[])
                .expect("fallback review route")
                .selected_agent,
            "strict-reviewer"
        );
        assert_eq!(
            project
                .workflow
                .agent_route_for_stage(WorkflowStage::InProgress, &["ops".into()])
                .expect("label route")
                .selected_agent,
            "incident-investigator"
        );
        assert_eq!(
            project
                .workflow
                .agent_route_for_stage(WorkflowStage::InProgress, &["rust".into()])
                .expect("root default route")
                .selected_agent,
            "rust-engineer"
        );
    }
}
