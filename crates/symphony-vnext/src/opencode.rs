mod acp;
mod types;
mod worktree;

use serde_json::json;
use thiserror::Error;
use tokio::{io::BufReader, process::Command};
use tracing::{debug, info, warn};

use crate::{
    config::ProjectConfig,
    linear::LinearIssue,
    state::{LifecycleStage, OpenCodeSessionRecord, OpenCodeStage},
};
use acp::{
    acp_request, extract_session_id, read_acp_response, session_new_params,
    set_session_config_option, write_acp_request,
};
pub use types::{
    GitClosureEvidence, OpenCodeEvalResult, OpenCodeHandoff, OpenCodeLaunchSpec,
    OpenCodeRuntimeConfig, OpenCodeSessionEvent, OpenCodeStartedSession, OpenCodeStopReason,
    PermissionPolicy,
};
pub use worktree::worktree_path_allowed;
use worktree::{ensure_worktree, handoff_sidecar_path, remove_stale_handoff_sidecar};

#[async_trait::async_trait]
pub trait OpenCodeLauncher: Sync {
    async fn launch(
        &self,
        spec: &OpenCodeLaunchSpec,
    ) -> Result<OpenCodeStartedSession, OpenCodeError>;

    async fn latest_handoff(
        &self,
        _session: &OpenCodeSessionRecord,
    ) -> Result<Option<OpenCodeHandoff>, OpenCodeError> {
        Ok(None)
    }

    async fn continue_repair(
        &self,
        _session: &OpenCodeSessionRecord,
        _failure_fingerprint: &str,
    ) -> Result<(), OpenCodeError> {
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct DeterministicOpenCodeLauncher;

#[async_trait::async_trait]
impl OpenCodeLauncher for DeterministicOpenCodeLauncher {
    async fn launch(
        &self,
        spec: &OpenCodeLaunchSpec,
    ) -> Result<OpenCodeStartedSession, OpenCodeError> {
        Ok(OpenCodeStartedSession {
            session_id: deterministic_session_id(&spec.cwd.display().to_string()),
        })
    }
}

#[derive(Debug, Default)]
pub struct StdioOpenCodeLauncher;

#[async_trait::async_trait]
impl OpenCodeLauncher for StdioOpenCodeLauncher {
    async fn launch(
        &self,
        spec: &OpenCodeLaunchSpec,
    ) -> Result<OpenCodeStartedSession, OpenCodeError> {
        info!(
            issue = %spec.issue_identifier,
            cwd = %spec.cwd.display(),
            command = %spec.command.display(),
            agent = %spec.agent,
            model = spec.model.as_deref().unwrap_or("default"),
            "launching OpenCode ACP session"
        );
        ensure_worktree(spec).await?;
        remove_stale_handoff_sidecar(&spec.cwd).await?;
        let mut child = Command::new(&spec.command)
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().ok_or(OpenCodeError::MissingStdin)?;
        let stdout = child.stdout.take().ok_or(OpenCodeError::MissingStdout)?;
        let mut stdout = BufReader::new(stdout);

        let mut next_id = 1_u64;
        acp_request(
            &mut stdin,
            &mut stdout,
            &spec.permission_policy,
            next_id,
            "initialize",
            json!({
                "protocolVersion": 1,
                "agent": spec.agent,
                "model": spec.model,
            }),
        )
        .await?;
        next_id += 1;

        let session_result = acp_request(
            &mut stdin,
            &mut stdout,
            &spec.permission_policy,
            next_id,
            "session/new",
            session_new_params(spec),
        )
        .await?;
        next_id += 1;
        let session_id = extract_session_id(&session_result)?;
        info!(
            issue = %spec.issue_identifier,
            session_id = %session_id,
            cwd = %spec.cwd.display(),
            "OpenCode ACP session created"
        );
        set_session_config_option(
            &mut stdin,
            &mut stdout,
            &spec.permission_policy,
            next_id,
            &session_id,
            "mode",
            Some(spec.agent.as_str()),
        )
        .await?;
        next_id += 1;
        set_session_config_option(
            &mut stdin,
            &mut stdout,
            &spec.permission_policy,
            next_id,
            &session_id,
            "model",
            spec.model.as_deref(),
        )
        .await?;
        next_id += 1;
        set_session_config_option(
            &mut stdin,
            &mut stdout,
            &spec.permission_policy,
            next_id,
            &session_id,
            "effort",
            spec.effort.as_deref(),
        )
        .await?;
        next_id += 1;
        let prompt_request_id = next_id;
        let prompt = format!(
            "OpenCode ACP session id: {session_id}\n\n{task_prompt}",
            task_prompt = spec.prompt.as_str()
        );
        write_acp_request(
            &mut stdin,
            prompt_request_id,
            "session/prompt",
            json!({
                "sessionId": session_id.as_str(),
                "prompt": [
                    {
                        "type": "text",
                        "text": prompt.as_str(),
                    }
                ],
            }),
        )
        .await?;
        let permission_policy = spec.permission_policy.clone();

        tokio::spawn(async move {
            if let Err(error) = read_acp_response(
                &mut stdout,
                &mut stdin,
                &permission_policy,
                prompt_request_id,
                "session/prompt",
            )
            .await
            {
                warn!(error = %error, "OpenCode ACP prompt stream ended with error");
            }
            let _ = child.wait().await;
        });

        Ok(OpenCodeStartedSession { session_id })
    }

    async fn latest_handoff(
        &self,
        session: &OpenCodeSessionRecord,
    ) -> Result<Option<OpenCodeHandoff>, OpenCodeError> {
        let path = handoff_sidecar_path(&session.worktree_path);
        if !tokio::fs::try_exists(&path).await? {
            debug!(
                session_id = %session.session_id,
                worktree_path = %session.worktree_path,
                "OpenCode handoff sidecar absent"
            );
            return Ok(None);
        }

        let input = tokio::fs::read_to_string(&path).await?;
        let handoff = serde_json::from_str(&input)
            .map_err(|error| OpenCodeError::MalformedHandoff(format!("{path:?}: {error}")))?;
        info!(
            session_id = %session.session_id,
            path = %path.display(),
            "OpenCode handoff sidecar loaded"
        );
        Ok(Some(handoff))
    }
}

pub fn build_acp_launch_spec(project: &ProjectConfig, issue: &LinearIssue) -> OpenCodeLaunchSpec {
    OpenCodeLaunchSpec {
        command: project.opencode.command.clone(),
        args: project.opencode.args.clone(),
        cwd: project.branch.worktree_root.join(&issue.identifier),
        worktree_root: Some(project.branch.worktree_root.clone()),
        issue_identifier: issue.identifier.clone(),
        repo_path: Some(project.repo_path.clone()),
        base_ref: Some(project.branch.base.clone()),
        agent: project.opencode.agent.clone(),
        model: project.opencode.model.clone(),
        effort: project.opencode.effort.clone(),
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
         On completion, write the structured Symphony handoff JSON to:\n\
         {handoff_path}\n\n\
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
        handoff_path =
            handoff_sidecar_path(project.branch.worktree_root.join(&issue.identifier)).display(),
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
    #[error("opencode child stdout was not piped")]
    MissingStdout,
    #[error("opencode ACP protocol error: {0}")]
    AcpProtocol(String),
    #[error("invalid opencode worktree: {0}")]
    InvalidWorktree(String),
    #[error("git command failed: {command}: {stderr}")]
    GitCommand { command: String, stderr: String },
    #[error("malformed opencode handoff: {0}")]
    MalformedHandoff(String),
}
