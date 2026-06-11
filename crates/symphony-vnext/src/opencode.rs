mod acp;
mod archive;
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
    acp_request, drain_acp_stream, extract_session_id, read_acp_response, session_new_params,
    session_resume_params, set_session_config_option, write_acp_request,
};
pub use archive::{
    OpenCodeSessionActivity, OpenCodeSessionArchiveReport, OpenCodeSessionArchiveRequest,
    OpenCodeSessionTreeActivity, OpenCodeSessionTreeMetrics, OpenCodeTimelineEvent,
    OpenCodeTodoActivity, archive_and_delete_session_tree, read_session_tree_activity,
    read_session_tree_metrics,
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
        _spec: &OpenCodeLaunchSpec,
        session: &OpenCodeSessionRecord,
        _failure_fingerprint: &str,
        _repair_message: &str,
    ) -> Result<OpenCodeStartedSession, OpenCodeError> {
        Ok(OpenCodeStartedSession {
            session_id: session.session_id.clone(),
            process_id: session.process_id,
        })
    }

    async fn continue_session(
        &self,
        _spec: &OpenCodeLaunchSpec,
        session: &OpenCodeSessionRecord,
        _continuation_message: &str,
    ) -> Result<OpenCodeStartedSession, OpenCodeError> {
        Ok(OpenCodeStartedSession {
            session_id: session.session_id.clone(),
            process_id: session.process_id,
        })
    }

    async fn resume(
        &self,
        _spec: &OpenCodeLaunchSpec,
        session: &OpenCodeSessionRecord,
    ) -> Result<OpenCodeStartedSession, OpenCodeError> {
        Ok(OpenCodeStartedSession {
            session_id: session.session_id.clone(),
            process_id: session.process_id,
        })
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
            process_id: None,
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
        let process_id = child.id();
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

        Ok(OpenCodeStartedSession {
            session_id,
            process_id,
        })
    }

    async fn resume(
        &self,
        spec: &OpenCodeLaunchSpec,
        session: &OpenCodeSessionRecord,
    ) -> Result<OpenCodeStartedSession, OpenCodeError> {
        info!(
            issue = %spec.issue_identifier,
            session_id = %session.session_id,
            cwd = %spec.cwd.display(),
            command = %spec.command.display(),
            "resuming OpenCode ACP session"
        );
        ensure_worktree(spec).await?;
        let mut child = Command::new(&spec.command)
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let process_id = child.id();
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

        let resume_result = acp_request(
            &mut stdin,
            &mut stdout,
            &spec.permission_policy,
            next_id,
            "session/resume",
            session_resume_params(spec, session),
        )
        .await?;
        next_id += 1;
        let resumed_session_id =
            extract_session_id(&resume_result).unwrap_or_else(|_| session.session_id.clone());
        if resumed_session_id != session.session_id {
            return Err(OpenCodeError::AcpProtocol(format!(
                "ACP session/resume returned `{resumed_session_id}` for `{}`",
                session.session_id
            )));
        }
        set_session_config_option(
            &mut stdin,
            &mut stdout,
            &spec.permission_policy,
            next_id,
            &session.session_id,
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
            &session.session_id,
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
            &session.session_id,
            "effort",
            spec.effort.as_deref(),
        )
        .await?;

        let permission_policy = spec.permission_policy.clone();
        tokio::spawn(async move {
            if let Err(error) = drain_acp_stream(stdout, stdin, permission_policy).await {
                warn!(error = %error, "OpenCode ACP resumed stream ended with error");
            }
            let _ = child.wait().await;
        });

        Ok(OpenCodeStartedSession {
            session_id: session.session_id.clone(),
            process_id,
        })
    }

    async fn continue_repair(
        &self,
        spec: &OpenCodeLaunchSpec,
        session: &OpenCodeSessionRecord,
        failure_fingerprint: &str,
        repair_message: &str,
    ) -> Result<OpenCodeStartedSession, OpenCodeError> {
        info!(
            issue = %spec.issue_identifier,
            session_id = %session.session_id,
            cwd = %spec.cwd.display(),
            command = %spec.command.display(),
            failure_fingerprint,
            "continuing OpenCode ACP repair"
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
        let process_id = child.id();
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

        let resume_result = acp_request(
            &mut stdin,
            &mut stdout,
            &spec.permission_policy,
            next_id,
            "session/resume",
            session_resume_params(spec, session),
        )
        .await?;
        next_id += 1;
        let resumed_session_id =
            extract_session_id(&resume_result).unwrap_or_else(|_| session.session_id.clone());
        if resumed_session_id != session.session_id {
            return Err(OpenCodeError::AcpProtocol(format!(
                "ACP session/resume returned `{resumed_session_id}` for `{}`",
                session.session_id
            )));
        }
        set_session_config_option(
            &mut stdin,
            &mut stdout,
            &spec.permission_policy,
            next_id,
            &session.session_id,
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
            &session.session_id,
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
            &session.session_id,
            "effort",
            spec.effort.as_deref(),
        )
        .await?;
        next_id += 1;

        let prompt_request_id = next_id;
        let prompt = format!(
            "Symphony repair required for existing ACP session `{}`.\n\n\
             Failure fingerprint: `{}`\n\n\
             Repair details:\n{}\n\n\
             Validation policy:\n{}\n\n\
             Continue the same implementation session. Do not start a new task. \
             Fix the implementation or handoff, rerun the required validation, \
             and rewrite the structured Symphony handoff JSON at the configured sidecar path.",
            session.session_id,
            failure_fingerprint,
            repair_message,
            validation_policy_text()
        );
        write_acp_request(
            &mut stdin,
            prompt_request_id,
            "session/prompt",
            json!({
                "sessionId": session.session_id.as_str(),
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
                warn!(error = %error, "OpenCode ACP repair prompt stream ended with error");
            }
            let _ = child.wait().await;
        });

        Ok(OpenCodeStartedSession {
            session_id: session.session_id.clone(),
            process_id,
        })
    }

    async fn continue_session(
        &self,
        spec: &OpenCodeLaunchSpec,
        session: &OpenCodeSessionRecord,
        continuation_message: &str,
    ) -> Result<OpenCodeStartedSession, OpenCodeError> {
        info!(
            issue = %spec.issue_identifier,
            session_id = %session.session_id,
            cwd = %spec.cwd.display(),
            command = %spec.command.display(),
            "continuing OpenCode ACP session"
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
        let process_id = child.id();
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

        let resume_result = acp_request(
            &mut stdin,
            &mut stdout,
            &spec.permission_policy,
            next_id,
            "session/resume",
            session_resume_params(spec, session),
        )
        .await?;
        next_id += 1;
        let resumed_session_id =
            extract_session_id(&resume_result).unwrap_or_else(|_| session.session_id.clone());
        if resumed_session_id != session.session_id {
            return Err(OpenCodeError::AcpProtocol(format!(
                "ACP session/resume returned `{resumed_session_id}` for `{}`",
                session.session_id
            )));
        }
        set_session_config_option(
            &mut stdin,
            &mut stdout,
            &spec.permission_policy,
            next_id,
            &session.session_id,
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
            &session.session_id,
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
            &session.session_id,
            "effort",
            spec.effort.as_deref(),
        )
        .await?;
        next_id += 1;

        let prompt_request_id = next_id;
        let prompt = format!(
            "Symphony continuation required for existing ACP session `{}`.\n\n\
             Continue the same implementation session. Do not start a new task. \
             Do not repeat already completed work unless validation requires it.\n\n\
             Validation policy:\n{}\n\n{}",
            session.session_id,
            validation_policy_text(),
            continuation_message
        );
        write_acp_request(
            &mut stdin,
            prompt_request_id,
            "session/prompt",
            json!({
                "sessionId": session.session_id.as_str(),
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
                warn!(error = %error, "OpenCode ACP continuation prompt stream ended with error");
            }
            let _ = child.wait().await;
        });

        Ok(OpenCodeStartedSession {
            session_id: session.session_id.clone(),
            process_id,
        })
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
        process_id: started.process_id,
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

pub fn apply_session_tree_metrics(
    session: &mut OpenCodeSessionRecord,
    metrics: &OpenCodeSessionTreeMetrics,
) {
    if metrics.message_count > 0 || metrics.part_count > 0 || metrics.todo_count > 0 {
        session.stage = match session.stage {
            OpenCodeStage::Starting | OpenCodeStage::Silent => OpenCodeStage::Running,
            stage => stage,
        };
        session.silence_observed = false;
    }
    session.active_agent = metrics
        .active_agent
        .clone()
        .or(session.active_agent.clone());
    session.active_model = metrics
        .active_model
        .clone()
        .or(session.active_model.clone());
    session.message_count = metrics.message_count;
    session.todo_count = metrics.todo_count;
    session.part_count = metrics.part_count;
    session.token_count = metrics.tokens_total;
    session.cost_micros = metrics.cost_micros;
    session.subagent_count = metrics.subagent_count;
    session.lifecycle_marker = Some("opencode_db_activity".into());
    session.last_event = metrics
        .last_updated_ms
        .map(|updated| format!("opencode_db_updated:{updated}"))
        .or_else(|| Some("opencode_db_snapshot".into()));
}

pub fn apply_session_tree_metrics_preserving_marker(
    session: &mut OpenCodeSessionRecord,
    metrics: &OpenCodeSessionTreeMetrics,
    previous_last_event: Option<&str>,
    previous_marker: Option<&str>,
) {
    apply_session_tree_metrics(session, metrics);
    if session.last_event.as_deref() == previous_last_event {
        session.lifecycle_marker = previous_marker.map(ToOwned::to_owned);
    }
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
         Eval default suite: {eval_suite} (fallback metadata, not a blanket workspace gate)\n\
         Linear state: {state}\n\
         URL: {url}\n\n\
         Validation policy:\n\
         {validation_policy}\n\n\
         On completion, write the structured Symphony handoff JSON to:\n\
         {handoff_path}\n\n\
         The handoff file must be valid JSON matching this exact shape:\n\
         {{\n\
           \"session_id\": \"{session_id}\",\n\
           \"lifecycle_stages\": [\"starting\", \"running\", \"eval\", \"handoff\", \"completed\"],\n\
           \"subagents\": [\"agent-name:session-id\"],\n\
           \"eval_results\": [{{\"suite\": \"suite-name\", \"passed\": true, \"failure_fingerprint\": null, \"details\": \"command outcomes\"}}],\n\
           \"changed_files\": [\"path:start-end\"],\n\
           \"git\": {{\"branch\": \"branch-name\", \"head_sha\": \"commit-sha\", \"pr_url\": null, \"worktree_path\": \"{worktree}\"}},\n\
           \"risks\": [\"remaining risk or omitted validation\"],\n\
           \"stop_reason\": {{\"type\": \"success\"}}\n\
         }}\n\
         For eval failures use \"stop_reason\": {{\"type\":\"eval_failed\",\"failure_fingerprint\":\"stable-id\"}}.\n\
         For provider or owner blockers use \"provider_blocker\" or \"owner_question\" with \"message\"/\"question\".\n\
         Do not use status, subagents_used, object-shaped eval_results, or string stop_reason values.\n\n\
         Full issue spec:\n{description}\n",
        identifier = issue.identifier,
        session_id = "the ACP session id",
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
        validation_policy = validation_policy_text(),
    )
}

fn validation_policy_text() -> &'static str {
    "- Treat the issue's Validation section as the authority for required commands.\n\
     - Scope validation to the changed surface and the issue's explicit acceptance criteria.\n\
     - For docs-only/no-code changes, run documentation/file-level validation such as git diff --check and reference checks; do not run cargo nextest --workspace, full workspace tests, or release gates unless the issue explicitly requires them.\n\
     - For Rust source changes, prefer the narrowest package/filter/profile that covers the changed behavior before escalating to workspace-level checks.\n\
     - If a broader check is intentionally skipped, record the reason in eval_results.details and risks."
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
    #[error("opencode sqlite error: {0}")]
    Sqlite(#[from] libsql::Error),
    #[error("opencode json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("opencode archive error: {0}")]
    Archive(String),
}
