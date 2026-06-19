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

    let base_ref = effective_base_ref(repo_path, base_ref).await?;

    if tokio::fs::try_exists(spec.cwd.join(".git")).await? {
        return ensure_existing_git_worktree(spec, repo_path, &base_ref).await;
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

    prune_stale_worktree_entries(repo_path).await?;
    create_git_worktree(spec, repo_path, &base_ref).await
}

pub(super) async fn ensure_resumable_worktree(
    spec: &OpenCodeLaunchSpec,
) -> Result<(), OpenCodeError> {
    validate_launch_worktree(spec)?;

    let Some(repo_path) = &spec.repo_path else {
        tokio::fs::create_dir_all(&spec.cwd).await?;
        return Ok(());
    };
    let Some(base_ref) = &spec.base_ref else {
        tokio::fs::create_dir_all(&spec.cwd).await?;
        return Ok(());
    };

    let base_ref = effective_base_ref(repo_path, base_ref).await?;

    if tokio::fs::try_exists(spec.cwd.join(".git")).await? {
        return ensure_existing_resumable_git_worktree(spec).await;
    }

    ensure_worktree_with_effective_base(spec, repo_path, &base_ref).await
}

async fn ensure_worktree_with_effective_base(
    spec: &OpenCodeLaunchSpec,
    repo_path: &Path,
    base_ref: &str,
) -> Result<(), OpenCodeError> {
    if tokio::fs::try_exists(&spec.cwd).await? && directory_has_entries(&spec.cwd).await? {
        return Err(OpenCodeError::InvalidWorktree(format!(
            "target worktree {} exists but is not a git worktree",
            spec.cwd.display()
        )));
    }

    if let Some(parent) = spec.cwd.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    prune_stale_worktree_entries(repo_path).await?;
    create_git_worktree(spec, repo_path, base_ref).await
}

async fn ensure_existing_git_worktree(
    spec: &OpenCodeLaunchSpec,
    repo_path: &Path,
    base_ref: &str,
) -> Result<(), OpenCodeError> {
    let current_branch = git_output(
        &spec.cwd,
        ["branch", "--show-current"],
        "git branch --show-current",
    )
    .await?;
    let status = git_output(
        &spec.cwd,
        ["status", "--porcelain", "--untracked-files=all"],
        "git status --porcelain --untracked-files=all",
    )
    .await?;

    let observed_branch = current_branch.trim();
    if observed_branch.is_empty() {
        return Err(OpenCodeError::InvalidWorktree(format!(
            "existing worktree {} has detached HEAD; expected branch {}",
            spec.cwd.display(),
            spec.branch_name
        )));
    }

    if observed_branch != spec.branch_name {
        let dirty_evidence = dirty_or_untracked_evidence(&status);
        let mut message = format!(
            "existing worktree {} is on branch {} but expected {}",
            spec.cwd.display(),
            observed_branch,
            spec.branch_name
        );
        if !dirty_evidence.is_empty() {
            message.push_str("; dirty or untracked files: ");
            message.push_str(&dirty_evidence);
        }
        return Err(OpenCodeError::InvalidWorktree(message));
    }

    if !status.trim().is_empty() {
        return Err(OpenCodeError::InvalidWorktree(format!(
            "existing worktree {} is on expected branch {} but has dirty or untracked files: {}",
            spec.cwd.display(),
            spec.branch_name,
            dirty_or_untracked_evidence(&status)
        )));
    }

    align_existing_worktree_to_base(spec, repo_path, base_ref).await?;

    debug!(
        issue = %spec.issue_identifier,
        cwd = %spec.cwd.display(),
        branch_name = %spec.branch_name,
        base_ref,
        "OpenCode worktree already exists on expected branch with clean status"
    );
    Ok(())
}

