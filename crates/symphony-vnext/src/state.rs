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
pub struct IssueStateRecord {
    pub project_id: String,
    pub issue_id: String,
    pub identifier: String,
    pub title: String,
    pub state: String,
    pub lifecycle_stage: LifecycleStage,
    pub blocker: Option<BlockerRecord>,
    pub failure: Option<FailureRecord>,
    pub git_ref: Option<GitRefRecord>,
    pub cleanup_status: CleanupStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OpenCodeSessionRecord {
    pub project_id: String,
    pub issue_id: String,
    pub session_id: String,
    pub agent: String,
    pub model: Option<String>,
    pub worktree_path: String,
    pub lifecycle_stage: LifecycleStage,
    pub stage: OpenCodeStage,
    pub active_agent: Option<String>,
    pub active_model: Option<String>,
    pub message_count: u64,
    pub todo_count: u64,
    pub part_count: u64,
    pub token_count: u64,
    pub cost_micros: u64,
    pub subagent_count: u64,
    pub eval_stage: Option<String>,
    pub lifecycle_marker: Option<String>,
    pub last_event: Option<String>,
    pub silence_observed: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OpenCodeStageEventRecord {
    pub project_id: String,
    pub issue_id: String,
    pub session_id: String,
    pub sequence: u64,
    pub stage: OpenCodeStage,
    pub event: Option<String>,
}

impl OpenCodeSessionRecord {
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
pub struct BlockerRecord {
    pub kind: String,
    pub message: String,
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
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenCodeStage {
    Starting,
    Running,
    Eval,
    Review,
    Handoff,
    Silent,
    Completed,
    Failed,
}

impl OpenCodeStage {
    pub fn as_str(self) -> &'static str {
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

impl fmt::Display for OpenCodeStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for OpenCodeStage {
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
            other => Err(StateParseError::OpenCodeStage(other.into())),
        }
    }
}

impl LifecycleStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Blocked => "blocked",
            Self::Completed => "completed",
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
    pub fn as_str(self) -> &'static str {
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
    #[error("unknown OpenCode stage `{0}`")]
    OpenCodeStage(String),
    #[error("unknown cleanup status `{0}`")]
    CleanupStatus(String),
}
