use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectStateRecord {
    pub project_id: String,
    pub name: String,
    pub enabled: bool,
    pub lifecycle_stage: LifecycleStage,
    pub cleanup_status: CleanupStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectRuntimeLivenessRecord {
    pub project_id: String,
    pub status: RuntimeLivenessStatus,
    pub reason: String,
    pub last_poll_at: Option<String>,
    pub last_successful_candidate_scan_at: Option<String>,
    pub max_sessions: u32,
    pub running_sessions: u32,
    pub available_sessions: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeLivenessStatus {
    InactiveRuntime,
    NoEligibleIssues,
    BlockedIssues,
    CapacityFull,
    HealthyCapacityAvailable,
    RunnerProcessDead,
    RunnerSetupFailed,
    RunnerStaleKilled,
}

impl RuntimeLivenessStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InactiveRuntime => "inactive_runtime",
            Self::NoEligibleIssues => "no_eligible_issues",
            Self::BlockedIssues => "blocked_issues",
            Self::CapacityFull => "capacity_full",
            Self::HealthyCapacityAvailable => "healthy_capacity_available",
            Self::RunnerProcessDead => "runner_process_dead",
            Self::RunnerSetupFailed => "runner_setup_failed",
            Self::RunnerStaleKilled => "runner_stale_killed",
        }
    }
}

impl fmt::Display for RuntimeLivenessStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for RuntimeLivenessStatus {
    type Err = StateParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "inactive_runtime" => Ok(Self::InactiveRuntime),
            "no_eligible_issues" => Ok(Self::NoEligibleIssues),
            "blocked_issues" => Ok(Self::BlockedIssues),
            "capacity_full" => Ok(Self::CapacityFull),
            "healthy_capacity_available" => Ok(Self::HealthyCapacityAvailable),
            "runner_process_dead" => Ok(Self::RunnerProcessDead),
            "runner_setup_failed" => Ok(Self::RunnerSetupFailed),
            "runner_stale_killed" => Ok(Self::RunnerStaleKilled),
            other => Err(StateParseError::RuntimeLivenessStatus(other.into())),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IssueStateRecord {
    pub project_id: String,
    pub issue_id: String,
    pub identifier: String,
    pub title: String,
    pub lifecycle_stage: LifecycleStage,
    pub blocker: Option<BlockerRecord>,
    pub failure: Option<FailureRecord>,
    pub git_ref: Option<GitRefRecord>,
    pub cleanup_status: CleanupStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunnerSessionRecord {
    pub project_id: String,
    pub issue_id: String,
    pub session_id: String,
    pub provider_mode: RuntimeProviderMode,
    pub provider_id: Option<String>,
    pub agent: String,
    pub model: Option<String>,
    pub worktree_path: String,
    pub process_id: Option<u32>,
    pub lifecycle_stage: LifecycleStage,
    pub stage: RunnerStage,
    pub active_agent: Option<String>,
    pub active_model: Option<String>,
    pub message_count: u64,
    pub todo_count: u64,
    pub part_count: u64,
    pub token_count: u64,
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub tokens_reasoning: u64,
    pub tokens_cache_read: u64,
    pub tokens_cache_write: u64,
    pub tokens_reported_total: u64,
    pub token_usage_status: String,
    pub token_usage_source: String,
    pub cost_micros: u64,
    pub subagent_count: u64,
    pub eval_stage: Option<String>,
    pub lifecycle_marker: Option<String>,
    pub last_event: Option<String>,
    pub runtime_failure_kind: Option<RuntimeFailureKind>,
    pub acp_frame_count: u64,
    pub session_evidence_refs: Vec<String>,
    pub silence_observed: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeProviderMode {
    Acp,
    OmpAcp,
}

impl RuntimeProviderMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Acp => "acp",
            Self::OmpAcp => "omp_acp",
        }
    }
}

impl fmt::Display for RuntimeProviderMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for RuntimeProviderMode {
    type Err = StateParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "acp" => Ok(Self::Acp),
            "omp_acp" => Ok(Self::OmpAcp),
            other => Err(StateParseError::RuntimeProviderMode(other.into())),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeFailureKind {
    MissingBinary,
    ProviderAuthUnavailable,
    MalformedAcpFrame,
    UnsupportedOmpVersion,
}

impl RuntimeFailureKind {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::MissingBinary => "missing_binary",
            Self::ProviderAuthUnavailable => "provider_auth_unavailable",
            Self::MalformedAcpFrame => "malformed_acp_frame",
            Self::UnsupportedOmpVersion => "unsupported_omp_version",
        }
    }
}