async fn ensure_existing_resumable_git_worktree(
    spec: &OpenCodeLaunchSpec,
) -> Result<(), OpenCodeError> {
    let current_branch = git_output(
        &spec.cwd,
        ["branch", "--show-current"],
        "git branch --show-current",
    )
    .await?;
    let status = git_output(
        &spec.cwd,
        ["status", "--porcelain", "--untracked-files=all"],
        "git status --porcelain --untracked-files=all",
    )
    .await?;

    let observed_branch = current_branch.trim();
    if observed_branch.is_empty() {
        return Err(OpenCodeError::InvalidWorktree(format!(
            "existing worktree {} has detached HEAD; expected branch {}",
            spec.cwd.display(),
            spec.branch_name
        )));
    }

    if observed_branch != spec.branch_name
        && !same_issue_generated_branch(observed_branch, &spec.issue_identifier)
    {
        let dirty_evidence = dirty_or_untracked_evidence(&status);
        let mut message = format!(
            "existing worktree {} is on branch {} but expected {}",
            spec.cwd.display(),
            observed_branch,
            spec.branch_name
        );
        if !dirty_evidence.is_empty() {
            message.push_str("; dirty or untracked files: ");
            message.push_str(&dirty_evidence);
        }
        return Err(OpenCodeError::InvalidWorktree(message));
    }

    if observed_branch != spec.branch_name {
        info!(
            issue = %spec.issue_identifier,
            cwd = %spec.cwd.display(),
            observed_branch,
            expected_branch = %spec.branch_name,
            dirty = !status.trim().is_empty(),
            "OpenCode resumable worktree accepted same-issue branch-name drift"
        );
    }

    debug!(
        issue = %spec.issue_identifier,
        cwd = %spec.cwd.display(),
        branch_name = %spec.branch_name,
        dirty = !status.trim().is_empty(),
        "OpenCode worktree is resumable on expected branch"
    );
    Ok(())
}

async fn effective_base_ref(repo_path: &Path, base_ref: &str) -> Result<String, OpenCodeError> {
    let base_ref = base_ref.trim();
    if base_ref.is_empty() {
        return Err(OpenCodeError::InvalidWorktree(
            "configured base ref must not be empty".into(),
        ));
    }

    if base_ref == "HEAD"
        || base_ref.starts_with("refs/")
        || base_ref.starts_with("origin/")
        || !remote_refreshed_base_by_default(base_ref)
    {
        return Ok(base_ref.into());
    }

    if git_output(
        repo_path,
        ["remote", "get-url", "origin"],
        "git remote get-url origin",
    )
    .await
    .is_err()
    {
        return Ok(base_ref.into());
    }

    let remote_base = format!("refs/remotes/origin/{base_ref}");
    let fetch_refspec = format!("+refs/heads/{base_ref}:{remote_base}");
    git_status(
        repo_path,
        ["fetch", "origin", &fetch_refspec],
        "git fetch origin <base>",
    )
    .await?;
    Ok(remote_base)
}

fn remote_refreshed_base_by_default(base_ref: &str) -> bool {
    matches!(base_ref, "main" | "master" | "trunk")
}

async fn align_existing_worktree_to_base(
    spec: &OpenCodeLaunchSpec,
    repo_path: &Path,
    base_ref: &str,
) -> Result<(), OpenCodeError> {
    let base_commit_ref = format!("{base_ref}^{{commit}}");
    let base_head = git_output(
        repo_path,
        ["rev-parse", "--verify", &base_commit_ref],
        "git rev-parse --verify <base>^{commit}",
    )
    .await?;
    if git_success(
        &spec.cwd,
        ["merge-base", "--is-ancestor", base_head.trim(), "HEAD"],
    )
    .await?
    {
        return Ok(());
    }

    info!(
        issue = %spec.issue_identifier,
        cwd = %spec.cwd.display(),
        branch_name = %spec.branch_name,
        base_ref,
        base_head = %base_head.trim(),
        "rebasing clean existing OpenCode worktree onto current base before launch"
    );
    match git_status(&spec.cwd, ["rebase", base_head.trim()], "git rebase <base>").await {
        Ok(()) => {
            let status = git_output(
                &spec.cwd,
                ["status", "--porcelain", "--untracked-files=all"],
                "git status --porcelain --untracked-files=all",
            )
            .await?;
            if status.trim().is_empty() {
                Ok(())
            } else {
                Err(OpenCodeError::InvalidWorktree(format!(
                    "existing worktree {} was dirty after rebasing onto base {}: {}",
                    spec.cwd.display(),
                    base_ref,
                    dirty_or_untracked_evidence(&status)
                )))
            }
        }
        Err(error) => {
            let _ = git_status(&spec.cwd, ["rebase", "--abort"], "git rebase --abort").await;
            Err(error)
        }
    }
}

