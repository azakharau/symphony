use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    config::ProjectConfig,
    linear::LinearIssue,
    state::{LifecycleStage, OpenCodeSessionRecord, OpenCodeStage},
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeRuntimeConfig {
    pub command: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    pub agent: String,
    pub model: Option<String>,
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
    pub prompt: String,
    pub permission_policy: PermissionPolicy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenCodeStartedSession {
    pub session_id: String,
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

pub trait OpenCodeLauncher {
    fn launch(&self, spec: &OpenCodeLaunchSpec) -> Result<OpenCodeStartedSession, OpenCodeError>;

    fn latest_handoff(
        &self,
        _session: &OpenCodeSessionRecord,
    ) -> Result<Option<OpenCodeHandoff>, OpenCodeError> {
        Ok(None)
    }

    fn continue_repair(
        &self,
        _session: &OpenCodeSessionRecord,
        _failure_fingerprint: &str,
    ) -> Result<(), OpenCodeError> {
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct DeterministicOpenCodeLauncher;

impl OpenCodeLauncher for DeterministicOpenCodeLauncher {
    fn launch(&self, spec: &OpenCodeLaunchSpec) -> Result<OpenCodeStartedSession, OpenCodeError> {
        Ok(OpenCodeStartedSession {
            session_id: deterministic_session_id(&spec.cwd.display().to_string()),
        })
    }
}

#[derive(Debug, Default)]
pub struct StdioOpenCodeLauncher;

impl OpenCodeLauncher for StdioOpenCodeLauncher {
    fn launch(&self, spec: &OpenCodeLaunchSpec) -> Result<OpenCodeStartedSession, OpenCodeError> {
        fs::create_dir_all(&spec.cwd)?;
        let mut child = Command::new(&spec.command)
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().ok_or(OpenCodeError::MissingStdin)?;
        stdin.write_all(spec.prompt.as_bytes())?;
        drop(stdin);

        Ok(OpenCodeStartedSession {
            session_id: format!("pid:{}", child.id()),
        })
    }
}

pub fn build_acp_launch_spec(project: &ProjectConfig, issue: &LinearIssue) -> OpenCodeLaunchSpec {
    OpenCodeLaunchSpec {
        command: project.opencode.command.clone(),
        args: project.opencode.args.clone(),
        cwd: project.branch.worktree_root.join(&issue.identifier),
        prompt: build_issue_prompt(project, issue),
        permission_policy: project.opencode.permission_policy.clone(),
    }
}

pub fn new_session_record(
    project: &ProjectConfig,
    issue: &LinearIssue,
    started: OpenCodeStartedSession,
    spec: &OpenCodeLaunchSpec,
) -> OpenCodeSessionRecord {
    OpenCodeSessionRecord {
        project_id: project.id.clone(),
        issue_id: issue.id.clone(),
        session_id: started.session_id,
        agent: project.opencode.agent.clone(),
        model: project.opencode.model.clone(),
        worktree_path: spec.cwd.display().to_string(),
        lifecycle_stage: LifecycleStage::Running,
        stage: OpenCodeStage::Starting,
        active_agent: Some(project.opencode.agent.clone()),
        active_model: project.opencode.model.clone(),
        message_count: 0,
        todo_count: 0,
        part_count: 0,
        token_count: 0,
        cost_micros: 0,
        subagent_count: 0,
        eval_stage: Some(project.eval.default_suite.clone()),
        lifecycle_marker: Some("acp_started".into()),
        last_event: Some("acp_process_started".into()),
        silence_observed: false,
    }
}

pub fn ingest_session_event(session: &mut OpenCodeSessionRecord, event: OpenCodeSessionEvent) {
    if let Some(stage) = event.stage {
        session.stage = stage;
    }
    if let Some(agent) = event.active_agent {
        session.active_agent = Some(agent);
    }
    if let Some(model) = event.active_model {
        session.active_model = Some(model);
    }
    if let Some(eval_stage) = event.eval_stage {
        session.eval_stage = Some(eval_stage);
    }
    if let Some(marker) = event.lifecycle_marker {
        session.lifecycle_marker = Some(marker);
    }
    if let Some(last_event) = event.last_event {
        session.last_event = Some(last_event);
    }

    session.message_count = session.message_count.saturating_add(event.message_delta);
    session.todo_count = session.todo_count.saturating_add(event.todo_delta);
    session.part_count = session.part_count.saturating_add(event.part_delta);
    session.token_count = session.token_count.saturating_add(event.token_delta);
    session.cost_micros = session.cost_micros.saturating_add(event.cost_micros_delta);
    session.subagent_count = session.subagent_count.saturating_add(event.subagent_delta);
}

pub fn mark_session_silence(session: &mut OpenCodeSessionRecord, reason: &str) {
    session.stage = OpenCodeStage::Silent;
    session.silence_observed = true;
    session.last_event = Some(format!("silence:{reason}"));
}

fn build_issue_prompt(project: &ProjectConfig, issue: &LinearIssue) -> String {
    let description = issue
        .description
        .as_deref()
        .unwrap_or("No description provided.");
    format!(
        "Run OpenCode ACP for {identifier}: {title}\n\n\
         Project: {project_id}\n\
         Repository: {repo_path}\n\
         Isolated worktree: {worktree}\n\
         Eval default suite: {eval_suite}\n\
         Linear state: {state}\n\
         URL: {url}\n\n\
         Full issue spec:\n{description}\n",
        identifier = issue.identifier,
        title = issue.title,
        project_id = project.id,
        repo_path = project.repo_path.display(),
        worktree = project
            .branch
            .worktree_root
            .join(&issue.identifier)
            .display(),
        eval_suite = project.eval.default_suite,
        state = issue.state,
        url = issue.url.as_deref().unwrap_or("none"),
    )
}

fn deterministic_session_id(input: &str) -> String {
    format!("opencode:{input}")
}

#[derive(Debug, Error)]
pub enum OpenCodeError {
    #[error("opencode io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("opencode child stdin was not piped")]
    MissingStdin,
}
