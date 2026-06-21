use serde_json::{Value, json};
use tracing::info;

use crate::state::RuntimeFailureKind;

use super::lifecycle::AcpChildLifecycle;
use super::{
    OpenCodeError, OpenCodeLaunchObserver, OpenCodeLaunchSpec, OpenCodeProcessStarted,
    OpenCodeSessionCreated, OpenCodeStartedSession,
    acp::{acp_request, extract_session_id, write_acp_request},
    ensure_worktree, launch_uses_issue_worktree, remove_stale_handoff_sidecar, spawn_prompt_reader,
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
        spec: &OpenCodeLaunchSpec,
        observer: &dyn OpenCodeLaunchObserver,
    ) -> Result<OpenCodeStartedSession, OpenCodeError> {
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
            if let OpenCodeError::Io(io) = &error
                && io.kind() == std::io::ErrorKind::NotFound
            {
                return OpenCodeError::RuntimeFailure {
                    kind: RuntimeFailureKind::MissingBinary,
                    message: format!("OMP ACP binary not found: {}", spec.command.display()),
                };
            }
            error
        })?;
        let process_id = child.process_id();
        if let Err(error) = observer
            .process_started(OpenCodeProcessStarted { process_id })
            .await
        {
            return Err(child
                .setup_failed(&spec.issue_identifier, None, error.to_string())
                .await);
        }

        let mut telemetry = OmpAcpTelemetry::default();
        let mut next_id = 1_u64;
        let setup = async {
            let initialize = omp_acp_request(
                &mut child,
                &mut telemetry,
                spec,
                next_id,
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "client": {"name": "symphony", "version": env!("CARGO_PKG_VERSION")},
                    "agent": spec.agent,
                    "model": spec.model,
                    "providerId": spec.provider_id,
                }),
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
                json!({
                    "cwd": spec.cwd,
                    "title": spec.issue_identifier,
                    "agent": spec.agent,
                    "model": spec.model,
                    "mcpServers": [],
                }),
            )
            .await?;
            telemetry
                .session_evidence_refs
                .extend(evidence_refs(&session_result));
            let session_id = extract_session_id(&session_result)?;
            next_id += 1;
            observer
                .session_created(OpenCodeSessionCreated {
                    session_id: session_id.clone(),
                    process_id,
                })
                .await?;
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

        write_acp_request(
            child.stdin(),
            next_id,
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": spec.prompt}],
            }),
        )
        .await?;
        telemetry.frame_count = telemetry.frame_count.saturating_add(1);
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

        Ok(OpenCodeStartedSession {
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
    spec: &OpenCodeLaunchSpec,
    id: u64,
    method: &str,
    params: Value,
) -> Result<Value, OpenCodeError> {
    let (stdin, stdout) = child.io();
    let response = acp_request(stdin, stdout, &spec.permission_policy, id, method, params).await;
    telemetry.frame_count = telemetry.frame_count.saturating_add(2);
    response.map_err(classify_protocol_error)
}

fn ensure_supported_version(value: &Value) -> Result<(), OpenCodeError> {
    let version = value
        .get("protocolVersion")
        .or_else(|| value.get("protocol_version"))
        .and_then(Value::as_u64)
        .unwrap_or(1);
    if version == 1 {
        return Ok(());
    }
    Err(OpenCodeError::RuntimeFailure {
        kind: RuntimeFailureKind::UnsupportedOmpVersion,
        message: format!("unsupported OMP ACP protocol version {version}"),
    })
}

fn classify_protocol_error(error: OpenCodeError) -> OpenCodeError {
    let OpenCodeError::AcpProtocol(message) = error else {
        return error;
    };
    let kind = classify_omp_acp_failure_kind(&message);
    OpenCodeError::RuntimeFailure { kind, message }
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
