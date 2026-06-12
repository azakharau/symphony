use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use tracing::{info, warn};

pub(super) async fn cleanup_worktree(repo_path: &Path, worktree_path: &str) -> anyhow::Result<()> {
    let path = PathBuf::from(worktree_path);
    if !tokio::fs::try_exists(&path).await? {
        info!(
            repo_path = %repo_path.display(),
            worktree_path = %path.display(),
            "worktree already absent; pruning git metadata"
        );
        prune_git_worktrees(repo_path).await?;
        return Ok(());
    }

    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["worktree", "remove", "--force"])
        .arg(&path)
        .output()
        .await
        .with_context(|| format!("remove git worktree {}", path.display()))?;

    if output.status.success() {
        info!(
            repo_path = %repo_path.display(),
            worktree_path = %path.display(),
            "git worktree removed"
        );
        return Ok(());
    }

    if tokio::fs::try_exists(path.join(".git")).await? {
        bail!(
            "git worktree remove failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    warn!(
        repo_path = %repo_path.display(),
        worktree_path = %path.display(),
        "git worktree remove failed, removing accepted non-git directory"
    );
    tokio::fs::remove_dir_all(&path)
        .await
        .with_context(|| format!("remove accepted non-git worktree {}", path.display()))?;
    prune_git_worktrees(repo_path).await?;
    Ok(())
}

async fn prune_git_worktrees(repo_path: &Path) -> anyhow::Result<()> {
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["worktree", "prune"])
        .output()
        .await
        .with_context(|| format!("prune git worktrees for {}", repo_path.display()))?;

    if !output.status.success() {
        bail!(
            "git worktree prune failed for {}: {}",
            repo_path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    info!(repo_path = %repo_path.display(), "git worktrees pruned");
    Ok(())
}
