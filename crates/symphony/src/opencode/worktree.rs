use std::path::{Component, Path, PathBuf};

use tokio::process::Command;
use tracing::{debug, info};

use super::{OpenCodeError, OpenCodeLaunchSpec};

pub(super) async fn ensure_worktree(spec: &OpenCodeLaunchSpec) -> Result<(), OpenCodeError> {
    validate_launch_worktree(spec)?;

    let Some(repo_path) = &spec.repo_path else {
        tokio::fs::create_dir_all(&spec.cwd).await?;
        return Ok(());
    };
    let Some(base_ref) = &spec.base_ref else {
        tokio::fs::create_dir_all(&spec.cwd).await?;
        return Ok(());
    };

    if tokio::fs::try_exists(spec.cwd.join(".git")).await? {
        debug!(
            issue = %spec.issue_identifier,
            cwd = %spec.cwd.display(),
            "OpenCode worktree already exists"
        );
        return Ok(());
    }

    if tokio::fs::try_exists(&spec.cwd).await? && directory_has_entries(&spec.cwd).await? {
        return Err(OpenCodeError::InvalidWorktree(format!(
            "target worktree {} exists but is not a git worktree",
            spec.cwd.display()
        )));
    }

    if let Some(parent) = spec.cwd.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["worktree", "add", "-B"])
        .arg(&spec.branch_name)
        .arg(&spec.cwd)
        .arg(base_ref)
        .output()
        .await?;

    if output.status.success() {
        info!(
            issue = %spec.issue_identifier,
            repo_path = %repo_path.display(),
            cwd = %spec.cwd.display(),
            base_ref,
            branch_name = %spec.branch_name,
            "OpenCode worktree created"
        );
        Ok(())
    } else {
        Err(OpenCodeError::GitCommand {
            command: format!(
                "git -C {} worktree add -B {} {} {}",
                repo_path.display(),
                spec.branch_name,
                spec.cwd.display(),
                base_ref
            ),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

async fn directory_has_entries(path: &Path) -> Result<bool, OpenCodeError> {
    let mut entries = tokio::fs::read_dir(path).await?;
    Ok(entries.next_entry().await?.is_some())
}

fn validate_launch_worktree(spec: &OpenCodeLaunchSpec) -> Result<(), OpenCodeError> {
    if !safe_worktree_name(&spec.issue_identifier) {
        return Err(OpenCodeError::InvalidWorktree(format!(
            "issue identifier `{}` is not a safe worktree path component",
            spec.issue_identifier
        )));
    }
    if !safe_branch_name(&spec.branch_name) {
        return Err(OpenCodeError::InvalidWorktree(format!(
            "branch `{}` is not safe for an OpenCode issue worktree",
            spec.branch_name
        )));
    }

    if let Some(root) = &spec.worktree_root {
        let expected = root.join(&spec.issue_identifier);
        if spec.cwd != expected {
            return Err(OpenCodeError::InvalidWorktree(format!(
                "worktree path {} does not match configured root plus issue identifier {}",
                spec.cwd.display(),
                expected.display()
            )));
        }
    }

    Ok(())
}

fn safe_worktree_name(identifier: &str) -> bool {
    !identifier.is_empty()
        && identifier
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

fn safe_branch_name(branch: &str) -> bool {
    !branch.is_empty()
        && branch != "HEAD"
        && !branch.starts_with('/')
        && !branch.ends_with('/')
        && !branch.contains("..")
        && !branch.contains("//")
        && branch.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '/')
        })
}

pub(super) fn handoff_sidecar_path(path: impl AsRef<Path>) -> PathBuf {
    path.as_ref()
        .join(".symphony")
        .join("opencode-handoff.json")
}

pub(super) async fn remove_stale_handoff_sidecar(
    worktree_path: &Path,
) -> Result<(), OpenCodeError> {
    let path = handoff_sidecar_path(worktree_path);
    match tokio::fs::remove_file(&path).await {
        Ok(()) => {
            debug!(path = %path.display(), "removed stale OpenCode handoff sidecar");
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub fn worktree_path_allowed(root: &Path, candidate: &Path) -> bool {
    candidate.is_absolute()
        && candidate.starts_with(root)
        && !candidate
            .components()
            .any(|component| matches!(component, Component::ParentDir))
}
