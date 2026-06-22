use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::state::{RunnerStage, RuntimeProviderMode};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerRuntimeConfig {
    pub command: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    pub agent: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub permission_policy: PermissionPolicy,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPolicy {
    Reject,
    Cancel,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunnerLaunchSpec {
    pub provider_mode: RuntimeProviderMode,
    pub provider_id: Option<String>,
    pub command: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env_allowlist: Vec<String>,
    pub worktree_root: Option<PathBuf>,
    pub issue_identifier: String,
    pub branch_name: String,
    pub repo_path: Option<PathBuf>,
    pub recall_workspace_root: Option<PathBuf>,
    pub base_ref: Option<String>,
    pub agent: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub prompt: String,
    pub permission_policy: PermissionPolicy,
}

pub(crate) const OMP_CLEANUP_MARKER_ENV: &str = "SYMPHONY_OMP_CLEANUP_MARKER";

impl RunnerLaunchSpec {
    pub(crate) fn omp_cleanup_marker(&self) -> Option<String> {
        if self.provider_mode != RuntimeProviderMode::OmpAcp {
            return None;
        }
        let provider_id = self.provider_id.as_deref()?;
        Some(format!(
            "provider={provider_id};issue={};cwd={}",
            self.issue_identifier,
            self.cwd.display()
        ))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunnerStartedSession {
    pub session_id: String,
    pub process_id: Option<u32>,
    pub acp_frame_count: u64,
    pub session_evidence_refs: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunnerProcessStarted {
    pub process_id: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunnerSessionCreated {
    pub session_id: String,
    pub process_id: Option<u32>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RunnerSessionEvent {
    pub stage: Option<RunnerStage>,
    pub active_agent: Option<String>,
    pub active_model: Option<String>,
    pub message_delta: u64,
    pub todo_delta: u64,
    pub part_delta: u64,
    pub token_delta: u64,
    pub cost_micros_delta: u64,
    pub subagent_delta: u64,
    pub eval_stage: Option<String>,
    pub lifecycle_marker: Option<String>,
    pub last_event: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerHandoff {
    pub session_id: String,
    pub lifecycle_stages: Vec<RunnerStage>,
    pub subagents: Vec<String>,
    pub eval_results: Vec<RunnerEvalResult>,
    pub changed_files: Vec<String>,
    pub git: Option<GitClosureEvidence>,
    pub risks: Vec<String>,
    pub stop_reason: RunnerStopReason,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerEvalResult {
    pub suite: String,
    pub passed: bool,
    pub failure_fingerprint: Option<String>,
    pub details: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_ref: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GitClosureEvidence {
    pub branch: String,
    pub head_sha: Option<String>,
    pub pr_url: Option<String>,
    pub worktree_path: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum RunnerStopReason {
    Success,
    EvalFailed { failure_fingerprint: String },
    ProviderBlocker { message: String },
    AuthBlocker { message: String },
    UnsupportedOmpSurface { message: String },
    OwnerQuestion { question: String },
}
