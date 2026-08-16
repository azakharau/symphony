use std::time::Duration;

use serde_json::{Value, json};

use crate::state::RuntimeProviderMode;

use super::RunnerLaunchSpec;

const OMP_PROMPT_STARTUP_PROBE: Duration = Duration::from_secs(5);
const OPENCODE_ACP_AGENT: &str = "build";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AgentExecutionAdapter {
    Acp,
    OmpAcp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AcpConfigOption<'a> {
    pub id: &'static str,
    pub value: Option<&'a str>,
}

impl AcpConfigOption<'_> {
    pub(super) fn is_supported_by(self, session_result: &Value) -> bool {
        let Some(value) = self.value.filter(|value| !value.trim().is_empty()) else {
            return false;
        };
        let Some(config_options) = session_result.get("configOptions") else {
            return false;
        };
        config_options.as_array().is_some_and(|options| {
            options.iter().any(|option| {
                option.get("id").and_then(Value::as_str) == Some(self.id)
                    && option
                        .get("options")
                        .and_then(Value::as_array)
                        .is_some_and(|values| {
                            values.iter().any(|option| {
                                option.get("value").and_then(Value::as_str) == Some(value)
                            })
                        })
            })
        })
    }
}

impl AgentExecutionAdapter {
    pub(super) const fn for_provider_mode(provider_mode: RuntimeProviderMode) -> Self {
        match provider_mode {
            RuntimeProviderMode::Acp => Self::Acp,
            RuntimeProviderMode::OmpAcp => Self::OmpAcp,
        }
    }

    pub(super) fn for_spec(spec: &RunnerLaunchSpec) -> Self {
        Self::for_provider_mode(spec.provider_mode)
    }

    pub(super) fn initialize_params(self, spec: &RunnerLaunchSpec) -> Value {
        match self {
            Self::Acp => json!({
                "protocolVersion": 1,
                "agent": OPENCODE_ACP_AGENT,
                "model": spec.model,
            }),
            Self::OmpAcp => json!({
                "protocolVersion": 1,
                "client": {"name": "symphony", "version": env!("CARGO_PKG_VERSION")},
                "providerId": spec.provider_id,
            }),
        }
    }

    pub(super) fn session_new_params(self, spec: &RunnerLaunchSpec) -> Value {
        let title = spec.prompt.lines().next().unwrap_or("Symphony agent issue");
        match self {
            Self::Acp => json!({
                "cwd": spec.cwd,
                "title": title,
                "agent": OPENCODE_ACP_AGENT,
                "mcpServers": [],
            }),
            Self::OmpAcp => json!({
                "cwd": spec.cwd,
                "title": spec.issue_identifier,
                "mcpServers": [],
            }),
        }
    }

    pub(super) fn session_resume_params(self, spec: &RunnerLaunchSpec, session_id: &str) -> Value {
        match self {
            Self::Acp | Self::OmpAcp => json!({
                "sessionId": session_id,
                "cwd": spec.cwd,
                "mcpServers": [],
            }),
        }
    }

    pub(super) fn config_options<'a>(self, spec: &'a RunnerLaunchSpec) -> Vec<AcpConfigOption<'a>> {
        match self {
            Self::Acp => vec![
                AcpConfigOption {
                    id: "mode",
                    value: Some(OPENCODE_ACP_AGENT),
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
            Self::Acp => None,
            Self::OmpAcp => Some(OMP_PROMPT_STARTUP_PROBE),
        }
    }
}
