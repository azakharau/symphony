use tokio::{
    io::{AsyncReadExt, BufReader},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    task::JoinHandle,
    time::{Duration, sleep},
};
use tracing::warn;

use super::{OpenCodeError, OpenCodeLaunchSpec};

pub(super) struct AcpChildLifecycle {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr_drain: JoinHandle<()>,
    process_id: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessTreeTerminationEvidence {
    pub root_process_id: u32,
    pub descendant_process_ids: Vec<u32>,
    pub term_signal_sent: bool,
    pub kill_signal_sent: bool,
    pub still_alive: bool,
    pub reason: String,
}

impl AcpChildLifecycle {
    pub(super) async fn spawn(spec: &OpenCodeLaunchSpec) -> Result<Self, OpenCodeError> {
        let mut command = Command::new(&spec.command);
        command
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .env("SYMPHONY_ISSUE_WORKTREE", &spec.cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if let Some(mnemesh_workspace_root) = &spec.mnemesh_workspace_root {
            command.env("SYMPHONY_MNEMESH_WORKSPACE_ROOT", mnemesh_workspace_root);
        }
        let mut child = command.spawn()?;
        let process_id = child.id();
        let stdin = child.stdin.take().ok_or(OpenCodeError::MissingStdin)?;
        let stdout = child.stdout.take().ok_or(OpenCodeError::MissingStdout)?;
        let stderr = child.stderr.take().ok_or(OpenCodeError::MissingStderr)?;
        let stderr_drain = drain_stderr(stderr);
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            stderr_drain,
            process_id,
        })
    }

    pub(super) const fn process_id(&self) -> Option<u32> {
        self.process_id
    }

    pub(super) const fn stdin(&mut self) -> &mut ChildStdin {
        &mut self.stdin
    }

    pub(super) const fn io(&mut self) -> (&mut ChildStdin, &mut BufReader<ChildStdout>) {
        (&mut self.stdin, &mut self.stdout)
    }

    pub(super) fn into_parts(self) -> (Child, ChildStdin, BufReader<ChildStdout>, JoinHandle<()>) {
        (self.child, self.stdin, self.stdout, self.stderr_drain)
    }

    pub(super) async fn setup_failed(
        mut self,
        issue_identifier: &str,
        session_id: Option<String>,
        reason: String,
    ) -> OpenCodeError {
        let termination = match self.process_id {
            Some(process_id) => terminate_process_tree(process_id, reason.as_str())
                .await
                .unwrap_or_else(|error| ProcessTreeTerminationEvidence {
                    root_process_id: process_id,
                    descendant_process_ids: Vec::new(),
                    term_signal_sent: false,
                    kill_signal_sent: false,
                    still_alive: true,
                    reason: format!("termination failed after setup error `{reason}`: {error}"),
                }),
            None => ProcessTreeTerminationEvidence {
                root_process_id: 0,
                descendant_process_ids: Vec::new(),
                term_signal_sent: false,
                kill_signal_sent: false,
                still_alive: false,
                reason: reason.clone(),
            },
        };
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
        self.stderr_drain.abort();
        OpenCodeError::AcpSetupFailed {
            issue_identifier: issue_identifier.to_owned(),
            process_id: self.process_id,
            session_id,
            reason,
            termination: Box::new(termination),
        }
    }
}

fn drain_stderr(mut stderr: ChildStderr) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut buffer = [0_u8; 8192];
        loop {
            match stderr.read(&mut buffer).await {
                Ok(0) => break,
                Ok(_) => {}
                Err(error) => {
                    warn!(error = %error, "OpenCode ACP stderr drain ended with error");
                    break;
                }
            }
        }
    })
}

