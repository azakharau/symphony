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
        return ensure_existing_git_worktree(spec).await;
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

    create_git_worktree(spec, repo_path, base_ref).await
}

async fn ensure_existing_git_worktree(spec: &OpenCodeLaunchSpec) -> Result<(), OpenCodeError> {
    let current_branch = git_output(
        &spec.cwd,
        ["branch", "--show-current"],
        "git branch --show-current",
    )
    .await?;
    if current_branch.trim() == spec.branch_name {
        debug!(
            issue = %spec.issue_identifier,
            cwd = %spec.cwd.display(),
            branch_name = %spec.branch_name,
            "OpenCode worktree already exists on expected branch"
        );
        return Ok(());
    }

    let status = git_output(
        &spec.cwd,
        ["status", "--porcelain"],
        "git status --porcelain",
    )
    .await?;
    if !status.trim().is_empty() {
        let observed_branch = if current_branch.trim().is_empty() {
            "DETACHED"
        } else {
            current_branch.trim()
        };
        return Err(OpenCodeError::InvalidWorktree(format!(
            "existing worktree {} is on branch {} but expected {}; dirty or untracked files prevent safe repair: {}",
            spec.cwd.display(),
            observed_branch,
            spec.branch_name,
            status.trim().lines().take(5).collect::<Vec<_>>().join("; ")
        )));
    }

    let Some(repo_path) = &spec.repo_path else {
        return Err(OpenCodeError::InvalidWorktree(format!(
            "existing worktree {} is not on expected branch {} and cannot be repaired without repo_path",
            spec.cwd.display(),
            spec.branch_name
        )));
    };

    let remove = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["worktree", "remove"])
        .arg(&spec.cwd)
        .output()
        .await?;
    if !remove.status.success() {
        return Err(OpenCodeError::GitCommand {
            command: format!(
                "git -C {} worktree remove {}",
                repo_path.display(),
                spec.cwd.display()
            ),
            stderr: String::from_utf8_lossy(&remove.stderr).to_string(),
        });
    }

    info!(
        issue = %spec.issue_identifier,
        cwd = %spec.cwd.display(),
        branch_name = %spec.branch_name,
        "removed clean stale OpenCode worktree before recreation"
    );
    create_git_worktree(spec, repo_path, spec.base_ref.as_deref().unwrap_or("HEAD")).await
}

async fn create_git_worktree(
    spec: &OpenCodeLaunchSpec,
    repo_path: &Path,
    base_ref: &str,
) -> Result<(), OpenCodeError> {
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

async fn git_output<const N: usize>(
    cwd: &Path,
    args: [&str; N],
    command: &str,
) -> Result<String, OpenCodeError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .await?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(OpenCodeError::GitCommand {
            command: format!("git -C {} {command}", cwd.display()),
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
