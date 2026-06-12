use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::state::OpenCodeStage;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeRuntimeConfig {
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
pub struct OpenCodeLaunchSpec {
    pub command: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub worktree_root: Option<PathBuf>,
    pub issue_identifier: String,
    pub repo_path: Option<PathBuf>,
    pub mnemesh_workspace_root: Option<PathBuf>,
    pub base_ref: Option<String>,
    pub agent: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub prompt: String,
    pub permission_policy: PermissionPolicy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenCodeStartedSession {
    pub session_id: String,
    pub process_id: Option<u32>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OpenCodeSessionEvent {
    pub stage: Option<OpenCodeStage>,
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
pub struct OpenCodeHandoff {
    pub session_id: String,
    pub lifecycle_stages: Vec<OpenCodeStage>,
    pub subagents: Vec<String>,
    pub eval_results: Vec<OpenCodeEvalResult>,
    pub changed_files: Vec<String>,
    pub git: Option<GitClosureEvidence>,
    pub risks: Vec<String>,
    pub stop_reason: OpenCodeStopReason,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeEvalResult {
    pub suite: String,
    pub passed: bool,
    pub failure_fingerprint: Option<String>,
    pub details: Option<String>,
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
pub enum OpenCodeStopReason {
    Success,
    EvalFailed { failure_fingerprint: String },
    ProviderBlocker { message: String },
    OwnerQuestion { question: String },
}
