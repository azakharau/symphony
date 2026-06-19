use std::{process::Stdio, time::Duration};

use anyhow::{Context, anyhow};
use serde::Deserialize;
use tokio::{process::Command, time::timeout};

const OCU_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct OpenCodeQuotaSnapshot {
    pub buckets: Vec<OpenCodeQuotaBucket>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct OpenCodeQuotaBucket {
    pub title: String,
    pub windows: Vec<OpenCodeQuotaWindow>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct OpenCodeQuotaWindow {
    pub label: String,
    pub reset_at: i64,
    pub used_percent: u8,
}

impl OpenCodeQuotaSnapshot {
    pub(super) async fn load_localhost() -> anyhow::Result<Self> {
        let output = timeout(
            OCU_TIMEOUT,
            Command::new("ocu")
                .args(["--localhost", "--plain"])
                .stdin(Stdio::null())
                .output(),
        )
        .await
        .context("timeout while reading OpenCode quota")?
        .context("launch ocu --localhost --plain")?;

        if !output.status.success() {
            return Err(anyhow!(
                "ocu --localhost --plain exited with status {}",
                output.status
            ));
        }

        Self::from_json_slice(&output.stdout)
    }

    pub(super) fn primary_five_hour_window(&self) -> Option<&OpenCodeQuotaWindow> {
        self.buckets
            .iter()
            .flat_map(|bucket| bucket.windows.iter())
            .filter(|window| window.label.eq_ignore_ascii_case("5h"))
            .min_by_key(|window| window.left_percent())
    }

    fn from_json_slice(input: &[u8]) -> anyhow::Result<Self> {
        let decoded: OcuQuotaResponse =
            serde_json::from_slice(input).context("parse ocu quota json")?;
        Ok(decoded.into())
    }
}

impl OpenCodeQuotaWindow {
    pub(super) fn left_percent(&self) -> u8 {
        100u8.saturating_sub(self.used_percent.min(100))
    }
}

#[derive(Debug, Deserialize)]
struct OcuQuotaResponse {
    buckets: Vec<OcuQuotaBucket>,
}

#[derive(Debug, Deserialize)]
struct OcuQuotaBucket {
    title: String,
    windows: Vec<OcuQuotaWindow>,
}

#[derive(Debug, Deserialize)]
struct OcuQuotaWindow {
    label: String,
    reset_at: i64,
    used_percent: u8,
}

impl From<OcuQuotaResponse> for OpenCodeQuotaSnapshot {
    fn from(value: OcuQuotaResponse) -> Self {
        Self {
            buckets: value
                .buckets
                .into_iter()
                .map(|bucket| OpenCodeQuotaBucket {
                    title: bucket.title,
                    windows: bucket
                        .windows
                        .into_iter()
                        .map(|window| OpenCodeQuotaWindow {
                            label: window.label,
                            reset_at: window.reset_at,
                            used_percent: window.used_percent,
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::OpenCodeQuotaSnapshot;

    #[test]
    fn parses_ocu_plain_json_and_selects_lowest_five_hour_quota() {
        let snapshot = OpenCodeQuotaSnapshot::from_json_slice(
            br#"{
              "buckets": [
                {"title":"Main Codex bucket","windows":[
                  {"label":"5h","reset_at":1781878537,"used_percent":2},
                  {"label":"weekly","reset_at":1782336129,"used_percent":26}
                ]},
                {"title":"Codex 5.3 Spark","windows":[
                  {"label":"5h","reset_at":1781883030,"used_percent":37}
                ]}
              ]
            }"#,
        )
        .expect("snapshot");

        let five_hour = snapshot
            .primary_five_hour_window()
            .expect("five hour window");

        assert_eq!(five_hour.left_percent(), 63);
        assert_eq!(five_hour.reset_at, 1_781_883_030);
    }
}
