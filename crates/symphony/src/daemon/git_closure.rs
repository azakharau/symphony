use std::path::Path;

use anyhow::{Context, bail};
use tokio::process::Command;

use crate::{config::ProjectConfig, opencode::GitClosureEvidence};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum GitClosureResult {
    NoGitChanges,
    Integrated { base_branch: String },
}

pub(super) async fn verify_and_integrate_git_closure(
    project: &ProjectConfig,
    git: &GitClosureEvidence,
    changed_files: &[String],
) -> anyhow::Result<GitClosureResult> {
    let worktree_path = Path::new(git.worktree_path.trim());
    ensure_clean_worktree(worktree_path).await?;

    let Some(head_sha) = git
        .head_sha
        .as_deref()
        .map(str::trim)
        .filter(|sha| !sha.is_empty())
    else {
        if changed_files.is_empty() {
            ensure_no_unreported_worktree_commits(
                &project.repo_path,
                worktree_path,
                &project.branch.base,
            )
            .await?;
            return Ok(GitClosureResult::NoGitChanges);
        }
        bail!("git closure evidence did not include a commit SHA for changed files");
    };

    ensure_worktree_head(worktree_path, head_sha).await?;
    ensure_commit_exists(&project.repo_path, head_sha).await?;
    ensure_origin_remote(&project.repo_path).await?;
    ensure_issue_branch_pushed(&project.repo_path, git.branch.trim(), head_sha).await?;
    integrate_base_branch(&project.repo_path, &project.branch.base, head_sha).await?;
    ensure_remote_base_points_at(&project.repo_path, &project.branch.base, head_sha).await?;

    Ok(GitClosureResult::Integrated {
        base_branch: project.branch.base.clone(),
    })
}

async fn ensure_clean_worktree(worktree_path: &Path) -> anyhow::Result<()> {
    let status = git_output(
        worktree_path,
        &[
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--",
            ".",
            ":(exclude).symphony",
        ],
    )
    .await?;
    if status.trim().is_empty() {
        Ok(())
    } else {
        bail!("git closure worktree has uncommitted changes:\n{status}");
    }
}

async fn ensure_worktree_head(worktree_path: &Path, head_sha: &str) -> anyhow::Result<()> {
    let actual_head = git_output(worktree_path, &["rev-parse", "HEAD"]).await?;
    if same_sha(actual_head.trim(), head_sha) {
        Ok(())
    } else {
        bail!(
            "git closure commit `{head_sha}` does not match worktree HEAD `{}`",
            actual_head.trim()
        );
    }
}

async fn ensure_no_unreported_worktree_commits(
    repo_path: &Path,
    worktree_path: &Path,
    base_branch: &str,
) -> anyhow::Result<()> {
    let worktree_head = git_output(worktree_path, &["rev-parse", "HEAD"]).await?;
    let base_head = resolve_base_head(repo_path, base_branch).await?;
    if same_sha(worktree_head.trim(), base_head.trim()) {
        Ok(())
    } else {
        bail!(
            "no-change handoff omitted git.head_sha, but worktree HEAD `{}` differs from base `{base_branch}` at `{}`; commit, push, and report git.head_sha before cleanup",
            worktree_head.trim(),
            base_head.trim()
        );
    }
}

async fn resolve_base_head(repo_path: &Path, base_branch: &str) -> anyhow::Result<String> {
    if base_branch.trim().is_empty() {
        bail!("configured base branch must not be empty");
    }

    if git_output(repo_path, &["remote", "get-url", "origin"])
        .await
        .is_ok()
    {
        let remote_base = format!("refs/remotes/origin/{base_branch}");
        let fetch_refspec = format!("+refs/heads/{base_branch}:{remote_base}");
        git_status(repo_path, &["fetch", "origin", &fetch_refspec]).await?;
        return git_output(repo_path, &["rev-parse", &remote_base]).await;
    }

    let local_base = format!("refs/heads/{base_branch}");
    git_output(repo_path, &["rev-parse", &local_base]).await
}

