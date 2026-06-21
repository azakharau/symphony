use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::state::{OpenCodeStage, RuntimeProviderMode};

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

impl OpenCodeLaunchSpec {
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
pub struct OpenCodeStartedSession {
    pub session_id: String,
    pub process_id: Option<u32>,
    pub acp_frame_count: u64,
    pub session_evidence_refs: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenCodeProcessStarted {
    pub process_id: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenCodeSessionCreated {
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
pub enum OpenCodeStopReason {
    Success,
    EvalFailed { failure_fingerprint: String },
    ProviderBlocker { message: String },
    AuthBlocker { message: String },
    UnsupportedOmpSurface { message: String },
    OwnerQuestion { question: String },
}
