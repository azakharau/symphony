mod acp;
mod archive;
mod lifecycle;
mod prompt;
mod session_metrics;
mod types;
mod worktree;

use serde_json::json;
use thiserror::Error;
use tokio::{
    io::BufReader,
    process::{Child, ChildStdin, ChildStdout},
    task::JoinHandle,
};
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
use lifecycle::AcpChildLifecycle;
pub use lifecycle::ProcessTreeTerminationEvidence;
pub(crate) use lifecycle::terminate_process_tree;
use prompt::{
    build_issue_prompt, commit_policy_text, mcp_tool_loop_guard_text,
    mnemesh_workspace_contract_text, validation_policy_text,
};
pub use session_metrics::{
    apply_session_tree_metrics, apply_session_tree_metrics_preserving_marker, ingest_session_event,
    mark_session_silence,
};
pub use types::{
    GitClosureEvidence, OpenCodeEvalResult, OpenCodeHandoff, OpenCodeLaunchSpec,
    OpenCodeProcessStarted, OpenCodeRuntimeConfig, OpenCodeSessionCreated, OpenCodeSessionEvent,
    OpenCodeStartedSession, OpenCodeStopReason, PermissionPolicy,
};
pub use worktree::worktree_path_allowed;
use worktree::{
    ensure_resumable_worktree, ensure_worktree, handoff_sidecar_path, remove_stale_handoff_sidecar,
};

#[async_trait::async_trait]
pub trait OpenCodeLaunchObserver: Sync {
    async fn process_started(&self, _event: OpenCodeProcessStarted) -> Result<(), OpenCodeError> {
        Ok(())
    }

    async fn session_created(&self, _event: OpenCodeSessionCreated) -> Result<(), OpenCodeError> {
        Ok(())
    }
}

struct NoopOpenCodeLaunchObserver;

#[async_trait::async_trait]
impl OpenCodeLaunchObserver for NoopOpenCodeLaunchObserver {}

#[async_trait::async_trait]
pub trait OpenCodeLauncher: Sync {
    async fn launch(
        &self,
        spec: &OpenCodeLaunchSpec,
    ) -> Result<OpenCodeStartedSession, OpenCodeError>;

