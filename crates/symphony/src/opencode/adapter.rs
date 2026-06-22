use std::time::Duration;

use serde_json::{Value, json};

use crate::state::RuntimeProviderMode;

use super::OpenCodeLaunchSpec;

const OMP_PROMPT_STARTUP_PROBE: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AgentExecutionAdapter {
    OpenCodeAcp,
    OmpAcp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AcpConfigOption<'a> {
    pub id: &'static str,
    pub value: Option<&'a str>,
}

impl AgentExecutionAdapter {
    pub(super) const fn for_provider_mode(provider_mode: RuntimeProviderMode) -> Self {
        match provider_mode {
            RuntimeProviderMode::OpenCodeAcp => Self::OpenCodeAcp,
            RuntimeProviderMode::OmpAcp => Self::OmpAcp,
        }
    }

    pub(super) fn for_spec(spec: &OpenCodeLaunchSpec) -> Self {
        Self::for_provider_mode(spec.provider_mode)
    }

    pub(super) fn initialize_params(self, spec: &OpenCodeLaunchSpec) -> Value {
        match self {
            Self::OpenCodeAcp => json!({
                "protocolVersion": 1,
                "agent": spec.agent,
                "model": spec.model,
            }),
            Self::OmpAcp => json!({
                "protocolVersion": 1,
                "client": {"name": "symphony", "version": env!("CARGO_PKG_VERSION")},
                "providerId": spec.provider_id,
            }),
        }
    }

    pub(super) fn session_new_params(self, spec: &OpenCodeLaunchSpec) -> Value {
        let title = spec.prompt.lines().next().unwrap_or("Symphony agent issue");
        match self {
            Self::OpenCodeAcp => json!({
                "cwd": spec.cwd,
                "title": title,
                "agent": spec.agent,
                "mcpServers": [],
            }),
            Self::OmpAcp => json!({
                "cwd": spec.cwd,
                "title": spec.issue_identifier,
                "mcpServers": [],
            }),
        }
    }

    pub(super) fn session_resume_params(
        self,
        spec: &OpenCodeLaunchSpec,
        session_id: &str,
    ) -> Value {
        match self {
            Self::OpenCodeAcp | Self::OmpAcp => json!({
                "sessionId": session_id,
                "cwd": spec.cwd,
                "mcpServers": [],
            }),
        }
    }

    pub(super) fn config_options<'a>(
        self,
        spec: &'a OpenCodeLaunchSpec,
    ) -> Vec<AcpConfigOption<'a>> {
        match self {
            Self::OpenCodeAcp => vec![
                AcpConfigOption {
                    id: "mode",
                    value: Some(spec.agent.as_str()),
                },
                AcpConfigOption {
                    id: "model",
                    value: spec.model.as_deref(),
                },
                AcpConfigOption {
                    id: "effort",
                    value: spec.effort.as_deref(),
                },
            ],
            Self::OmpAcp => Vec::new(),
        }
    }

    pub(super) const fn prompt_startup_probe(self) -> Option<Duration> {
        match self {
            Self::OpenCodeAcp => None,
            Self::OmpAcp => Some(OMP_PROMPT_STARTUP_PROBE),
        }
    }
}
