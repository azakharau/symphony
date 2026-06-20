mod acp;
mod archive;
mod lifecycle;
mod prompt;
mod session_metrics;
mod types;
mod worktree;

use serde_json::{Value, json};
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
    OpenCodeSessionMessageError, OpenCodeSessionTreeActivity, OpenCodeSessionTreeMetrics,
    OpenCodeTimelineEvent, OpenCodeTodoActivity, archive_and_delete_session_tree,
    read_latest_session_tree_error, read_session_tree_activity, read_session_tree_metrics,
};
use lifecycle::AcpChildLifecycle;
pub use lifecycle::ProcessTreeTerminationEvidence;
pub(crate) use lifecycle::terminate_process_tree;
use prompt::{
    build_issue_prompt, commit_policy_text, delegated_subagent_contract_text,
    mcp_tool_loop_guard_text, mnemesh_workspace_contract_text, validation_policy_text,
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
        write_acp_request(
            child.stdin(),
            prompt_request_id,
            "session/prompt",
            json!({
                "sessionId": session_id.as_str(),
                "prompt": [
                    {
                        "type": "text",
                        "text": spec.prompt.as_str(),
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
            "Symphony repair required for the current ACP session.\n\n\
             Failure fingerprint: `{}`\n\n\
             Repair details:\n{}\n\n\
             Mnemesh evidence workspace contract:\n{}\n\n\
             MCP tool-schema loop guard:\n{}\n\n\
             Delegated review/evaluator subagent contract:\n{}\n\n\
             Validation policy:\n{}\n\n\
             Commit policy for successful handoff:\n{}\n\n\
             Continue the same implementation session. Do not start a new task. \
             Fix the implementation or handoff, rerun the required validation, \
             and rewrite the structured Symphony handoff JSON at the configured sidecar path.",
            failure_fingerprint,
            repair_message,
            mnemesh_workspace_contract_text(
                spec.mnemesh_workspace_root.as_deref(),
                spec.cwd.as_path()
            ),
            mcp_tool_loop_guard_text(),
            delegated_subagent_contract_text(),
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
            "Symphony continuation required for the current ACP session.\n\n\
             Continue the same implementation session. Do not start a new task. \
             Do not repeat already completed work unless validation requires it.\n\n\
             Mnemesh evidence workspace contract:\n{}\n\n\
             MCP tool-schema loop guard:\n{}\n\n\
             Delegated review/evaluator subagent contract:\n{}\n\n\
             Validation policy:\n{}\n\n\
             Commit policy for successful handoff:\n{}\n\n{}",
            mnemesh_workspace_contract_text(
                spec.mnemesh_workspace_root.as_deref(),
                spec.cwd.as_path()
            ),
            mcp_tool_loop_guard_text(),
            delegated_subagent_contract_text(),
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
        let mut value: Value = serde_json::from_str(&input)
            .map_err(|error| OpenCodeError::MalformedHandoff(format!("{path:?}: {error}")))?;
        normalize_handoff_sidecar_value(&mut value, &session.worktree_path);
        let handoff = serde_json::from_value(value)
            .map_err(|error| OpenCodeError::MalformedHandoff(format!("{path:?}: {error}")))?;
        info!(
            session_id = %session.session_id,
            path = %path.display(),
            "OpenCode handoff sidecar loaded"
        );
        Ok(Some(handoff))
    }
}

fn normalize_handoff_sidecar_value(value: &mut Value, worktree_path: &str) {
    let Some(object) = value.as_object_mut() else {
        return;
    };

    if !object.contains_key("stop_reason")
        && let Some(status) = object.get("status").and_then(Value::as_str)
    {
        object.insert("stop_reason".to_owned(), Value::String(status.to_owned()));
    }
    object.remove("schema_version");
    object.remove("status");
    object.remove("repair_fingerprint");
    object.remove("task_id");
    object.remove("subtask_id");
    object.remove("mnemesh");

    if !object.contains_key("subagents") {
        if let Some(subagents_used) = object.remove("subagents_used") {
            object.insert("subagents".to_owned(), subagents_used);
        }
    } else {
        object.remove("subagents_used");
    }
    normalize_string_array_field(object, "subagents", structured_agent_label);
    normalize_string_array_field(object, "changed_files", structured_changed_file_label);
    normalize_string_array_field(object, "risks", structured_summary_label);

    if let Some(stages) = object
        .get_mut("lifecycle_stages")
        .and_then(serde_json::Value::as_array_mut)
    {
        for stage in stages {
            if !stage.is_string() {
                *stage = Value::String(structured_stage_label(stage));
            }
            if let Some(stage_name) = stage.as_str().and_then(canonical_handoff_stage) {
                *stage = Value::String(stage_name.to_owned());
            }
        }
    }

    if object.get("eval_results").is_some_and(Value::is_object) {
        let eval = object.remove("eval_results").unwrap_or(Value::Null);
        let passed = compact_eval_object_passed(&eval);
        let evidence_ref = eval
            .get("evaluation_ref")
            .or_else(|| eval.get("evidence_ref"))
            .or_else(|| eval.get("verification_ref"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let failure_fingerprint = eval
            .get("failure_fingerprint")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let details = eval
            .get("details")
            .or_else(|| eval.get("summary"))
            .or_else(|| eval.get("verification"))
            .map(|value| match value {
                Value::String(details) => details.clone(),
                Value::Array(items) => items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n"),
                other => other.to_string(),
            });
        object.insert(
            "eval_results".to_owned(),
            json!([{
                "suite": "opencode-evaluation",
                "passed": passed,
                "failure_fingerprint": failure_fingerprint,
                "details": details,
                "evidence_ref": evidence_ref,
            }]),
        );
    }
    normalize_eval_results(object);

    object.remove("validation");

    if let Some(git) = object.get_mut("git").and_then(Value::as_object_mut) {
        if !git.contains_key("head_sha") {
            if let Some(commit) = git.remove("commit") {
                git.insert("head_sha".to_owned(), commit);
            }
        } else {
            git.remove("commit");
        }
        if git
            .get("worktree_path")
            .is_none_or(|value| value.is_null() || value.as_str().is_some_and(str::is_empty))
        {
            git.insert("worktree_path".to_owned(), json!(worktree_path));
        }
        git.remove("remote");
        git.remove("pushed");
        git.remove("status");
        git.remove("evidence_ref");
        git.remove("base_branch");
        git.remove("base_sha");
        git.remove("previous_head_sha");
        git.remove("remote_ref");
        git.remove("remote_head_sha");
        git.remove("commit_message");
        git.remove("commit_summary");
        normalize_object_string_field(git, "branch", structured_summary_label);
        normalize_object_string_field(git, "head_sha", structured_summary_label);
        normalize_object_string_field(git, "pr_url", structured_summary_label);
        normalize_object_string_field(git, "worktree_path", structured_summary_label);
        remove_null_object_fields(git, &["head_sha", "pr_url"]);
    }

    if let Some(stop_reason) = object.get_mut("stop_reason")
        && let Some(reason) = stop_reason.as_str()
    {
        *stop_reason = match reason {
            "accepted" | "completed" | "success" => json!({"type": "success"}),
            other => json!({"type": other}),
        };
    }
}

fn normalize_eval_results(object: &mut serde_json::Map<String, Value>) {
    let Some(results) = object
        .get_mut("eval_results")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };

    for result in results {
        let Some(result_object) = result.as_object_mut() else {
            continue;
        };
        if result_object
            .get("suite")
            .is_none_or(|value| value.is_null() || value.as_str().is_some_and(str::is_empty))
        {
            result_object.insert(
                "suite".to_owned(),
                Value::String("opencode-evaluation".to_owned()),
            );
        }
        normalize_object_string_field(result_object, "suite", structured_summary_label);
        normalize_object_string_field(
            result_object,
            "failure_fingerprint",
            structured_summary_label,
        );
        normalize_object_string_field(result_object, "details", structured_summary_label);
        normalize_object_string_field(result_object, "evidence_ref", structured_summary_label);
        remove_null_object_fields(
            result_object,
            &["failure_fingerprint", "details", "evidence_ref"],
        );
    }
}

fn remove_null_object_fields(object: &mut serde_json::Map<String, Value>, fields: &[&str]) {
    for field in fields {
        if object.get(*field).is_some_and(Value::is_null) {
            object.remove(*field);
        }
    }
}

fn compact_eval_object_passed(eval: &Value) -> bool {
    if eval
        .get("failure_fingerprint")
        .and_then(Value::as_str)
        .is_some_and(|fingerprint| !fingerprint.trim().is_empty())
    {
        return false;
    }

    if let Some(passed) = eval.get("passed").and_then(Value::as_bool) {
        return passed;
    }

    let mut saw_positive = false;
    for key in [
        "outcome",
        "recommendation",
        "status",
        "result",
        "verdict",
        "review",
        "review_verdict",
        "evaluator",
        "evaluator_recommendation",
    ] {
        let Some(value) = eval.get(key).and_then(Value::as_str) else {
            continue;
        };
        let normalized = value.trim().to_ascii_lowercase();
        if matches!(
            normalized.as_str(),
            "fail"
                | "failed"
                | "failure"
                | "reject"
                | "rejected"
                | "needs_repair"
                | "needs-repair"
                | "blocked"
                | "blocker"
        ) {
            return false;
        }
        if matches!(
            normalized.as_str(),
            "accept" | "accepted" | "pass" | "passed" | "success" | "succeeded"
        ) {
            saw_positive = true;
        }
    }

    saw_positive
}

fn normalize_string_array_field(
    object: &mut serde_json::Map<String, Value>,
    field: &str,
    formatter: fn(&Value) -> String,
) {
    let Some(value) = object.get_mut(field) else {
        return;
    };
    match value {
        Value::Array(items) => {
            for item in items {
                if !item.is_string() {
                    *item = Value::String(formatter(item));
                }
            }
        }
        other if !other.is_string() => {
            *other = Value::Array(vec![Value::String(formatter(other))]);
        }
        Value::String(_) => {
            let item = value.take();
            *value = Value::Array(vec![item]);
        }
        _ => {}
    }
}

fn normalize_object_string_field(
    object: &mut serde_json::Map<String, Value>,
    field: &str,
    formatter: fn(&Value) -> String,
) {
    let Some(value) = object.get_mut(field) else {
        return;
    };
    if !value.is_null() && !value.is_string() {
        *value = Value::String(formatter(value));
    }
}

fn structured_agent_label(value: &Value) -> String {
    let Some(object) = value.as_object() else {
        return structured_summary_label(value);
    };
    let name = object
        .get("name")
        .or_else(|| object.get("agent"))
        .or_else(|| object.get("role"))
        .or_else(|| object.get("id"))
        .and_then(Value::as_str);
    let session = object
        .get("session_id")
        .or_else(|| object.get("session"))
        .and_then(Value::as_str);
    match (name, session) {
        (Some(name), Some(session)) => format!("{name}:{session}"),
        (Some(name), None) => name.to_owned(),
        _ => structured_summary_label(value),
    }
}

fn structured_changed_file_label(value: &Value) -> String {
    let Some(object) = value.as_object() else {
        return structured_summary_label(value);
    };
    let Some(path) = object
        .get("path")
        .or_else(|| object.get("file"))
        .or_else(|| object.get("filepath"))
        .and_then(Value::as_str)
    else {
        return structured_summary_label(value);
    };
    let start = object
        .get("start")
        .or_else(|| object.get("start_line"))
        .or_else(|| object.get("line_start"))
        .and_then(Value::as_u64);
    let end = object
        .get("end")
        .or_else(|| object.get("end_line"))
        .or_else(|| object.get("line_end"))
        .and_then(Value::as_u64);
    match (start, end) {
        (Some(start), Some(end)) => format!("{path}:{start}-{end}"),
        (Some(line), None) | (None, Some(line)) => format!("{path}:{line}"),
        _ => object
            .get("line_span")
            .and_then(Value::as_str)
            .map(|span| format!("{path}:{span}"))
            .unwrap_or_else(|| path.to_owned()),
    }
}

fn structured_stage_label(value: &Value) -> String {
    value
        .as_object()
        .and_then(|object| {
            object
                .get("stage")
                .or_else(|| object.get("name"))
                .or_else(|| object.get("type"))
                .and_then(Value::as_str)
        })
        .map(str::to_owned)
        .unwrap_or_else(|| structured_summary_label(value))
}

fn structured_summary_label(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.to_owned();
    }
    if let Some(object) = value.as_object()
        && let Some(text) = object
            .get("summary")
            .or_else(|| object.get("message"))
            .or_else(|| object.get("description"))
            .or_else(|| object.get("name"))
            .or_else(|| object.get("risk"))
            .or_else(|| object.get("ref"))
            .or_else(|| object.get("path"))
            .and_then(Value::as_str)
    {
        return text.to_owned();
    }
    value.to_string()
}

fn canonical_handoff_stage(stage: &str) -> Option<&'static str> {
    match stage {
        "planning" | "implementation" | "repair" | "failure_analysis" | "commit"
        | "commit_push" | "final_commit" | "final_push" => Some("running"),
        "repair_intake" | "base_fetch" | "merge_origin_master" | "conflict_resolution" | "push" => {
            Some("running")
        }
        "git_closure_repair" => Some("running"),
        "verification" | "evaluation" | "final_verification" | "final_evaluation" => Some("eval"),
        "review" | "code_review" | "final_review" => Some("review"),
        "handoff" | "final_handoff" => Some("handoff"),
        "completed" => Some("completed"),
        "failed" => Some("failed"),
        "starting" => Some("starting"),
        "running" => Some("running"),
        "eval" => Some("eval"),
        "silent" => Some("silent"),
        _ => None,
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn normalizes_opencode_orchestrator_handoff_dialect() {
        let mut value = json!({
            "schema_version": "symphony-opencode-handoff/v1",
            "status": "completed",
            "session_id": "ses_test",
            "task_id": "task-1",
            "lifecycle_stages": [
                "planning",
                "implementation",
                "verification",
                "review",
                "repair",
                "evaluation",
                "commit",
                "push",
                "handoff"
            ],
            "subagents_used": ["rust-engineer", "code-reviewer", "evaluator"],
            "eval_results": {
                "outcome": "accept",
                "review_verdict": "pass",
                "evaluator_recommendation": "accept",
                "details": "review and evaluator passed",
                "commands": [
                    {"command": "cargo test", "status": "pass"}
                ]
            },
            "changed_files": ["src/lib.rs:1-10"],
            "git": {
                "branch": "feature/test",
                "head_sha": "0123456789abcdef",
                "worktree_path": "/tmp/worktree",
                "remote": "origin",
                "remote_ref": "refs/heads/feature/test",
                "pushed": true
            },
            "mnemesh": {
                "canonical_evidence_workspace": "/home/agent/proj/mnemesh"
            },
            "risks": [],
            "stop_reason": "accepted"
        });

        normalize_handoff_sidecar_value(&mut value, "/tmp/worktree");
        let handoff: OpenCodeHandoff =
            serde_json::from_value(value).expect("normalized handoff should parse");

        assert_eq!(handoff.session_id, "ses_test");
        assert_eq!(
            handoff.lifecycle_stages,
            vec![
                OpenCodeStage::Running,
                OpenCodeStage::Running,
                OpenCodeStage::Eval,
                OpenCodeStage::Review,
                OpenCodeStage::Running,
                OpenCodeStage::Eval,
                OpenCodeStage::Running,
                OpenCodeStage::Running,
                OpenCodeStage::Handoff,
            ]
        );
        assert_eq!(
            handoff.subagents,
            ["rust-engineer", "code-reviewer", "evaluator"]
        );
        assert!(handoff.eval_results[0].passed);
        assert_eq!(handoff.stop_reason, OpenCodeStopReason::Success);
        let git = handoff.git.expect("git evidence");
        assert_eq!(git.branch, "feature/test");
        assert_eq!(git.head_sha.as_deref(), Some("0123456789abcdef"));
    }

    #[test]
    fn normalizes_compact_review_and_evaluator_eval_object_as_passed() {
        let mut value = json!({
            "status": "completed",
            "session_id": "ses_nrv_30",
            "lifecycle_stages": [
                "planning",
                "implementation",
                "verification",
                "review",
                "repair",
                "verification",
                "review",
                "evaluation",
                "commit",
                "push"
            ],
            "subagents_used": [
                "rust-engineer",
                "code-reviewer",
                "evaluator"
            ],
            "eval_results": {
                "verification": [
                    "cargo fmt --all -- --check",
                    "cargo check -p nervure-types",
                    "cargo nextest run -p nervure-types (348 passed)",
                    "cargo run -p nervure-cli -- generate-schemas --out schemas",
                    "git diff --check"
                ],
                "review": "pass",
                "evaluator": "accept"
            },
            "changed_files": [
                "crates/nervure-types/src/lib.rs:16-17",
                "schemas/pi-companion-profile.schema.json:1-379"
            ],
            "git": {
                "branch": "feature/nrv-30-define-pi-oh-my-pi-companion-integration-profile",
                "commit": "dcc20c501b62fe3a7b5f368287b33985040a58da",
                "pushed": true,
                "remote": "origin"
            },
            "mnemesh": {
                "task_id": "3d0058f5-49bf-45df-a63f-2e3c779a7f6f"
            },
            "risks": [],
            "stop_reason": "accepted"
        });

        normalize_handoff_sidecar_value(&mut value, "/tmp/nrv-30");
        let handoff: OpenCodeHandoff =
            serde_json::from_value(value).expect("compact eval handoff should parse");

        assert_eq!(handoff.session_id, "ses_nrv_30");
        assert_eq!(handoff.stop_reason, OpenCodeStopReason::Success);
        assert_eq!(handoff.eval_results.len(), 1);
        assert!(handoff.eval_results[0].passed);
        assert_eq!(handoff.eval_results[0].suite, "opencode-evaluation");
        assert_eq!(
            handoff.eval_results[0].details.as_deref(),
            Some(
                "cargo fmt --all -- --check\ncargo check -p nervure-types\ncargo nextest run -p nervure-types (348 passed)\ncargo run -p nervure-cli -- generate-schemas --out schemas\ngit diff --check"
            )
        );
        let git = handoff.git.expect("git evidence");
        assert_eq!(
            git.head_sha.as_deref(),
            Some("dcc20c501b62fe3a7b5f368287b33985040a58da")
        );
        assert_eq!(git.worktree_path, "/tmp/nrv-30");
    }

    #[test]
    fn derives_success_stop_reason_from_status_when_stop_reason_is_absent() {
        let mut value = json!({
            "status": "completed",
            "session_id": "ses_test",
            "lifecycle_stages": ["completed"],
            "subagents": [],
            "eval_results": [],
            "changed_files": [],
            "git": {
                "branch": "feature/test",
                "head_sha": null,
                "worktree_path": "/tmp/worktree"
            },
            "risks": []
        });

        normalize_handoff_sidecar_value(&mut value, "/tmp/worktree");
        let handoff: OpenCodeHandoff =
            serde_json::from_value(value).expect("status-derived handoff should parse");

        assert_eq!(handoff.stop_reason, OpenCodeStopReason::Success);
    }

    #[test]
    fn normalizes_structured_string_fields_from_opencode_handoff() {
        let mut value = json!({
            "session_id": "ses_structured",
            "lifecycle_stages": [
                {"stage": "final_review"},
                "completed"
            ],
            "subagents": [
                {"agent": "rust-engineer", "session_id": "ses_child"},
                {"summary": "evaluator"}
            ],
            "eval_results": [
                {
                    "suite": {"name": "opencode-evaluation"},
                    "passed": true,
                    "failure_fingerprint": null,
                    "details": {"summary": "all validation passed"},
                    "evidence_ref": {"path": "docs/evidence.md"}
                }
            ],
            "changed_files": [
                {"path": "src/lib.rs", "start_line": 1, "end_line": 20},
                {"path": "scripts/tools/gate.py", "line_span": "1-771"}
            ],
            "git": {
                "branch": {"ref": "feature/mne-215"},
                "head_sha": {"ref": "0123456789abcdef"},
                "worktree_path": {"path": "/tmp/worktree"},
                "pr_url": {"ref": "https://example.test/pr/1"},
                "commit_message": "test: commit message should be ignored"
            },
            "risks": [
                {"risk": "none"}
            ],
            "stop_reason": {"type": "success"}
        });

        normalize_handoff_sidecar_value(&mut value, "/tmp/worktree");
        let handoff: OpenCodeHandoff =
            serde_json::from_value(value).expect("structured string fields should parse");

        assert_eq!(
            handoff.lifecycle_stages,
            vec![OpenCodeStage::Review, OpenCodeStage::Completed]
        );
        assert_eq!(handoff.subagents, ["rust-engineer:ses_child", "evaluator"]);
        assert_eq!(handoff.eval_results[0].suite, "opencode-evaluation");
        assert_eq!(
            handoff.eval_results[0].details.as_deref(),
            Some("all validation passed")
        );
        assert_eq!(
            handoff.eval_results[0].evidence_ref.as_deref(),
            Some("docs/evidence.md")
        );
        assert_eq!(
            handoff.changed_files,
            ["src/lib.rs:1-20", "scripts/tools/gate.py:1-771"]
        );
        assert_eq!(handoff.risks, ["none"]);
        let git = handoff.git.expect("git evidence");
        assert_eq!(git.branch, "feature/mne-215");
        assert_eq!(git.head_sha.as_deref(), Some("0123456789abcdef"));
        assert_eq!(git.worktree_path, "/tmp/worktree");
        assert_eq!(git.pr_url.as_deref(), Some("https://example.test/pr/1"));
    }
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
