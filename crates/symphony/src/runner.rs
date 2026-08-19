mod acp;
mod adapter;
mod archive;
mod handoff_sidecar;
mod lifecycle;
mod omp;
mod omp_metrics;
mod prompt;
mod session_metrics;
mod types;
mod worktree;

use std::{io::ErrorKind, path::Path, process::ExitStatus};

use serde_json::{Value, json};
use thiserror::Error;
use tokio::{
    io::{AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
    task::JoinHandle,
};
use tracing::{debug, info, warn};

use crate::{
    config::{OhMyPiAcpCwdPolicy, OhMyPiAcpProviderConfig, ProjectConfig, WorkflowStage},
    linear::LinearIssue,
    state::{LifecycleStage, RunnerSessionRecord, RunnerStage, RuntimeProviderMode},
};
use acp::{
    acp_request, drain_acp_stream, extract_session_id, read_acp_response,
    set_session_config_option, write_acp_request,
};
use adapter::AgentExecutionAdapter;
pub use archive::{
    RunnerSessionActivity, RunnerSessionArchiveReport, RunnerSessionArchiveRequest,
    RunnerSessionMessageError, RunnerSessionTreeActivity, RunnerSessionTreeMetrics,
    RunnerTimelineEvent, RunnerTodoActivity, archive_and_delete_session_tree,
    read_latest_session_tree_error, read_session_tree_activity, read_session_tree_metrics,
};
use lifecycle::AcpChildLifecycle;
pub use lifecycle::ProcessTreeTerminationEvidence;
pub(crate) use lifecycle::terminate_process_tree;
pub use omp::{OmpAcpTelemetry, classify_omp_acp_failure_kind};
pub use omp_metrics::{read_omp_session_tree_activity, read_omp_session_tree_metrics};
use prompt::{
    build_stage_invocation_prompt, commit_policy_text, delegated_subagent_contract_text,
    mcp_tool_loop_guard_text, validation_policy_text,
};
pub use session_metrics::{
    apply_omp_session_tree_metrics, apply_session_tree_metrics,
    apply_session_tree_metrics_preserving_marker, ingest_session_event, mark_session_silence,
};
pub(crate) use types::OMP_CLEANUP_MARKER_ENV;
pub use types::{
    GitClosureEvidence, PermissionPolicy, RunnerEvalResult, RunnerHandoff, RunnerLaunchSpec,
    RunnerProcessStarted, RunnerRuntimeConfig, RunnerSessionCreated, RunnerSessionEvent,
    RunnerStartedSession, RunnerStopReason,
};
pub use worktree::worktree_path_allowed;
use worktree::{
    ensure_resumable_worktree, ensure_worktree, handoff_sidecar_path, launch_uses_issue_worktree,
    remove_stale_handoff_sidecar,
};

#[async_trait::async_trait]
pub trait RunnerLaunchObserver: Sync {
    async fn process_started(&self, _event: RunnerProcessStarted) -> Result<(), RunnerError> {
        Ok(())
    }

    async fn session_created(&self, _event: RunnerSessionCreated) -> Result<(), RunnerError> {
        Ok(())
    }
}

struct NoopRunnerLaunchObserver;

#[async_trait::async_trait]
impl RunnerLaunchObserver for NoopRunnerLaunchObserver {}

#[async_trait::async_trait]
pub trait RunnerLauncher: Sync {
    async fn launch(&self, spec: &RunnerLaunchSpec) -> Result<RunnerStartedSession, RunnerError>;

    async fn launch_observed(
        &self,
        spec: &RunnerLaunchSpec,
        observer: &dyn RunnerLaunchObserver,
    ) -> Result<RunnerStartedSession, RunnerError> {
        let started = self.launch(spec).await?;
        observer
            .session_created(RunnerSessionCreated {
                session_id: started.session_id.clone(),
                process_id: started.process_id,
            })
            .await?;
        Ok(started)
    }

    async fn latest_handoff(
        &self,
        _session: &RunnerSessionRecord,
    ) -> Result<Option<RunnerHandoff>, RunnerError> {
        Ok(None)
    }

    async fn continue_repair(
        &self,
        _spec: &RunnerLaunchSpec,
        session: &RunnerSessionRecord,
        _failure_fingerprint: &str,
        _repair_message: &str,
    ) -> Result<RunnerStartedSession, RunnerError> {
        Ok(RunnerStartedSession {
            session_id: session.session_id.clone(),
            process_id: session.process_id,
            acp_frame_count: session.acp_frame_count,
            session_evidence_refs: session.session_evidence_refs.clone(),
        })
    }

    async fn continue_session(
        &self,
        _spec: &RunnerLaunchSpec,
        session: &RunnerSessionRecord,
        _continuation_message: &str,
    ) -> Result<RunnerStartedSession, RunnerError> {
        Ok(RunnerStartedSession {
            session_id: session.session_id.clone(),
            process_id: session.process_id,
            acp_frame_count: session.acp_frame_count,
            session_evidence_refs: session.session_evidence_refs.clone(),
        })
    }

    async fn resume(
        &self,
        _spec: &RunnerLaunchSpec,
        session: &RunnerSessionRecord,
    ) -> Result<RunnerStartedSession, RunnerError> {
        Ok(RunnerStartedSession {
            session_id: session.session_id.clone(),
            process_id: session.process_id,
            acp_frame_count: session.acp_frame_count,
            session_evidence_refs: session.session_evidence_refs.clone(),
        })
    }
}

#[derive(Debug, Default)]
pub struct DeterministicRunnerLauncher;

#[async_trait::async_trait]
impl RunnerLauncher for DeterministicRunnerLauncher {
    async fn launch(&self, spec: &RunnerLaunchSpec) -> Result<RunnerStartedSession, RunnerError> {
        Ok(RunnerStartedSession {
            session_id: deterministic_session_id(&spec.cwd.display().to_string()),
            process_id: None,
            acp_frame_count: 0,
            session_evidence_refs: Vec::new(),
        })
    }
}

#[derive(Debug, Default)]
pub struct StdioRunnerLauncher;

async fn initialize_acp_child(
    child: &mut AcpChildLifecycle,
    spec: &RunnerLaunchSpec,
    request_id: u64,
) -> Result<(), RunnerError> {
    let (stdin, stdout) = child.io();
    let adapter = AgentExecutionAdapter::for_spec(spec);
    acp_request(
        stdin,
        stdout,
        &spec.permission_policy,
        request_id,
        "initialize",
        adapter.initialize_params(spec),
    )
    .await?;
    Ok(())
}

async fn configure_acp_session(
    child: &mut AcpChildLifecycle,
    spec: &RunnerLaunchSpec,
    session_id: &str,
    next_id: &mut u64,
) -> Result<(), RunnerError> {
    let adapter = AgentExecutionAdapter::for_spec(spec);
    for option in adapter.config_options(spec) {
        let (stdin, stdout) = child.io();
        set_session_config_option(
            stdin,
            stdout,
            &spec.permission_policy,
            *next_id,
            session_id,
            option.id,
            option.value,
        )
        .await?;
        *next_id += 1;
    }
    Ok(())
}

async fn resume_acp_session(
    child: &mut AcpChildLifecycle,
    spec: &RunnerLaunchSpec,
    session: &RunnerSessionRecord,
    request_id: u64,
) -> Result<(), RunnerError> {
    let (stdin, stdout) = child.io();
    let resume_result = acp_request(
        stdin,
        stdout,
        &spec.permission_policy,
        request_id,
        "session/resume",
        AgentExecutionAdapter::for_spec(spec).session_resume_params(spec, &session.session_id),
    )
    .await?;
    let resumed_session_id =
        extract_session_id(&resume_result).unwrap_or_else(|_| session.session_id.clone());
    if resumed_session_id != session.session_id {
        return Err(RunnerError::AcpProtocol(format!(
            "ACP session/resume returned `{resumed_session_id}` for `{}`",
            session.session_id
        )));
    }
    Ok(())
}

fn spawn_prompt_reader(
    permission_policy: &PermissionPolicy,
    prompt_request_id: u64,
    warning: &'static str,
    session_id: String,
    worktree_path: std::path::PathBuf,
    mut child: Child,
    mut stdin: ChildStdin,
    mut stdout: BufReader<ChildStdout>,
    stderr_drain: JoinHandle<()>,
) {
    let permission_policy = permission_policy.clone();
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
            handle_reader_error(error, warning, &mut child).await;
        }
        let _ = child.wait().await;
        write_missing_handoff_sidecar_blocker_if_absent(&worktree_path, &session_id, warning).await;
        stderr_drain.abort();
    });
}