pub(crate) async fn terminate_process_tree(
    root_process_id: u32,
    reason: &str,
) -> Result<ProcessTreeTerminationEvidence, OpenCodeError> {
    if !process_exists(root_process_id).await {
        return Ok(ProcessTreeTerminationEvidence {
            root_process_id,
            descendant_process_ids: Vec::new(),
            term_signal_sent: false,
            kill_signal_sent: false,
            still_alive: false,
            reason: reason.to_owned(),
        });
    }

    let mut targets = descendant_process_ids(root_process_id).await?;
    let descendant_process_ids = targets.clone();
    targets.reverse();
    targets.push(root_process_id);

    terminate_processes(&targets, "-TERM").await?;
    for _ in 0..20 {
        if !process_exists(root_process_id).await {
            return Ok(ProcessTreeTerminationEvidence {
                root_process_id,
                descendant_process_ids,
                term_signal_sent: true,
                kill_signal_sent: false,
                still_alive: false,
                reason: reason.to_owned(),
            });
        }
        sleep(Duration::from_millis(100)).await;
    }

    terminate_processes(&targets, "-KILL").await?;
    for _ in 0..10 {
        if !process_exists(root_process_id).await {
            return Ok(ProcessTreeTerminationEvidence {
                root_process_id,
                descendant_process_ids,
                term_signal_sent: true,
                kill_signal_sent: true,
                still_alive: false,
                reason: reason.to_owned(),
            });
        }
        sleep(Duration::from_millis(100)).await;
    }

    Ok(ProcessTreeTerminationEvidence {
        root_process_id,
        descendant_process_ids,
        term_signal_sent: true,
        kill_signal_sent: true,
        still_alive: true,
        reason: reason.to_owned(),
    })
}

async fn terminate_processes(process_ids: &[u32], signal: &str) -> Result<(), OpenCodeError> {
    for process_id in process_ids {
        if !process_exists(*process_id).await {
            continue;
        }
        let status = Command::new("kill")
            .arg(signal)
            .arg(process_id.to_string())
            .status()
            .await?;
        if !status.success() && process_exists(*process_id).await {
            warn!(
                process_id,
                signal,
                status = %status,
                "failed to signal OpenCode process tree member"
            );
        }
    }
    Ok(())
}

async fn process_exists(process_id: u32) -> bool {
    let Ok(stat) = tokio::fs::read_to_string(format!("/proc/{process_id}/stat")).await else {
        return false;
    };
    let Some(after_command) = stat.rsplit_once(") ") else {
        return true;
    };
    !after_command.1.starts_with("Z ")
}

async fn descendant_process_ids(root_process_id: u32) -> Result<Vec<u32>, OpenCodeError> {
    let mut children = std::collections::BTreeMap::<u32, Vec<u32>>::new();
    let mut entries = tokio::fs::read_dir("/proc").await?;
    while let Some(entry) = entries.next_entry().await? {
        let file_name = entry.file_name();
        let Some(pid) = file_name.to_str().and_then(|name| name.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(parent_pid) = read_parent_process_id(pid).await else {
            continue;
        };
        children.entry(parent_pid).or_default().push(pid);
    }

    let mut descendants = Vec::new();
    let mut stack = children.remove(&root_process_id).unwrap_or_default();
    while let Some(pid) = stack.pop() {
        if let Some(grandchildren) = children.remove(&pid) {
            stack.extend(grandchildren);
        }
        descendants.push(pid);
    }
    Ok(descendants)
}

async fn read_parent_process_id(process_id: u32) -> Result<u32, OpenCodeError> {
    let stat = tokio::fs::read_to_string(format!("/proc/{process_id}/stat")).await?;
    let Some(after_command) = stat.rsplit_once(") ") else {
        return Err(OpenCodeError::ProcessTree(format!(
            "invalid proc stat for pid {process_id}"
        )));
    };
    let mut fields = after_command.1.split_whitespace();
    let _state = fields.next();
    let Some(parent_pid) = fields.next() else {
        return Err(OpenCodeError::ProcessTree(format!(
            "missing parent pid for pid {process_id}"
        )));
    };
    parent_pid
        .parse::<u32>()
        .map_err(|error| OpenCodeError::ProcessTree(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stale_process_tree_termination_reports_descendant_evidence() {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 60 & wait")
            .spawn()
            .expect("spawn process tree");
        let root = child.id().expect("child pid");

        for _ in 0..20 {
            if !descendant_process_ids(root)
                .await
                .expect("read descendants")
                .is_empty()
            {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }
        let evidence = terminate_process_tree(root, "stale_opencode_process_tree")
            .await
            .expect("terminate process tree");
        let _ = child.wait().await;

        assert_eq!(evidence.root_process_id, root);
        assert!(!evidence.descendant_process_ids.is_empty());
        assert!(evidence.term_signal_sent);
        assert_eq!(evidence.reason, "stale_opencode_process_tree");
        assert!(!evidence.still_alive);
    }
}
