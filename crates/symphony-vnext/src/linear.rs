use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LinearProjectConfig {
    pub team_key: String,
    pub project_id: Option<String>,
    pub project_milestone_id: Option<String>,
}