fn prompt_with_session_binding(prompt: &str, session_id: &str) -> String {
    format!(
        "Symphony active session binding\n\
         - Active Symphony ACP session: `{session_id}`\n\
         - The structured handoff sidecar MUST set JSON field `session_id` to exactly `{session_id}`.\n\
         - Do not use issue identifiers, branch names, semantic labels, or invented session ids.\n\n\
         {prompt}"
    )
}

fn spawn_stream_drain(
    permission_policy: &PermissionPolicy,
    warning: &'static str,
    session_id: String,
    worktree_path: std::path::PathBuf,
    mut child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr_drain: JoinHandle<()>,
) {
    let permission_policy = permission_policy.clone();
    tokio::spawn(async move {
        if let Err(error) = drain_acp_stream(stdout, stdin, permission_policy).await {
            handle_reader_error(error, warning, &mut child).await;
        }
        let _ = child.wait().await;
        write_missing_handoff_sidecar_blocker_if_absent(&worktree_path, &session_id, warning).await;
        stderr_drain.abort();
    });
}

async fn write_missing_handoff_sidecar_blocker_if_absent(
    worktree_path: &Path,
    session_id: &str,
    reason: &str,
) {
    let path = handoff_sidecar_path(worktree_path);

    let handoff = RunnerHandoff {
        session_id: session_id.to_owned(),
        lifecycle_stages: vec![RunnerStage::Running, RunnerStage::Failed],
        subagents: Vec::new(),
        eval_results: Vec::new(),
        changed_files: Vec::new(),
        git: None,
        risks: vec![reason.to_owned()],
        stop_reason: RunnerStopReason::ProviderBlocker {
            message:
                ".symphony/runner-handoff.json was not produced before the runner ACP process ended"
                    .into(),
        },
    };
    let payload = match serde_json::to_vec_pretty(&handoff) {
        Ok(payload) => payload,
        Err(error) => {
            warn!(error = %error, "could not serialize missing handoff sidecar blocker");
            return;
        }
    };
    if let Some(parent) = path.parent()
        && let Err(error) = tokio::fs::create_dir_all(parent).await
    {
        warn!(
            error = %error,
            path = %parent.display(),
            "could not create runner handoff sidecar directory"
        );
        return;
    }
    match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .await
    {
        Ok(mut file) => {
            if let Err(error) = file.write_all(&payload).await {
                warn!(
                    error = %error,
                    path = %path.display(),
                    "could not write missing handoff sidecar blocker"
                );
            }
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
        Err(error) => {
            warn!(
                error = %error,
                path = %path.display(),
                "could not create missing handoff sidecar blocker"
            );
        }
    }
}

async fn handle_reader_error(error: RunnerError, warning: &'static str, child: &mut Child) {
    match child.try_wait() {
        Ok(Some(status)) if process_exit_was_managed_shutdown(status) => {
            debug!(
                error = %error,
                status = %status,
                message = warning,
                "runner ACP reader stopped after managed process shutdown"
            );
        }
        Ok(Some(status)) => {
            warn!(
                error = %error,
                status = %status,
                message = warning,
                "runner ACP reader ended after process exit with error"
            );
        }
        Ok(None) => {
            warn!(error = %error, message = warning, "runner ACP reader ended with error");
            if let Some(process_id) = child.id() {
                let _ = terminate_process_tree(process_id, warning).await;
            }
            let _ = child.kill().await;
        }
        Err(wait_error) => {
            warn!(
                error = %error,
                wait_error = %wait_error,
                message = warning,
                "runner ACP reader could not inspect process after stream error"
            );
            if let Some(process_id) = child.id() {
                let _ = terminate_process_tree(process_id, warning).await;
            }
            let _ = child.kill().await;
        }
    }
}

fn process_exit_was_managed_shutdown(status: ExitStatus) -> bool {
    if status.success() {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        matches!(status.signal(), Some(9 | 15))
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[async_trait::async_trait]
impl RunnerLauncher for StdioRunnerLauncher {
    async fn launch(&self, spec: &RunnerLaunchSpec) -> Result<RunnerStartedSession, RunnerError> {
        self.launch_observed(spec, &NoopRunnerLaunchObserver).await
    }

    async fn launch_observed(
        &self,
        spec: &RunnerLaunchSpec,
        observer: &dyn RunnerLaunchObserver,
    ) -> Result<RunnerStartedSession, RunnerError> {
        if spec.provider_mode == RuntimeProviderMode::OmpAcp {
            return omp::StdioOmpAcpLauncher
                .launch_observed(spec, observer)
                .await;
        }
        info!(
            issue = %spec.issue_identifier,
            cwd = %spec.cwd.display(),
            command = %spec.command.display(),
            agent = %spec.agent,
            model = spec.model.as_deref().unwrap_or("default"),
            "launching runner ACP session"
        );
        ensure_worktree(spec).await?;
        remove_stale_handoff_sidecar(&spec.cwd).await?;
        let mut child = AcpChildLifecycle::spawn(spec).await?;
        let process_id = child.process_id();
        if let Err(error) = observer
            .process_started(RunnerProcessStarted { process_id })
            .await
        {
            return Err(child
                .setup_failed(&spec.issue_identifier, None, error.to_string())
                .await);
        }

        let mut next_id = 1_u64;
        let setup = async {
            initialize_acp_child(&mut child, spec, next_id).await?;
            next_id += 1;

            let (stdin, stdout) = child.io();
            let session_result = acp_request(
                stdin,
                stdout,
                &spec.permission_policy,
                next_id,
                "session/new",
                AgentExecutionAdapter::for_spec(spec).session_new_params(spec),
            )
            .await?;
            next_id += 1;
            let session_id = extract_session_id(&session_result)?;
            info!(
                issue = %spec.issue_identifier,
                session_id = %session_id,
                cwd = %spec.cwd.display(),
                "runner ACP session created"
            );
            observer
                .session_created(RunnerSessionCreated {
                    session_id: session_id.clone(),
                    process_id,
                })
                .await?;
            configure_acp_session(&mut child, spec, &session_id, &mut next_id).await?;
            Ok::<String, RunnerError>(session_id)
        }
        .await;
        let session_id = match setup {
            Ok(session_id) => session_id,
            Err(error) => {
                return Err(child
                    .setup_failed(&spec.issue_identifier, None, error.to_string())
                    .await);
            }
        };
        let prompt_request_id = next_id;
        let prompt = prompt_with_session_binding(&spec.prompt, &session_id);
        write_acp_request(
            child.stdin(),
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
        let (process, stdin, stdout, stderr_drain) = child.into_parts();
        spawn_prompt_reader(
            &spec.permission_policy,
            prompt_request_id,
            "runner ACP prompt stream ended with error",
            session_id.clone(),
            spec.cwd.clone(),
            process,
            stdin,
            stdout,
            stderr_drain,
        );

        Ok(RunnerStartedSession {
            session_id,
            process_id,
            acp_frame_count: 4,
            session_evidence_refs: Vec::new(),
        })
    }

    async fn resume(
        &self,
        spec: &RunnerLaunchSpec,
        session: &RunnerSessionRecord,
    ) -> Result<RunnerStartedSession, RunnerError> {
        info!(
            issue = %spec.issue_identifier,
            session_id = %session.session_id,
            cwd = %spec.cwd.display(),
            command = %spec.command.display(),
            "resuming runner ACP session"
        );
        ensure_resumable_worktree(spec).await?;
        let mut child = AcpChildLifecycle::spawn(spec).await?;
        let process_id = child.process_id();

        let mut next_id = 1_u64;
        let setup = async {
            initialize_acp_child(&mut child, spec, next_id).await?;
            next_id += 1;
            resume_acp_session(&mut child, spec, session, next_id).await?;
            next_id += 1;
            configure_acp_session(&mut child, spec, &session.session_id, &mut next_id).await?;
            Ok::<(), RunnerError>(())
        }
        .await;
        if let Err(error) = setup {
            return Err(child
                .setup_failed(
                    &spec.issue_identifier,
                    Some(session.session_id.clone()),
                    error.to_string(),
                )
                .await);
        }
        let (process, stdin, stdout, stderr_drain) = child.into_parts();
        spawn_stream_drain(
            &spec.permission_policy,
            "runner ACP resumed stream ended with error",
            session.session_id.clone(),
            spec.cwd.clone(),
            process,
            stdin,
            stdout,
            stderr_drain,
        );

        Ok(RunnerStartedSession {
            session_id: session.session_id.clone(),
            process_id,
            acp_frame_count: 3,
            session_evidence_refs: session.session_evidence_refs.clone(),
        })
    }

    async fn continue_repair(
        &self,
        spec: &RunnerLaunchSpec,
        session: &RunnerSessionRecord,
        failure_fingerprint: &str,
        repair_message: &str,
    ) -> Result<RunnerStartedSession, RunnerError> {
        info!(
            issue = %spec.issue_identifier,
            session_id = %session.session_id,
            cwd = %spec.cwd.display(),
            command = %spec.command.display(),
            failure_fingerprint,
            "continuing runner ACP repair"
        );
        ensure_resumable_worktree(spec).await?;
        remove_stale_handoff_sidecar(&spec.cwd).await?;
        let mut child = AcpChildLifecycle::spawn(spec).await?;
        let process_id = child.process_id();

        let mut next_id = 1_u64;
        let setup = async {
            initialize_acp_child(&mut child, spec, next_id).await?;
            next_id += 1;
            resume_acp_session(&mut child, spec, session, next_id).await?;
            next_id += 1;
            configure_acp_session(&mut child, spec, &session.session_id, &mut next_id).await?;
            Ok::<(), RunnerError>(())
        }
        .await;
        if let Err(error) = setup {
            return Err(child
                .setup_failed(
                    &spec.issue_identifier,
                    Some(session.session_id.clone()),
                    error.to_string(),
                )
                .await);
        }

        let prompt_request_id = next_id;
        let prompt = prompt_with_session_binding(
            &repair_prompt(spec, failure_fingerprint, repair_message),
            &session.session_id,
        );
        write_acp_request(
            child.stdin(),
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
        let (process, stdin, stdout, stderr_drain) = child.into_parts();
        spawn_prompt_reader(
            &spec.permission_policy,
            prompt_request_id,
            "runner ACP repair prompt stream ended with error",
            session.session_id.clone(),
            spec.cwd.clone(),
            process,
            stdin,
            stdout,
            stderr_drain,
        );

        Ok(RunnerStartedSession {
            session_id: session.session_id.clone(),
            process_id,
            acp_frame_count: 3,
            session_evidence_refs: session.session_evidence_refs.clone(),
        })
    }

    async fn continue_session(
        &self,
        spec: &RunnerLaunchSpec,
        session: &RunnerSessionRecord,
        continuation_message: &str,
    ) -> Result<RunnerStartedSession, RunnerError> {
        info!(
            issue = %spec.issue_identifier,
            session_id = %session.session_id,
            cwd = %spec.cwd.display(),
            command = %spec.command.display(),
            "continuing runner ACP session"
        );
        ensure_resumable_worktree(spec).await?;
        remove_stale_handoff_sidecar(&spec.cwd).await?;
        let mut child = AcpChildLifecycle::spawn(spec).await?;
        let process_id = child.process_id();

        let mut next_id = 1_u64;
        let setup = async {
            initialize_acp_child(&mut child, spec, next_id).await?;
            next_id += 1;
            resume_acp_session(&mut child, spec, session, next_id).await?;
            next_id += 1;
            configure_acp_session(&mut child, spec, &session.session_id, &mut next_id).await?;
            Ok::<(), RunnerError>(())
        }
        .await;
        if let Err(error) = setup {
            return Err(child
                .setup_failed(
                    &spec.issue_identifier,
                    Some(session.session_id.clone()),
                    error.to_string(),
                )
                .await);
        }

        let prompt_request_id = next_id;
        let prompt = prompt_with_session_binding(
            &continuation_prompt(spec, continuation_message),
            &session.session_id,
        );
        write_acp_request(
            child.stdin(),
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
        let (process, stdin, stdout, stderr_drain) = child.into_parts();
        spawn_prompt_reader(
            &spec.permission_policy,
            prompt_request_id,
            "runner ACP continuation prompt stream ended with error",
            session.session_id.clone(),
            spec.cwd.clone(),
            process,
            stdin,
            stdout,
            stderr_drain,
        );

        Ok(RunnerStartedSession {
            session_id: session.session_id.clone(),
            process_id,
            acp_frame_count: 3,
            session_evidence_refs: session.session_evidence_refs.clone(),
        })
    }

    async fn latest_handoff(
        &self,
        session: &RunnerSessionRecord,
    ) -> Result<Option<RunnerHandoff>, RunnerError> {
        let path = handoff_sidecar_path(&session.worktree_path);
        if !tokio::fs::try_exists(&path).await? {
            debug!(
                session_id = %session.session_id,
                worktree_path = %session.worktree_path,
                "runner handoff sidecar absent"
            );
            return Ok(None);
        }

        let input = tokio::fs::read_to_string(&path).await?;
        let mut value: Value = serde_json::from_str(&input)
            .map_err(|error| RunnerError::MalformedHandoff(format!("{path:?}: {error}")))?;
        handoff_sidecar::normalize_handoff_sidecar_value(&mut value, &session.worktree_path);
        let handoff = serde_json::from_value(value)
            .map_err(|error| RunnerError::MalformedHandoff(format!("{path:?}: {error}")))?;
        info!(
            session_id = %session.session_id,
            path = %path.display(),
            "runner handoff sidecar loaded"
        );
        Ok(Some(handoff))
    }
}

pub fn build_acp_launch_spec(project: &ProjectConfig, issue: &LinearIssue) -> RunnerLaunchSpec {
    build_acp_launch_spec_for_stage(project, issue, WorkflowStage::InProgress)
}

pub fn build_acp_launch_spec_for_stage(
    project: &ProjectConfig,
    issue: &LinearIssue,
    stage: WorkflowStage,
) -> RunnerLaunchSpec {
    if project.runner.provider_mode == RuntimeProviderMode::OmpAcp {
        if let Some(provider) = project.omp_acp_providers.first() {
            return build_omp_acp_launch_spec_for_stage(project, issue, provider, stage);
        }
        warn!(
            project_id = %project.id,
            "runner.provider_mode=omp_acp has no configured provider; falling back to ACP"
        );
    }
    let branch_name = issue_branch_name(issue);
    let (agent, _, _) = workflow_agent_route_for_issue(project, issue, stage);
    let prompt = build_stage_invocation_prompt(project, issue, &branch_name, stage, &agent);
    RunnerLaunchSpec {
        provider_mode: RuntimeProviderMode::Acp,
        provider_id: None,
        command: project.runner.command.clone(),
        args: project.runner.args.clone(),
        cwd: project.branch.worktree_root.join(&issue.identifier),
        env_allowlist: Vec::new(),
        worktree_root: Some(project.branch.worktree_root.clone()),
        issue_identifier: issue.identifier.clone(),
        branch_name: branch_name.clone(),
        repo_path: Some(project.repo_path.clone()),
        recall_workspace_root: None,
        base_ref: Some(project.branch.base.clone()),
        agent,
        model: project.runner.model.clone(),
        effort: project.runner.effort.clone(),
        prompt,
        permission_policy: project.runner.permission_policy.clone(),
    }
}

pub fn build_omp_acp_launch_spec(
    project: &ProjectConfig,
    issue: &LinearIssue,
    provider: &OhMyPiAcpProviderConfig,
) -> RunnerLaunchSpec {
    build_omp_acp_launch_spec_for_stage(project, issue, provider, WorkflowStage::InProgress)
}

pub fn build_omp_acp_launch_spec_for_stage(
    project: &ProjectConfig,
    issue: &LinearIssue,
    provider: &OhMyPiAcpProviderConfig,
    stage: WorkflowStage,
) -> RunnerLaunchSpec {
    let branch_name = issue_branch_name(issue);
    let issue_worktree = project.branch.worktree_root.join(&issue.identifier);
    let cwd = match provider.cwd {
        OhMyPiAcpCwdPolicy::IssueWorktree => issue_worktree,
        OhMyPiAcpCwdPolicy::ProjectRepo => project.repo_path.clone(),
    };
    let (agent, _, _) = workflow_agent_route_for_issue(project, issue, stage);
    let prompt = build_stage_invocation_prompt(project, issue, &branch_name, stage, &agent);
    RunnerLaunchSpec {
        provider_mode: RuntimeProviderMode::OmpAcp,
        provider_id: Some(provider.id.clone()),
        command: provider.command.clone(),
        args: provider.args.clone(),
        cwd,
        env_allowlist: provider.env_allowlist.clone(),
        worktree_root: Some(project.branch.worktree_root.clone()),
        issue_identifier: issue.identifier.clone(),
        branch_name: branch_name.clone(),
        repo_path: Some(project.repo_path.clone()),
        recall_workspace_root: None,
        base_ref: Some(project.branch.base.clone()),
        agent,
        model: provider
            .model
            .clone()
            .or_else(|| project.runner.model.clone()),
        effort: provider
            .effort
            .clone()
            .or_else(|| project.runner.effort.clone()),
        prompt,
        permission_policy: project.runner.permission_policy.clone(),
    }
}

fn workflow_agent_route_for_issue(
    project: &ProjectConfig,
    issue: &LinearIssue,
    stage: WorkflowStage,
) -> (String, String, Option<String>) {
    project
        .workflow
        .agent_route_for_stage(stage, &issue.labels)
        .map(|route| {
            (
                route.selected_agent,
                route.reason.as_str().to_owned(),
                route.selected_label,
            )
        })
        .unwrap_or_else(|| (project.runner.agent.clone(), "fallback".into(), None))
}

fn repair_prompt(
    spec: &RunnerLaunchSpec,
    failure_fingerprint: &str,
    repair_message: &str,
) -> String {
    let provider_context = provider_context_text(spec);
    format!(
        "Symphony repair required for the current ACP session.\n\n\
         Failure fingerprint: `{failure_fingerprint}`\n\n\
         Repair details:\n{repair_message}\n\n\
         {provider_context}\
         MCP tool-schema loop guard:\n{mcp_tool_loop_guard}\n\n\
         Delegated review/evaluator subagent contract:\n{delegated_subagent_contract}\n\n\
         Validation policy:\n{validation_policy}\n\n\
         Commit policy for successful handoff:\n{commit_policy}\n\n\
         Continue the same implementation session. Do not start a new task. \
         Fix the implementation or handoff, rerun the required validation, \
         and rewrite the structured Symphony handoff JSON at the configured sidecar path.",
        mcp_tool_loop_guard = mcp_tool_loop_guard_text(),
        delegated_subagent_contract = delegated_subagent_contract_text(),
        validation_policy = validation_policy_text(),
        commit_policy = commit_policy_text()
    )
}

fn continuation_prompt(spec: &RunnerLaunchSpec, continuation_message: &str) -> String {
    let provider_context = provider_context_text(spec);
    format!(
        "Symphony continuation required for the current ACP session.\n\n\
         Continue the same implementation session. Do not start a new task. \
         Do not repeat already completed work unless validation requires it.\n\n\
         {provider_context}\
         MCP tool-schema loop guard:\n{mcp_tool_loop_guard}\n\n\
         Delegated review/evaluator subagent contract:\n{delegated_subagent_contract}\n\n\
         Validation policy:\n{validation_policy}\n\n\
         Commit policy for successful handoff:\n{commit_policy}\n\n{continuation_message}",
        mcp_tool_loop_guard = mcp_tool_loop_guard_text(),
        delegated_subagent_contract = delegated_subagent_contract_text(),
        validation_policy = validation_policy_text(),
        commit_policy = commit_policy_text()
    )
}

fn provider_context_text(spec: &RunnerLaunchSpec) -> String {
    match spec.provider_mode {
        RuntimeProviderMode::Acp => String::new(),
        RuntimeProviderMode::OmpAcp => String::new(),
    }
}

fn issue_branch_name(issue: &LinearIssue) -> String {
    issue
        .branch_name
        .as_deref()
        .filter(|branch| !branch.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("feature/{}", issue.identifier.to_ascii_lowercase()))
}

pub fn new_session_record(
    project: &ProjectConfig,
    issue: &LinearIssue,
    started: RunnerStartedSession,
    spec: &RunnerLaunchSpec,
) -> RunnerSessionRecord {
    new_session_record_for_stage(project, issue, started, spec, WorkflowStage::InProgress)
}

pub fn new_session_record_for_stage(
    project: &ProjectConfig,
    issue: &LinearIssue,
    started: RunnerStartedSession,
    spec: &RunnerLaunchSpec,
    stage: WorkflowStage,
) -> RunnerSessionRecord {
    let (_, agent_routing_reason, agent_routing_label) =
        workflow_agent_route_for_issue(project, issue, stage);
    RunnerSessionRecord {
        project_id: project.id.clone(),
        issue_id: issue.id.clone(),
        session_id: started.session_id,
        provider_mode: spec.provider_mode,
        provider_id: spec.provider_id.clone(),
        agent: spec.agent.clone(),
        agent_routing_reason,
        agent_routing_label,
        model: spec.model.clone(),
        worktree_path: spec.cwd.display().to_string(),
        process_id: started.process_id,
        lifecycle_stage: LifecycleStage::Running,
        stage: RunnerStage::Starting,
        active_agent: Some(spec.agent.clone()),
        active_model: spec.model.clone(),
        message_count: 0,
        todo_count: 0,
        part_count: 0,
        token_count: 0,
        tokens_input: 0,
        tokens_output: 0,
        tokens_reasoning: 0,
        tokens_cache_read: 0,
        tokens_cache_write: 0,
        tokens_reported_total: 0,
        token_usage_status: "missing".into(),
        token_usage_source: "none".into(),
        cost_micros: 0,
        subagent_count: 0,
        eval_stage: Some(project.eval.default_suite.clone()),
        lifecycle_marker: Some("acp_process_started".into()),
        last_event: Some("acp_process_started".into()),
        runtime_failure_kind: None,
        acp_frame_count: started.acp_frame_count,
        session_evidence_refs: started.session_evidence_refs,
        silence_observed: false,
    }
}

fn deterministic_session_id(input: &str) -> String {
    format!("runner:{input}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn prompt_reader_treats_owner_terminated_process_as_managed_shutdown() {
        use std::os::unix::process::ExitStatusExt;

        assert!(process_exit_was_managed_shutdown(ExitStatus::from_raw(0)));
        assert!(process_exit_was_managed_shutdown(ExitStatus::from_raw(15)));
        assert!(process_exit_was_managed_shutdown(ExitStatus::from_raw(9)));
        assert!(!process_exit_was_managed_shutdown(ExitStatus::from_raw(
            1 << 8
        )));
    }

    #[tokio::test]
    async fn writes_provider_blocker_handoff_when_prompt_reader_finishes_without_sidecar() {
        let dir = tempfile::tempdir().expect("tempdir");
        let worktree = dir.path();

        write_missing_handoff_sidecar_blocker_if_absent(
            worktree,
            "ses-missing-sidecar",
            "runner ACP prompt stream ended with error",
        )
        .await;

        let sidecar = handoff_sidecar_path(worktree);
        let input = tokio::fs::read_to_string(&sidecar)
            .await
            .expect("fallback sidecar");
        let handoff: RunnerHandoff = serde_json::from_str(&input).expect("typed fallback handoff");

        assert_eq!(handoff.session_id, "ses-missing-sidecar");
        assert_eq!(
            handoff.lifecycle_stages,
            vec![RunnerStage::Running, RunnerStage::Failed]
        );
        assert!(matches!(
            handoff.stop_reason,
            RunnerStopReason::ProviderBlocker { ref message }
                if message == ".symphony/runner-handoff.json was not produced before the runner ACP process ended"
        ));
        assert_eq!(handoff.risks, ["runner ACP prompt stream ended with error"]);
    }

    #[tokio::test]
    async fn missing_handoff_fallback_does_not_overwrite_existing_sidecar() {
        let dir = tempfile::tempdir().expect("tempdir");
        let worktree = dir.path();
        let sidecar = handoff_sidecar_path(worktree);
        tokio::fs::create_dir_all(sidecar.parent().expect("sidecar parent"))
            .await
            .expect("sidecar directory");
        tokio::fs::write(&sidecar, "real runner handoff")
            .await
            .expect("real sidecar");

        write_missing_handoff_sidecar_blocker_if_absent(
            worktree,
            "ses-fallback",
            "runner ACP prompt stream ended with error",
        )
        .await;

        let input = tokio::fs::read_to_string(&sidecar)
            .await
            .expect("sidecar contents");
        assert_eq!(input, "real runner handoff");
    }
}

#[derive(Debug, Error)]
pub enum RunnerError {
    #[error("runner io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("runner child stdin was not piped")]
    MissingStdin,
    #[error("runner child stdout was not piped")]
    MissingStdout,
    #[error("runner child stderr was not piped")]
    MissingStderr,
    #[error("runner ACP protocol error: {0}")]
    AcpProtocol(String),
    #[error(
        "runner ACP setup failed for {issue_identifier} pid={process_id:?} session={session_id:?}: {reason}; termination={termination:?}"
    )]
    AcpSetupFailed {
        issue_identifier: String,
        process_id: Option<u32>,
        session_id: Option<String>,
        reason: String,
        termination: Box<ProcessTreeTerminationEvidence>,
    },
    #[error("runtime provider failure ({kind}): {message}")]
    RuntimeFailure {
        kind: crate::state::RuntimeFailureKind,
        message: String,
    },
    #[error("runner process tree error: {0}")]
    ProcessTree(String),
    #[error("invalid runner worktree: {0}")]
    InvalidWorktree(String),
    #[error("git command failed: {command}: {stderr}")]
    GitCommand { command: String, stderr: String },
    #[error("malformed runner handoff: {0}")]
    MalformedHandoff(String),
    #[error("runner sqlite error: {0}")]
    Sqlite(#[from] libsql::Error),
    #[error("runner json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("runner archive error: {0}")]
    Archive(String),
    #[error("runner launch observer error: {0}")]
    LaunchObserver(String),
}