async fn ensure_commit_exists(repo_path: &Path, head_sha: &str) -> anyhow::Result<()> {
    git_status(
        repo_path,
        &["cat-file", "-e", &format!("{head_sha}^{{commit}}")],
    )
    .await
}

async fn ensure_origin_remote(repo_path: &Path) -> anyhow::Result<()> {
    git_output(repo_path, &["remote", "get-url", "origin"])
        .await
        .map(|_| ())
        .context("git closure requires an origin remote")
}

async fn ensure_issue_branch_pushed(
    repo_path: &Path,
    branch: &str,
    head_sha: &str,
) -> anyhow::Result<()> {
    if branch.is_empty() {
        bail!("git closure evidence did not include an issue branch");
    }

    let remote_ref = format!("refs/remotes/origin/{branch}");
    let fetch_refspec = format!("+refs/heads/{branch}:{remote_ref}");
    git_status(repo_path, &["fetch", "origin", &fetch_refspec]).await?;
    let remote_head = git_output(repo_path, &["rev-parse", &remote_ref]).await?;
    if same_sha(remote_head.trim(), head_sha) {
        Ok(())
    } else {
        bail!(
            "origin/{branch} points at `{}` instead of handoff commit `{head_sha}`",
            remote_head.trim()
        );
    }
}

async fn integrate_base_branch(
    repo_path: &Path,
    base_branch: &str,
    head_sha: &str,
) -> anyhow::Result<()> {
    if base_branch.trim().is_empty() {
        bail!("configured base branch must not be empty");
    }

    let remote_base = format!("refs/remotes/origin/{base_branch}");
    let fetch_refspec = format!("+refs/heads/{base_branch}:{remote_base}");
    git_status(repo_path, &["fetch", "origin", &fetch_refspec]).await?;
    git_status(
        repo_path,
        &["merge-base", "--is-ancestor", &remote_base, head_sha],
    )
    .await
    .with_context(|| {
        format!(
            "handoff commit `{head_sha}` is not a fast-forward descendant of origin/{base_branch}"
        )
    })?;

    let current_branch = git_output(repo_path, &["branch", "--show-current"]).await?;
    if current_branch.trim() == base_branch {
        ensure_clean_worktree(repo_path).await?;
        git_status(repo_path, &["merge", "--ff-only", head_sha]).await?;
    } else {
        let local_base = format!("refs/heads/{base_branch}");
        git_status(repo_path, &["update-ref", &local_base, head_sha]).await?;
    }

    let push_refspec = format!("{head_sha}:refs/heads/{base_branch}");
    git_status(repo_path, &["push", "origin", &push_refspec]).await
}

async fn ensure_remote_base_points_at(
    repo_path: &Path,
    base_branch: &str,
    head_sha: &str,
) -> anyhow::Result<()> {
    let remote_ref = format!("refs/heads/{base_branch}");
    let output = git_output(repo_path, &["ls-remote", "origin", &remote_ref]).await?;
    let Some(remote_head) = output.split_whitespace().next() else {
        bail!("origin/{base_branch} was not found after push");
    };
    if same_sha(remote_head, head_sha) {
        Ok(())
    } else {
        bail!("origin/{base_branch} points at `{remote_head}` instead of `{head_sha}` after push");
    }
}

async fn git_status(repo_path: &Path, args: &[&str]) -> anyhow::Result<()> {
    run_git(repo_path, args).await.map(|_| ())
}

async fn git_output(repo_path: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = run_git(repo_path, args).await?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

async fn run_git(repo_path: &Path, args: &[&str]) -> anyhow::Result<std::process::Output> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(args)
        .output()
        .await
        .with_context(|| format!("run git -C {} {}", repo_path.display(), args.join(" ")))?;

    if output.status.success() {
        Ok(output)
    } else {
        bail!(
            "git -C {} {} failed: {}",
            repo_path.display(),
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn same_sha(left: &str, right: &str) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}
