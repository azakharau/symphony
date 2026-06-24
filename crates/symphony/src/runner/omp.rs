use serde_json::{Value, json};
use tokio::time::timeout;
use tracing::info;

use crate::state::RuntimeFailureKind;

use super::{
    RunnerError, RunnerLaunchObserver, RunnerLaunchSpec, RunnerProcessStarted,
    RunnerSessionCreated, RunnerStartedSession,
    acp::{acp_request, extract_session_id, write_acp_request},
    adapter::AgentExecutionAdapter,
    ensure_worktree, launch_uses_issue_worktree,
    lifecycle::AcpChildLifecycle,
    prompt_with_session_binding, read_acp_response, remove_stale_handoff_sidecar,
    spawn_prompt_reader, spawn_stream_drain,
};

#[derive(Debug, Default)]
pub struct StdioOmpAcpLauncher;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OmpAcpTelemetry {
    pub frame_count: u64,
    pub session_evidence_refs: Vec<String>,
}

impl StdioOmpAcpLauncher {
    pub async fn launch_observed(
        &self,
        spec: &RunnerLaunchSpec,
        observer: &dyn RunnerLaunchObserver,
    ) -> Result<RunnerStartedSession, RunnerError> {
        info!(
            issue = %spec.issue_identifier,
            cwd = %spec.cwd.display(),
            command = %spec.command.display(),
            provider_id = spec.provider_id.as_deref().unwrap_or("unknown"),
            "launching OMP ACP session"
        );
        if launch_uses_issue_worktree(spec) {
            ensure_worktree(spec).await?;
            remove_stale_handoff_sidecar(&spec.cwd).await?;
        }
        let mut child = AcpChildLifecycle::spawn(spec).await.map_err(|error| {
            if let RunnerError::Io(io) = &error
                && io.kind() == std::io::ErrorKind::NotFound
            {
                return RunnerError::RuntimeFailure {
                    kind: RuntimeFailureKind::MissingBinary,
                    message: format!("OMP ACP binary not found: {}", spec.command.display()),
                };
            }
            error
        })?;
        let process_id = child.process_id();
        if let Err(error) = observer
            .process_started(RunnerProcessStarted { process_id })
            .await
        {
            return Err(child
                .setup_failed(&spec.issue_identifier, None, error.to_string())
                .await);
        }

        let mut telemetry = OmpAcpTelemetry::default();
        let adapter = AgentExecutionAdapter::for_spec(spec);
        let mut next_id = 1_u64;
        let setup = async {
            let initialize = omp_acp_request(
                &mut child,
                &mut telemetry,
                spec,
                next_id,
                "initialize",
                adapter.initialize_params(spec),
            )
            .await?;
            ensure_supported_version(&initialize)?;
            next_id += 1;

            let session_result = omp_acp_request(
                &mut child,
                &mut telemetry,
                spec,
                next_id,
                "session/new",
                adapter.session_new_params(spec),
            )
            .await?;
            telemetry
                .session_evidence_refs
                .extend(evidence_refs(&session_result));
            let session_id = extract_session_id(&session_result)?;
            next_id += 1;
            observer
                .session_created(RunnerSessionCreated {
                    session_id: session_id.clone(),
                    process_id,
                })
                .await?;
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

        let prompt = prompt_with_session_binding(&spec.prompt, &session_id);
        write_acp_request(
            child.stdin(),
            next_id,
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": prompt}],
            }),
        )
        .await?;
        telemetry.frame_count = telemetry.frame_count.saturating_add(1);
        let prompt_response = {
            let (stdin, stdout) = child.io();
            timeout(
                adapter
                    .prompt_startup_probe()
                    .expect("OMP ACP adapter must define prompt startup probe"),
                read_acp_response(
                    stdout,
                    stdin,
                    &spec.permission_policy,
                    next_id,
                    "session/prompt",
                ),
            )
            .await
        };
        match prompt_response {
            Ok(Ok(_)) => {
                let (process, stdin, stdout, stderr_drain) = child.into_parts();
                spawn_stream_drain(
                    &spec.permission_policy,
                    "OMP ACP stream drain ended with error",
                    process,
                    stdin,
                    stdout,
                    stderr_drain,
                );
            }
            Ok(Err(error)) => {
                return Err(child
                    .setup_failed(&spec.issue_identifier, Some(session_id), error.to_string())
                    .await);
            }
            Err(_) => {
                let (process, stdin, stdout, stderr_drain) = child.into_parts();
                spawn_prompt_reader(
                    &spec.permission_policy,
                    next_id,
                    "OMP ACP prompt stream ended with error",
                    process,
                    stdin,
                    stdout,
                    stderr_drain,
                );
            }
        }

        Ok(RunnerStartedSession {
            session_id,
            process_id,
            acp_frame_count: telemetry.frame_count,
            session_evidence_refs: telemetry.session_evidence_refs,
        })
    }
}

async fn omp_acp_request(
    child: &mut AcpChildLifecycle,
    telemetry: &mut OmpAcpTelemetry,
    spec: &RunnerLaunchSpec,
    id: u64,
    method: &str,
    params: Value,
) -> Result<Value, RunnerError> {
    let (stdin, stdout) = child.io();
    let response = acp_request(stdin, stdout, &spec.permission_policy, id, method, params).await;
    telemetry.frame_count = telemetry.frame_count.saturating_add(2);
    response.map_err(classify_protocol_error)
}

fn ensure_supported_version(value: &Value) -> Result<(), RunnerError> {
    let version = value
        .get("protocolVersion")
        .or_else(|| value.get("protocol_version"))
        .and_then(Value::as_u64)
        .unwrap_or(1);
    if version == 1 {
        return Ok(());
    }
    Err(RunnerError::RuntimeFailure {
        kind: RuntimeFailureKind::UnsupportedOmpVersion,
        message: format!("unsupported OMP ACP protocol version {version}"),
    })
}

fn classify_protocol_error(error: RunnerError) -> RunnerError {
    let RunnerError::AcpProtocol(message) = error else {
        return error;
    };
    let kind = classify_omp_acp_failure_kind(&message);
    RunnerError::RuntimeFailure { kind, message }
}

pub fn classify_omp_acp_failure_kind(message: &str) -> RuntimeFailureKind {
    let lower = message.to_ascii_lowercase();
    if lower.contains("auth")
        || lower.contains("credential")
        || lower.contains("api key")
        || lower.contains("provider unavailable")
    {
        RuntimeFailureKind::ProviderAuthUnavailable
    } else if lower.contains("unsupported") && lower.contains("version") {
        RuntimeFailureKind::UnsupportedOmpVersion
    } else {
        RuntimeFailureKind::MalformedAcpFrame
    }
}

fn evidence_refs(value: &Value) -> Vec<String> {
    [
        "sessionEvidenceRefs",
        "sdkSessionEvidenceRefs",
        "evidenceRefs",
    ]
    .into_iter()
    .flat_map(|field| {
        value
            .get(field)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
    })
    .filter_map(Value::as_str)
    .filter(|reference| !reference.trim().is_empty())
    .take(8)
    .map(str::to_owned)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_omp_acp_runtime_failures() {
        assert_eq!(
            classify_omp_acp_failure_kind("provider auth unavailable"),
            RuntimeFailureKind::ProviderAuthUnavailable
        );
        assert_eq!(
            classify_omp_acp_failure_kind("unsupported protocol version 2"),
            RuntimeFailureKind::UnsupportedOmpVersion
        );
        assert_eq!(
            classify_omp_acp_failure_kind("invalid ACP JSON"),
            RuntimeFailureKind::MalformedAcpFrame
        );
    }
}