impl fmt::Display for RuntimeFailureKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for RuntimeFailureKind {
    type Err = StateParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "missing_binary" => Ok(Self::MissingBinary),
            "provider_auth_unavailable" => Ok(Self::ProviderAuthUnavailable),
            "malformed_acp_frame" => Ok(Self::MalformedAcpFrame),
            "unsupported_omp_version" => Ok(Self::UnsupportedOmpVersion),
            other => Err(StateParseError::RuntimeFailureKind(other.into())),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunnerStageEventRecord {
    pub project_id: String,
    pub issue_id: String,
    pub session_id: String,
    pub sequence: u64,
    pub stage: RunnerStage,
    pub event: Option<String>,
}

impl RunnerSessionRecord {
    pub fn failure_marker(&self) -> Option<&str> {
        if self.lifecycle_stage == LifecycleStage::Failed {
            self.lifecycle_marker.as_deref()
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvalRunRecord {
    pub project_id: String,
    pub issue_id: String,
    pub run_id: String,
    pub suite: String,
    pub status: String,
    pub details_json: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SelfDefectRecord {
    pub registry_id: String,
    pub fingerprint: String,
    pub defect_kind: String,
    pub category: String,
    pub severity: String,
    pub initial_routing_decision: String,
    pub source_project_id: String,
    pub source_issue_id: String,
    pub source_issue_identifier: String,
    pub source_session_id: Option<String>,
    pub source_process_id: Option<u32>,
    pub managed_issue_id: String,
    pub managed_issue_identifier: String,
    pub occurrence_count: u32,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub latest_evidence_summary: String,
    pub resolution_state: SelfDefectResolutionState,
    pub relation_mode: SelfDefectRelationMode,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SelfDefectOccurrenceRecord {
    pub fingerprint: String,
    pub defect_kind: String,
    pub category: String,
    pub severity: String,
    pub initial_routing_decision: String,
    pub source_project_id: String,
    pub source_issue_id: String,
    pub source_issue_identifier: String,
    pub source_session_id: Option<String>,
    pub source_process_id: Option<u32>,
    pub managed_issue_id: String,
    pub managed_issue_identifier: String,
    pub latest_evidence_summary: String,
    pub relation_mode: SelfDefectRelationMode,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SelfDefectRecommendationRecord {
    pub recommendation_id: String,
    pub evidence_fingerprint: String,
    pub defect_kind: String,
    pub defect_category: String,
    pub confidence: SelfDefectRecommendationConfidence,
    pub evidence_refs: Vec<String>,
    pub recommended_action: String,
    pub rationale: String,
    pub source_project_id: String,
    pub source_issue_id: String,
    pub source_issue_identifier: String,
    pub source_session_id: Option<String>,
    pub source_process_id: Option<u32>,
    pub occurrence_count: u32,
    pub first_seen_at: String,
    pub last_seen_at: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelfDefectRecommendationConfidence {
    Low,
    Medium,
    High,
}

impl SelfDefectRecommendationConfidence {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelfDefectResolutionState {
    Open,
    Done,
    Canceled,
}

impl SelfDefectResolutionState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Done => "done",
            Self::Canceled => "canceled",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelfDefectRelationMode {
    Blocking,
    RelatedOnly,
}

impl SelfDefectRelationMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Blocking => "blocking",
            Self::RelatedOnly => "related_only",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BlockerRecord {
    pub kind: String,
    pub message: String,
    #[serde(default)]
    pub observed_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FailureRecord {
    pub kind: String,
    pub message: String,
    #[serde(default)]
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub occurrence_count: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GitRefRecord {
    pub branch: String,
    pub worktree_path: String,
    pub head_sha: Option<String>,
    #[serde(default)]
    pub pr_url: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleStage {
    Queued,
    Running,
    Blocked,
    Completed,
    Canceled,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerStage {
    Starting,
    Running,
    Eval,
    Review,
    Handoff,
    Silent,
    Completed,
    Failed,
}

impl RunnerStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Eval => "eval",
            Self::Review => "review",
            Self::Handoff => "handoff",
            Self::Silent => "silent",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for RunnerStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for RunnerStage {
    type Err = StateParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "starting" => Ok(Self::Starting),
            "running" => Ok(Self::Running),
            "eval" => Ok(Self::Eval),
            "review" => Ok(Self::Review),
            "handoff" => Ok(Self::Handoff),
            "silent" => Ok(Self::Silent),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            other => Err(StateParseError::RunnerStage(other.into())),
        }
    }
}

impl LifecycleStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Blocked => "blocked",
            Self::Completed => "completed",
            Self::Canceled => "canceled",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for LifecycleStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for LifecycleStage {
    type Err = StateParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "blocked" => Ok(Self::Blocked),
            "completed" => Ok(Self::Completed),
            "canceled" => Ok(Self::Canceled),
            "failed" => Ok(Self::Failed),
            other => Err(StateParseError::LifecycleStage(other.into())),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupStatus {
    Clean,
    Pending,
    InProgress,
    Complete,
    Failed,
}

impl CleanupStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Complete => "complete",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for CleanupStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for CleanupStatus {
    type Err = StateParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "clean" => Ok(Self::Clean),
            "pending" => Ok(Self::Pending),
            "in_progress" => Ok(Self::InProgress),
            "complete" => Ok(Self::Complete),
            "failed" => Ok(Self::Failed),
            other => Err(StateParseError::CleanupStatus(other.into())),
        }
    }
}

#[derive(Debug, Error)]
pub enum StateParseError {
    #[error("unknown lifecycle stage `{0}`")]
    LifecycleStage(String),
    #[error("unknown runner stage `{0}`")]
    RunnerStage(String),
    #[error("unknown cleanup status `{0}`")]
    CleanupStatus(String),
    #[error("unknown runtime liveness status `{0}`")]
    RuntimeLivenessStatus(String),
    #[error("unknown runtime provider mode `{0}`")]
    RuntimeProviderMode(String),
    #[error("unknown runtime failure kind `{0}`")]
    RuntimeFailureKind(String),
}
