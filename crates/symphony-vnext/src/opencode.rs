use std::path::PathBuf;

use serde::{Deserialize, Serialize};

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