    async fn launch_observed(
        &self,
        spec: &OpenCodeLaunchSpec,
        observer: &dyn OpenCodeLaunchObserver,
    ) -> Result<OpenCodeStartedSession, OpenCodeError> {
        let started = self.launch(spec).await?;
        observer
            .session_created(OpenCodeSessionCreated {
                session_id: started.session_id.clone(),
                process_id: started.process_id,
            })
            .await?;
        Ok(started)
    }

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

async fn initialize_acp_child(
    child: &mut AcpChildLifecycle,
    spec: &OpenCodeLaunchSpec,
    request_id: u64,
) -> Result<(), OpenCodeError> {
    let (stdin, stdout) = child.io();
    acp_request(
        stdin,
        stdout,
        &spec.permission_policy,
        request_id,
        "initialize",
        json!({
            "protocolVersion": 1,
            "agent": spec.agent,
            "model": spec.model,
        }),
    )
    .await?;
    Ok(())
}

async fn configure_acp_session(
    child: &mut AcpChildLifecycle,
    spec: &OpenCodeLaunchSpec,
    session_id: &str,
    next_id: &mut u64,
) -> Result<(), OpenCodeError> {
    let (stdin, stdout) = child.io();
    set_session_config_option(
        stdin,
        stdout,
        &spec.permission_policy,
        *next_id,
        session_id,
        "mode",
        Some(spec.agent.as_str()),
    )
    .await?;
    *next_id += 1;
    let (stdin, stdout) = child.io();
    set_session_config_option(
        stdin,
        stdout,
        &spec.permission_policy,
        *next_id,
        session_id,
        "model",
        spec.model.as_deref(),
    )
    .await?;
    *next_id += 1;
    let (stdin, stdout) = child.io();
    set_session_config_option(
        stdin,
        stdout,
        &spec.permission_policy,
        *next_id,
        session_id,
        "effort",
        spec.effort.as_deref(),
    )
    .await?;
    *next_id += 1;
    Ok(())
}

async fn resume_acp_session(
    child: &mut AcpChildLifecycle,
    spec: &OpenCodeLaunchSpec,
    session: &OpenCodeSessionRecord,
    request_id: u64,
) -> Result<(), OpenCodeError> {
    let (stdin, stdout) = child.io();
    let resume_result = acp_request(
        stdin,
        stdout,
        &spec.permission_policy,
        request_id,
        "session/resume",
        session_resume_params(spec, session),
    )
    .await?;
    let resumed_session_id =
        extract_session_id(&resume_result).unwrap_or_else(|_| session.session_id.clone());
    if resumed_session_id != session.session_id {
        return Err(OpenCodeError::AcpProtocol(format!(
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
            warn!(error = %error, message = warning, "OpenCode ACP prompt stream ended with error");
        }
        let _ = child.wait().await;
        stderr_drain.abort();
    });
}

fn spawn_stream_drain(
    permission_policy: &PermissionPolicy,
    warning: &'static str,
    mut child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr_drain: JoinHandle<()>,
) {
    let permission_policy = permission_policy.clone();
    tokio::spawn(async move {
        if let Err(error) = drain_acp_stream(stdout, stdin, permission_policy).await {
            warn!(error = %error, message = warning, "OpenCode ACP stream drain ended with error");
        }
        let _ = child.wait().await;
        stderr_drain.abort();
    });
}

#[async_trait::async_trait]
impl OpenCodeLauncher for StdioOpenCodeLauncher {
    async fn launch(
        &self,
        spec: &OpenCodeLaunchSpec,
    ) -> Result<OpenCodeStartedSession, OpenCodeError> {
        self.launch_observed(spec, &NoopOpenCodeLaunchObserver)
            .await
    }

    async fn launch_observed(
        &self,
        spec: &OpenCodeLaunchSpec,
        observer: &dyn OpenCodeLaunchObserver,
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
        let mut child = AcpChildLifecycle::spawn(spec).await?;
        let process_id = child.process_id();
        if let Err(error) = observer
            .process_started(OpenCodeProcessStarted { process_id })
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
            observer
                .session_created(OpenCodeSessionCreated {
                    session_id: session_id.clone(),
                    process_id,
                })
                .await?;
            configure_acp_session(&mut child, spec, &session_id, &mut next_id).await?;
            Ok::<String, OpenCodeError>(session_id)
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
        let prompt = format!(
            "OpenCode ACP session id: {session_id}\n\n{task_prompt}",
            task_prompt = spec.prompt.as_str()
        );
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
            "OpenCode ACP prompt stream ended with error",
            process,
            stdin,
            stdout,
            stderr_drain,
        );

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
            Ok::<(), OpenCodeError>(())
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
            "OpenCode ACP resumed stream ended with error",
            process,
            stdin,
            stdout,
            stderr_drain,
        );

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
            Ok::<(), OpenCodeError>(())
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
        let prompt = format!(
            "Symphony repair required for existing ACP session `{}`.\n\n\
             Failure fingerprint: `{}`\n\n\
             Repair details:\n{}\n\n\
             Mnemesh evidence workspace contract:\n{}\n\n\
             MCP tool-schema loop guard:\n{}\n\n\
             Validation policy:\n{}\n\n\
             Commit policy for successful handoff:\n{}\n\n\
             Continue the same implementation session. Do not start a new task. \
             Fix the implementation or handoff, rerun the required validation, \
             and rewrite the structured Symphony handoff JSON at the configured sidecar path.",
            session.session_id,
            failure_fingerprint,
            repair_message,
            mnemesh_workspace_contract_text(
                spec.mnemesh_workspace_root.as_deref(),
                spec.cwd.as_path()
            ),
            mcp_tool_loop_guard_text(),
            validation_policy_text(),
            commit_policy_text()
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
            "OpenCode ACP repair prompt stream ended with error",
            process,
            stdin,
            stdout,
            stderr_drain,
        );

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
            Ok::<(), OpenCodeError>(())
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
        let prompt = format!(
            "Symphony continuation required for existing ACP session `{}`.\n\n\
             Continue the same implementation session. Do not start a new task. \
             Do not repeat already completed work unless validation requires it.\n\n\
             Mnemesh evidence workspace contract:\n{}\n\n\
             MCP tool-schema loop guard:\n{}\n\n\
             Validation policy:\n{}\n\n\
             Commit policy for successful handoff:\n{}\n\n{}",
            session.session_id,
            mnemesh_workspace_contract_text(
                spec.mnemesh_workspace_root.as_deref(),
                spec.cwd.as_path()
            ),
            mcp_tool_loop_guard_text(),
            validation_policy_text(),
            commit_policy_text(),
            continuation_message
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
            "OpenCode ACP continuation prompt stream ended with error",
            process,
            stdin,
            stdout,
            stderr_drain,
        );

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
    let branch_name = issue_branch_name(issue);
    OpenCodeLaunchSpec {
        command: project.opencode.command.clone(),
        args: project.opencode.args.clone(),
        cwd: project.branch.worktree_root.join(&issue.identifier),
        worktree_root: Some(project.branch.worktree_root.clone()),
        issue_identifier: issue.identifier.clone(),
        branch_name: branch_name.clone(),
        repo_path: Some(project.repo_path.clone()),
        mnemesh_workspace_root: project
            .mnemesh
            .as_ref()
            .map(|mnemesh| mnemesh.workspace_root.clone()),
        base_ref: Some(project.branch.base.clone()),
        agent: project.opencode.agent.clone(),
        model: project.opencode.model.clone(),
        effort: project.opencode.effort.clone(),
        prompt: build_issue_prompt(project, issue, &branch_name),
        permission_policy: project.opencode.permission_policy.clone(),
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
    #[error("opencode child stderr was not piped")]
    MissingStderr,
    #[error("opencode ACP protocol error: {0}")]
    AcpProtocol(String),
    #[error(
        "opencode ACP setup failed for {issue_identifier} pid={process_id:?} session={session_id:?}: {reason}; termination={termination:?}"
    )]
    AcpSetupFailed {
        issue_identifier: String,
        process_id: Option<u32>,
        session_id: Option<String>,
        reason: String,
        termination: Box<ProcessTreeTerminationEvidence>,
    },
    #[error("opencode process tree error: {0}")]
    ProcessTree(String),
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
    #[error("opencode launch observer error: {0}")]
    LaunchObserver(String),
}