fn dirty_or_untracked_evidence(status: &str) -> String {
    let status = status.trim();
    status.lines().take(5).collect::<Vec<_>>().join("; ")
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

async fn prune_stale_worktree_entries(repo_path: &Path) -> Result<(), OpenCodeError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["worktree", "prune"])
        .output()
        .await?;

    if output.status.success() {
        Ok(())
    } else {
        Err(OpenCodeError::GitCommand {
            command: format!("git -C {} worktree prune", repo_path.display()),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

async fn git_status<const N: usize>(
    cwd: &Path,
    args: [&str; N],
    command: &str,
) -> Result<(), OpenCodeError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .await?;
    if output.status.success() {
        Ok(())
    } else {
        Err(OpenCodeError::GitCommand {
            command: format!("git -C {} {command}", cwd.display()),
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

async fn git_success<const N: usize>(cwd: &Path, args: [&str; N]) -> Result<bool, OpenCodeError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .await?;
    Ok(output.status.success())
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

fn same_issue_generated_branch(branch: &str, issue_identifier: &str) -> bool {
    let issue_prefix = format!(
        "feature/{}-",
        issue_identifier.to_ascii_lowercase().replace('/', "-")
    );
    branch.starts_with(&issue_prefix)
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

#[cfg(test)]
mod tests {
    use std::{fs, process::Command as StdCommand};

    use super::*;
    use crate::opencode::PermissionPolicy;

    #[tokio::test]
    async fn existing_worktree_reuse_rejects_wrong_branch() {
        let fixture = GitFixture::new();
        fixture.add_worktree("other/SYM-1", false);

        let error = ensure_worktree(&fixture.launch_spec())
            .await
            .expect_err("wrong branch must be rejected");

        assert!(
            matches!(&error, OpenCodeError::InvalidWorktree(message) if message.contains("is on branch other/SYM-1 but expected symphony/SYM-1")),
            "{error}"
        );
    }

    #[tokio::test]
    async fn existing_worktree_reuse_rejects_detached_head() {
        let fixture = GitFixture::new();
        fixture.add_worktree("symphony/SYM-1", true);

        let error = ensure_worktree(&fixture.launch_spec())
            .await
            .expect_err("detached worktree must be rejected");

        assert!(
            matches!(&error, OpenCodeError::InvalidWorktree(message) if message.contains("has detached HEAD")),
            "{error}"
        );
    }

    #[tokio::test]
    async fn existing_worktree_reuse_rejects_dirty_expected_branch() {
        let fixture = GitFixture::new();
        fixture.add_worktree("symphony/SYM-1", false);
        fs::write(fixture.worktree.join("untracked.txt"), "dirty").expect("write dirty file");

        let error = ensure_worktree(&fixture.launch_spec())
            .await
            .expect_err("dirty worktree must be rejected");

        assert!(
            matches!(&error, OpenCodeError::InvalidWorktree(message) if message.contains("has dirty or untracked files")),
            "{error}"
        );
    }

    #[tokio::test]
    async fn resumable_worktree_accepts_dirty_expected_branch() {
        let fixture = GitFixture::new();
        fixture.add_worktree("symphony/SYM-1", false);
        fs::write(fixture.worktree.join("untracked.txt"), "dirty").expect("write dirty file");

        ensure_resumable_worktree(&fixture.launch_spec())
            .await
            .expect("dirty expected branch must be resumable");
    }

    #[tokio::test]
    async fn resumable_worktree_rejects_wrong_branch() {
        let fixture = GitFixture::new();
        fixture.add_worktree("other/SYM-1", false);
        fs::write(fixture.worktree.join("untracked.txt"), "dirty").expect("write dirty file");

        let error = ensure_resumable_worktree(&fixture.launch_spec())
            .await
            .expect_err("wrong branch must not be resumable");

        assert!(
            matches!(&error, OpenCodeError::InvalidWorktree(message) if message.contains("is on branch other/SYM-1 but expected symphony/SYM-1")),
            "{error}"
        );
    }

    #[tokio::test]
    async fn existing_worktree_reuse_accepts_clean_expected_branch() {
        let fixture = GitFixture::new();
        fixture.add_worktree("symphony/SYM-1", false);

        ensure_worktree(&fixture.launch_spec())
            .await
            .expect("clean expected branch can be reused");
    }

    #[tokio::test]
    async fn existing_worktree_reuse_rebases_clean_stale_branch_onto_current_base() {
        let fixture = GitFixture::new();
        fixture.add_worktree("symphony/SYM-1", false);
        fs::write(fixture.repo.join("README.md"), "base\nadvanced\n").expect("advance readme");
        run_git(&fixture.repo, ["add", "README.md"]);
        run_git(&fixture.repo, ["commit", "-m", "advance base"]);
        let base_head = git_output(
            &fixture.repo,
            ["rev-parse", "master"],
            "git rev-parse master",
        )
        .await
        .expect("base head");

        ensure_worktree(&fixture.launch_spec())
            .await
            .expect("clean stale branch should be rebased before launch");

        assert!(
            git_success(
                &fixture.worktree,
                ["merge-base", "--is-ancestor", base_head.trim(), "HEAD"]
            )
            .await
            .expect("merge-base check"),
            "existing worktree HEAD must descend from current base"
        );
        assert_eq!(
            fs::read_to_string(fixture.worktree.join("README.md")).expect("read worktree file"),
            "base\nadvanced\n"
        );
    }

    #[tokio::test]
    async fn missing_registered_worktree_is_pruned_before_launch() {
        let fixture = GitFixture::new();
        fixture.add_worktree("symphony/old", false);
        fs::remove_dir_all(&fixture.worktree).expect("remove worktree dir");

        ensure_worktree(&fixture.launch_spec())
            .await
            .expect("missing registered worktree must be pruned and recreated");

        assert_eq!(
            git_output(
                &fixture.worktree,
                ["branch", "--show-current"],
                "git branch --show-current",
            )
            .await
            .expect("worktree branch")
            .trim(),
            "symphony/SYM-1"
        );
    }

    struct GitFixture {
        _dir: tempfile::TempDir,
        repo: PathBuf,
        root: PathBuf,
        worktree: PathBuf,
    }

    impl GitFixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let repo = dir.path().join("repo");
            let root = dir.path().join("worktrees");
            let worktree = root.join("SYM-1");
            fs::create_dir_all(&repo).expect("repo dir");
            fs::create_dir_all(&root).expect("worktree root");
            run_git(&repo, ["init", "--initial-branch", "master"]);
            run_git(&repo, ["config", "user.email", "symphony@example.test"]);
            run_git(&repo, ["config", "user.name", "Symphony Test"]);
            fs::write(repo.join("README.md"), "base").expect("readme");
            run_git(&repo, ["add", "README.md"]);
            run_git(&repo, ["commit", "-m", "base"]);
            Self {
                _dir: dir,
                repo,
                root,
                worktree,
            }
        }

        fn add_worktree(&self, branch: &str, detached: bool) {
            if detached {
                run_git(
                    &self.repo,
                    [
                        "worktree",
                        "add",
                        "--detach",
                        self.worktree.to_str().expect("worktree utf8"),
                        "HEAD",
                    ],
                );
            } else {
                run_git(
                    &self.repo,
                    [
                        "worktree",
                        "add",
                        "-b",
                        branch,
                        self.worktree.to_str().expect("worktree utf8"),
                        "HEAD",
                    ],
                );
            }
        }

        fn launch_spec(&self) -> OpenCodeLaunchSpec {
            OpenCodeLaunchSpec {
                command: PathBuf::from("opencode"),
                args: Vec::new(),
                cwd: self.worktree.clone(),
                worktree_root: Some(self.root.clone()),
                issue_identifier: "SYM-1".into(),
                branch_name: "symphony/SYM-1".into(),
                repo_path: Some(self.repo.clone()),
                mnemesh_workspace_root: None,
                base_ref: Some("master".into()),
                agent: "build".into(),
                model: None,
                effort: None,
                prompt: "test".into(),
                permission_policy: PermissionPolicy::Reject,
            }
        }
    }

    fn run_git<const N: usize>(repo: &Path, args: [&str; N]) {
        let output = StdCommand::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
