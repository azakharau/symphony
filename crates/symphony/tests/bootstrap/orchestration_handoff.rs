use super::*;

#[derive(Debug)]
struct DoneRequiresStoppedProcessLinearClient {
    issue: LinearIssue,
    process_id: u32,
    transitions: std::sync::Mutex<Vec<(String, LinearTransition)>>,
    evidence: std::sync::Mutex<Vec<(String, LinearIssueEvidence)>>,
}

impl DoneRequiresStoppedProcessLinearClient {
    fn new(issue: LinearIssue, process_id: u32) -> Self {
        Self {
            issue,
            process_id,
            transitions: std::sync::Mutex::new(Vec::new()),
            evidence: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn transitions(&self) -> Vec<(String, LinearTransition)> {
        self.transitions.lock().expect("transitions lock").clone()
    }
}

#[async_trait::async_trait]
impl LinearClient for DoneRequiresStoppedProcessLinearClient {
    async fn fetch_candidate_issues(
        &self,
        _project: &symphony::config::ProjectConfig,
    ) -> Result<Vec<LinearIssue>, LinearClientError> {
        Ok(vec![self.issue.clone()])
    }

    async fn transition_issue(
        &self,
        issue_id: &str,
        transition: LinearTransition,
    ) -> Result<(), LinearClientError> {
        if transition == LinearTransition::Done && test_process_is_alive(self.process_id) {
            return Err(LinearClientError::Message(
                "Done transition happened before OpenCode process termination".into(),
            ));
        }
        self.transitions
            .lock()
            .expect("transitions lock")
            .push((issue_id.to_string(), transition));
        Ok(())
    }

    async fn record_issue_evidence(
        &self,
        issue_id: &str,
        evidence: LinearIssueEvidence,
    ) -> Result<(), LinearClientError> {
        self.evidence
            .lock()
            .expect("evidence lock")
            .push((issue_id.to_string(), evidence));
        Ok(())
    }
}

fn test_process_is_alive(process_id: u32) -> bool {
    let path = format!("/proc/{process_id}/cmdline");
    fs::read(path)
        .map(|cmdline| {
            cmdline
                .split(|byte| *byte == 0)
                .filter_map(|part| std::str::from_utf8(part).ok())
                .any(|part| part.contains("opencode"))
        })
        .unwrap_or(false)
}

#[tokio::test]
async fn passing_opencode_handoff_moves_done_records_git_metadata_and_removes_worktree() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let repo = dir.path().join("repo");
    let origin = dir.path().join("origin.git");
    fs::create_dir_all(&origin).expect("origin dir");
    run_git(&origin, ["init", "--bare"]);
    fs::create_dir_all(&repo).expect("repo dir");
    run_git(&repo, ["init"]);
    run_git(&repo, ["config", "user.email", "symphony@example.test"]);
    run_git(&repo, ["config", "user.name", "Symphony Test"]);
    run_git(
        &repo,
        [
            "remote",
            "add",
            "origin",
            origin.to_str().expect("origin utf8"),
        ],
    );
    fs::write(repo.join("README.md"), "base checkout").expect("readme");
    run_git(&repo, ["add", "README.md"]);
    run_git(&repo, ["commit", "-m", "base"]);
    run_git(&repo, ["branch", "agent-server/opencode-runner-extension"]);
    run_git(
        &repo,
        ["push", "origin", "agent-server/opencode-runner-extension"],
    );
    let worktree_root = dir.path().join("allowed-worktrees");
    let worktree = worktree_root.join("SYM-80");
    run_git(
        &repo,
        [
            "worktree",
            "add",
            "--detach",
            worktree.to_str().expect("worktree path utf8"),
            "agent-server/opencode-runner-extension",
        ],
    );
    fs::write(worktree.join("artifact.txt"), "done").expect("artifact");
    run_git(&worktree, ["add", "artifact.txt"]);
    run_git(&worktree, ["commit", "-m", "SYM-80 implementation"]);
    let head_sha = git_output(&worktree, ["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    let issue_branch = "symphony/SYM-80";
    let issue_refspec = format!("HEAD:refs/heads/{issue_branch}");
    run_git(&worktree, ["push", "origin", &issue_refspec]);
    let config_toml = valid_config_toml()
        .replace(
            "repo_path = \"/home/agent/proj/symphony\"",
            &format!("repo_path = \"{}\"", repo.display()),
        )
        .replace(
            "/home/agent/.symphony/workspaces/opencode/symphony",
            &worktree_root.display().to_string(),
        );
    let config = RootConfig::from_toml_str(&config_toml).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "completed", "SYM-80"))
        .await
        .expect("running issue");
    store
        .upsert_opencode_session(test_session("symphony", "completed", "oc-80", &worktree))
        .await
        .expect("running session");

    let client = RecordingLinearClient::new(vec![linear_issue(
        "completed",
        "SYM-80",
        "In Progress",
        Some(1),
    )]);
    let opencode = ScriptedOpenCodeLauncher::new(Some(success_handoff(
        "oc-80",
        &worktree,
        issue_branch,
        &head_sha,
    )));

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert_eq!(
        client.transitions(),
        vec![("completed".into(), LinearTransition::Done)]
    );
    assert!(client.evidence().iter().any(|(_, evidence)| {
        evidence.body.contains(&head_sha)
            && evidence
                .body
                .contains("agent-server/opencode-runner-extension")
    }));
    let completed = store
        .issue("symphony", "completed")
        .await
        .expect("query completed")
        .expect("completed issue");
    assert_eq!(completed.lifecycle_stage, LifecycleStage::Completed);
    assert_eq!(completed.cleanup_status, CleanupStatus::Complete);
    let git_ref = completed.git_ref.expect("git ref");
    assert_eq!(git_ref.branch, issue_branch);
    assert_eq!(git_ref.head_sha.as_deref(), Some(head_sha.as_str()));
    assert_eq!(
        git_ref.pr_url.as_deref(),
        Some("https://example.test/pr/80")
    );
    assert_eq!(
        git_output(
            &origin,
            ["rev-parse", "agent-server/opencode-runner-extension"]
        )
        .trim(),
        head_sha
    );
    assert!(!worktree.exists(), "accepted handoff must remove worktree");
    assert!(
        !git_output(&repo, ["worktree", "list", "--porcelain"])
            .contains(worktree.to_str().expect("worktree path utf8")),
        "accepted handoff must unregister the git worktree"
    );
    let session = store
        .opencode_session("symphony", "completed", "oc-80")
        .await
        .expect("query completed session")
        .expect("completed session");
    assert_eq!(session.lifecycle_stage, LifecycleStage::Completed);
    assert_eq!(session.stage, OpenCodeStage::Completed);
    assert_eq!(session.process_id, None);
    assert_eq!(
        session.lifecycle_marker.as_deref(),
        Some("handoff_accepted")
    );
    assert_eq!(session.last_event.as_deref(), Some("issue_closed"));

    let terminal_poll =
        RecordingLinearClient::new(vec![linear_issue("completed", "SYM-80", "Done", Some(1))]);
    daemon::run_once_with_linear_client(&config, &store, &terminal_poll)
        .await
        .expect("terminal reconciliation");
    let reconciled = store
        .issue("symphony", "completed")
        .await
        .expect("query reconciled")
        .expect("reconciled issue");
    assert_eq!(reconciled.cleanup_status, CleanupStatus::Complete);
    assert_eq!(
        reconciled.git_ref.expect("git ref").head_sha.as_deref(),
        Some(head_sha.as_str())
    );

    run_git(
        &repo,
        [
            "worktree",
            "add",
            "--detach",
            worktree.to_str().expect("worktree path utf8"),
            "agent-server/opencode-runner-extension",
        ],
    );
}

#[tokio::test]
async fn passing_handoff_stops_process_before_done_and_removes_worktree_immediately_after() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let repo = dir.path().join("repo");
    let origin = dir.path().join("origin.git");
    fs::create_dir_all(&origin).expect("origin dir");
    run_git(&origin, ["init", "--bare"]);
    fs::create_dir_all(&repo).expect("repo dir");
    run_git(&repo, ["init"]);
    run_git(&repo, ["config", "user.email", "symphony@example.test"]);
    run_git(&repo, ["config", "user.name", "Symphony Test"]);
    run_git(
        &repo,
        [
            "remote",
            "add",
            "origin",
            origin.to_str().expect("origin utf8"),
        ],
    );
    fs::write(repo.join("README.md"), "base checkout").expect("readme");
    run_git(&repo, ["add", "README.md"]);
    run_git(&repo, ["commit", "-m", "base"]);
    run_git(&repo, ["branch", "agent-server/opencode-runner-extension"]);
    run_git(
        &repo,
        ["push", "origin", "agent-server/opencode-runner-extension"],
    );

    let worktree_root = dir.path().join("allowed-worktrees");
    let worktree = worktree_root.join("SYM-90");
    run_git(
        &repo,
        [
            "worktree",
            "add",
            "--detach",
            worktree.to_str().expect("worktree path utf8"),
            "agent-server/opencode-runner-extension",
        ],
    );
    fs::write(worktree.join("artifact.txt"), "implementation").expect("artifact");
    run_git(&worktree, ["add", "artifact.txt"]);
    run_git(&worktree, ["commit", "-m", "SYM-90 implementation"]);
    let head_sha = git_output(&worktree, ["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    let issue_branch = "symphony/SYM-90";
    let issue_refspec = format!("HEAD:refs/heads/{issue_branch}");
    run_git(&worktree, ["push", "origin", &issue_refspec]);

    let mut child = Command::new("bash")
        .args(["-c", "exec -a opencode sleep 60"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn fake opencode process");
    let process_id = child.id();

    let config_toml = valid_config_toml()
        .replace(
            "repo_path = \"/home/agent/proj/symphony\"",
            &format!("repo_path = \"{}\"", repo.display()),
        )
        .replace(
            "/home/agent/.symphony/workspaces/opencode/symphony",
            &worktree_root.display().to_string(),
        );
    let config = RootConfig::from_toml_str(&config_toml).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "close-order", "SYM-90"))
        .await
        .expect("running issue");
    let mut session = test_session("symphony", "close-order", "oc-90", &worktree);
    session.process_id = Some(process_id);
    store
        .upsert_opencode_session(session)
        .await
        .expect("running session");

    let client = DoneRequiresStoppedProcessLinearClient::new(
        linear_issue("close-order", "SYM-90", "In Progress", Some(1)),
        process_id,
    );
    let opencode = ScriptedOpenCodeLauncher::new(Some(success_handoff(
        "oc-90",
        &worktree,
        issue_branch,
        &head_sha,
    )));

    let result = daemon::run_once_with_clients(&config, &store, &client, &opencode).await;
    if test_process_is_alive(process_id) {
        child.kill().expect("kill fake opencode process");
    }
    let _ = child.wait();

    assert!(result.is_ok(), "{result:?}");
    assert_eq!(
        client.transitions(),
        vec![("close-order".into(), LinearTransition::Done)]
    );
    assert!(
        !worktree.exists(),
        "Done must be followed by worktree removal"
    );
}

#[tokio::test]
async fn passing_handoff_accepts_force_updated_issue_branch_after_repair() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let repo = dir.path().join("repo");
    let origin = dir.path().join("origin.git");
    fs::create_dir_all(&origin).expect("origin dir");
    run_git(&origin, ["init", "--bare"]);
    fs::create_dir_all(&repo).expect("repo dir");
    run_git(&repo, ["init"]);
    run_git(&repo, ["config", "user.email", "symphony@example.test"]);
    run_git(&repo, ["config", "user.name", "Symphony Test"]);
    run_git(
        &repo,
        [
            "remote",
            "add",
            "origin",
            origin.to_str().expect("origin utf8"),
        ],
    );
    fs::write(repo.join("README.md"), "base checkout").expect("readme");
    run_git(&repo, ["add", "README.md"]);
    run_git(&repo, ["commit", "-m", "base"]);
    run_git(&repo, ["branch", "agent-server/opencode-runner-extension"]);
    run_git(
        &repo,
        ["push", "origin", "agent-server/opencode-runner-extension"],
    );
    let base_sha = git_output(
        &repo,
        ["rev-parse", "agent-server/opencode-runner-extension"],
    )
    .trim()
    .to_owned();

    let worktree_root = dir.path().join("allowed-worktrees");
    let worktree = worktree_root.join("SYM-88");
    run_git(
        &repo,
        [
            "worktree",
            "add",
            "--detach",
            worktree.to_str().expect("worktree path utf8"),
            "agent-server/opencode-runner-extension",
        ],
    );
    let issue_branch = "symphony/SYM-88";
    fs::write(worktree.join("artifact.txt"), "old implementation").expect("old artifact");
    run_git(&worktree, ["add", "artifact.txt"]);
    run_git(&worktree, ["commit", "-m", "SYM-88 old implementation"]);
    let old_sha = git_output(&worktree, ["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    let issue_refspec = format!("HEAD:refs/heads/{issue_branch}");
    run_git(&worktree, ["push", "origin", &issue_refspec]);
    let stale_fetch_refspec =
        format!("refs/heads/{issue_branch}:refs/remotes/origin/{issue_branch}");
    run_git(&repo, ["fetch", "origin", &stale_fetch_refspec]);

    run_git(&worktree, ["reset", "--hard", &base_sha]);
    fs::write(worktree.join("artifact.txt"), "repaired implementation").expect("repaired artifact");
    run_git(&worktree, ["add", "artifact.txt"]);
    run_git(
        &worktree,
        ["commit", "-m", "SYM-88 repaired implementation"],
    );
    let repaired_sha = git_output(&worktree, ["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    run_git(&worktree, ["push", "--force", "origin", &issue_refspec]);
    run_git(
        &repo,
        [
            "update-ref",
            &format!("refs/remotes/origin/{issue_branch}"),
            &old_sha,
        ],
    );
    let stale_remote_sha = git_output(
        &repo,
        ["rev-parse", &format!("refs/remotes/origin/{issue_branch}")],
    );
    assert_ne!(stale_remote_sha.trim(), repaired_sha);

    let config_toml = valid_config_toml()
        .replace(
            "repo_path = \"/home/agent/proj/symphony\"",
            &format!("repo_path = \"{}\"", repo.display()),
        )
        .replace(
            "/home/agent/.symphony/workspaces/opencode/symphony",
            &worktree_root.display().to_string(),
        );
    let config = RootConfig::from_toml_str(&config_toml).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "force-repair", "SYM-88"))
        .await
        .expect("running issue");
    store
        .upsert_opencode_session(test_session("symphony", "force-repair", "oc-88", &worktree))
        .await
        .expect("running session");

    let client = RecordingLinearClient::new(vec![linear_issue(
        "force-repair",
        "SYM-88",
        "In Progress",
        Some(1),
    )]);
    let opencode = ScriptedOpenCodeLauncher::new(Some(success_handoff(
        "oc-88",
        &worktree,
        issue_branch,
        &repaired_sha,
    )));

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert_eq!(
        client.transitions(),
        vec![("force-repair".into(), LinearTransition::Done)]
    );
    assert_eq!(
        git_output(
            &repo,
            ["rev-parse", &format!("refs/remotes/origin/{issue_branch}")]
        )
        .trim(),
        repaired_sha
    );
    assert_eq!(
        git_output(
            &origin,
            ["rev-parse", "agent-server/opencode-runner-extension"]
        )
        .trim(),
        repaired_sha
    );
    assert!(!worktree.exists(), "accepted handoff must remove worktree");
}

#[tokio::test]
async fn successful_handoff_with_unpushed_issue_commit_does_not_close_or_cleanup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let repo = dir.path().join("repo");
    let origin = dir.path().join("origin.git");
    fs::create_dir_all(&origin).expect("origin dir");
    run_git(&origin, ["init", "--bare"]);
    fs::create_dir_all(&repo).expect("repo dir");
    run_git(&repo, ["init"]);
    run_git(&repo, ["config", "user.email", "symphony@example.test"]);
    run_git(&repo, ["config", "user.name", "Symphony Test"]);
    run_git(
        &repo,
        [
            "remote",
            "add",
            "origin",
            origin.to_str().expect("origin utf8"),
        ],
    );
    fs::write(repo.join("README.md"), "base checkout").expect("readme");
    run_git(&repo, ["add", "README.md"]);
    run_git(&repo, ["commit", "-m", "base"]);
    run_git(&repo, ["branch", "agent-server/opencode-runner-extension"]);
    run_git(
        &repo,
        ["push", "origin", "agent-server/opencode-runner-extension"],
    );

    let worktree_root = dir.path().join("allowed-worktrees");
    let worktree = worktree_root.join("SYM-81");
    run_git(
        &repo,
        [
            "worktree",
            "add",
            "--detach",
            worktree.to_str().expect("worktree path utf8"),
            "agent-server/opencode-runner-extension",
        ],
    );
    fs::write(worktree.join("artifact.txt"), "done").expect("artifact");
    run_git(&worktree, ["add", "artifact.txt"]);
    run_git(&worktree, ["commit", "-m", "SYM-81 implementation"]);
    let head_sha = git_output(&worktree, ["rev-parse", "HEAD"])
        .trim()
        .to_owned();

    let config_toml = valid_config_toml()
        .replace(
            "repo_path = \"/home/agent/proj/symphony\"",
            &format!("repo_path = \"{}\"", repo.display()),
        )
        .replace(
            "/home/agent/.symphony/workspaces/opencode/symphony",
            &worktree_root.display().to_string(),
        );
    let config = RootConfig::from_toml_str(&config_toml).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "unpushed", "SYM-81"))
        .await
        .expect("running issue");
    store
        .upsert_opencode_session(test_session("symphony", "unpushed", "oc-81", &worktree))
        .await
        .expect("running session");

    let client = RecordingLinearClient::new(vec![linear_issue(
        "unpushed",
        "SYM-81",
        "In Progress",
        Some(1),
    )]);
    let opencode = ScriptedOpenCodeLauncher::new(Some(success_handoff(
        "oc-81",
        &worktree,
        "symphony/SYM-81",
        &head_sha,
    )));

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert_eq!(
        client.transitions(),
        vec![("unpushed".into(), LinearTransition::Todo)]
    );
    assert!(
        worktree.exists(),
        "unverified git closure must keep worktree"
    );
    assert!(
        client
            .evidence()
            .iter()
            .any(|(_, evidence)| evidence.kind == "malformed_handoff"
                && evidence.body.contains("refs/heads/symphony/SYM-81"))
    );
}

#[tokio::test]
async fn no_code_success_handoff_can_close_without_commit_sha() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    run_git(&repo, ["init"]);
    run_git(&repo, ["config", "user.email", "symphony@example.test"]);
    run_git(&repo, ["config", "user.name", "Symphony Test"]);
    fs::write(repo.join("README.md"), "base checkout").expect("readme");
    run_git(&repo, ["add", "README.md"]);
    run_git(&repo, ["commit", "-m", "base"]);
    run_git(&repo, ["branch", "agent-server/opencode-runner-extension"]);
    let worktree_root = dir.path().join("allowed-worktrees");
    let worktree = worktree_root.join("SYM-79");
    run_git(
        &repo,
        [
            "worktree",
            "add",
            "--detach",
            worktree.to_str().expect("worktree path utf8"),
            "agent-server/opencode-runner-extension",
        ],
    );
    let config_toml = valid_config_toml()
        .replace(
            "repo_path = \"/home/agent/proj/symphony\"",
            &format!("repo_path = \"{}\"", repo.display()),
        )
        .replace(
            "/home/agent/.symphony/workspaces/opencode/symphony",
            &worktree_root.display().to_string(),
        );
    let config = RootConfig::from_toml_str(&config_toml).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "no-code", "SYM-79"))
        .await
        .expect("running issue");
    store
        .upsert_opencode_session(test_session("symphony", "no-code", "oc-79", &worktree))
        .await
        .expect("running session");

    let client = RecordingLinearClient::new(vec![linear_issue(
        "no-code",
        "SYM-79",
        "In Progress",
        Some(1),
    )]);
    let opencode = ScriptedOpenCodeLauncher::new(Some(OpenCodeHandoff {
        session_id: "oc-79".into(),
        lifecycle_stages: vec![
            OpenCodeStage::Running,
            OpenCodeStage::Handoff,
            OpenCodeStage::Completed,
        ],
        subagents: Vec::new(),
        eval_results: vec![OpenCodeEvalResult {
            suite: "symphony-smoke".into(),
            passed: true,
            failure_fingerprint: None,
            details: Some("no-code smoke passed".into()),
        }],
        changed_files: Vec::new(),
        git: Some(GitClosureEvidence {
            branch: "agent-server/opencode-runner-extension".into(),
            head_sha: None,
            pr_url: None,
            worktree_path: worktree.display().to_string(),
        }),
        risks: Vec::new(),
        stop_reason: OpenCodeStopReason::Success,
    }));

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert_eq!(
        client.transitions(),
        vec![("no-code".into(), LinearTransition::Done)]
    );
    let completed = store
        .issue("symphony", "no-code")
        .await
        .expect("query no-code")
        .expect("no-code issue");
    assert_eq!(completed.lifecycle_stage, LifecycleStage::Completed);
    assert_eq!(completed.cleanup_status, CleanupStatus::Complete);
    assert_eq!(
        completed.git_ref.expect("git ref").head_sha.as_deref(),
        None
    );
    let session = store
        .opencode_session("symphony", "no-code", "oc-79")
        .await
        .expect("query no-code session")
        .expect("no-code session");
    assert_eq!(session.lifecycle_stage, LifecycleStage::Completed);
    assert_eq!(session.stage, OpenCodeStage::Completed);
    assert_eq!(session.process_id, None);
    assert!(!worktree.exists(), "no-code success must remove worktree");
}

#[tokio::test]
async fn successful_handoff_with_worktree_outside_configured_root_is_parked_without_cleanup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let allowed_root = dir.path().join("allowed-worktrees");
    let outside = dir.path().join("outside-worktree");
    fs::create_dir_all(&outside).expect("outside worktree");
    fs::write(outside.join("artifact.txt"), "must survive").expect("artifact");
    let config = RootConfig::from_toml_str(&valid_config_toml().replace(
        "/home/agent/.symphony/workspaces/opencode/symphony",
        allowed_root.to_str().expect("allowed root utf8"),
    ))
    .expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "completed", "SYM-80"))
        .await
        .expect("running issue");
    store
        .upsert_opencode_session(test_session("symphony", "completed", "oc-80", &outside))
        .await
        .expect("running session");

    let client = RecordingLinearClient::new(vec![linear_issue(
        "completed",
        "SYM-80",
        "In Progress",
        Some(1),
    )]);
    let opencode = ScriptedOpenCodeLauncher::new(Some(success_handoff(
        "oc-80",
        &outside,
        "agent-server/opencode-runner-extension",
        "abc123def456",
    )));

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert_eq!(
        client.transitions(),
        vec![("completed".into(), LinearTransition::Todo)]
    );
    assert!(outside.exists(), "outside path must not be removed");
    assert!(
        client
            .evidence()
            .iter()
            .any(|(_, evidence)| evidence.kind == "malformed_handoff"
                && evidence.body.contains("outside configured worktree root"))
    );
    assert!(opencode.repairs().is_empty());
    let issue = store
        .issue("symphony", "completed")
        .await
        .expect("query failed")
        .expect("failed issue");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Failed);
}

#[tokio::test]
async fn successful_handoff_with_sibling_worktree_is_parked_without_cleanup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let allowed_root = dir.path().join("allowed-worktrees");
    let active = allowed_root.join("SYM-80");
    let sibling = allowed_root.join("SYM-81");
    fs::create_dir_all(&active).expect("active worktree");
    fs::create_dir_all(&sibling).expect("sibling worktree");
    fs::write(sibling.join("artifact.txt"), "must survive").expect("artifact");
    let config = RootConfig::from_toml_str(&valid_config_toml().replace(
        "/home/agent/.symphony/workspaces/opencode/symphony",
        allowed_root.to_str().expect("allowed root utf8"),
    ))
    .expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "completed", "SYM-80"))
        .await
        .expect("running issue");
    store
        .upsert_opencode_session(test_session("symphony", "completed", "oc-80", &active))
        .await
        .expect("running session");

    let client = RecordingLinearClient::new(vec![linear_issue(
        "completed",
        "SYM-80",
        "In Progress",
        Some(1),
    )]);
    let opencode = ScriptedOpenCodeLauncher::new(Some(success_handoff(
        "oc-80",
        &sibling,
        "agent-server/opencode-runner-extension",
        "abc123def456",
    )));

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert_eq!(
        client.transitions(),
        vec![("completed".into(), LinearTransition::Todo)]
    );
    assert!(active.exists(), "active worktree must not be removed");
    assert!(sibling.exists(), "sibling worktree must not be removed");
    assert!(client.evidence().iter().any(|(_, evidence)| {
        evidence.kind == "malformed_handoff"
            && evidence
                .body
                .contains("does not match active session worktree")
    }));
    assert!(opencode.repairs().is_empty());
    let issue = store
        .issue("symphony", "completed")
        .await
        .expect("query failed")
        .expect("failed issue");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Failed);
}

#[tokio::test]
async fn successful_handoff_with_whitespace_worktree_path_is_parked_without_cleanup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let allowed_root = dir.path().join("allowed-worktrees");
    let active = allowed_root.join("SYM-80");
    fs::create_dir_all(&active).expect("active worktree");
    fs::write(active.join("artifact.txt"), "must survive").expect("artifact");
    let config = RootConfig::from_toml_str(&valid_config_toml().replace(
        "/home/agent/.symphony/workspaces/opencode/symphony",
        allowed_root.to_str().expect("allowed root utf8"),
    ))
    .expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "completed", "SYM-80"))
        .await
        .expect("running issue");
    store
        .upsert_opencode_session(test_session("symphony", "completed", "oc-80", &active))
        .await
        .expect("running session");

    let client = RecordingLinearClient::new(vec![linear_issue(
        "completed",
        "SYM-80",
        "In Progress",
        Some(1),
    )]);
    let opencode = ScriptedOpenCodeLauncher::new(Some(success_handoff(
        "oc-80",
        PathBuf::from(format!("{} ", active.display())),
        "agent-server/opencode-runner-extension",
        "abc123def456",
    )));

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert_eq!(
        client.transitions(),
        vec![("completed".into(), LinearTransition::Todo)]
    );
    assert!(active.exists(), "active worktree must not be removed");
    assert!(client.evidence().iter().any(|(_, evidence)| {
        evidence.kind == "malformed_handoff"
            && evidence.body.contains("leading or trailing whitespace")
    }));
    assert!(opencode.repairs().is_empty());
    let issue = store
        .issue("symphony", "completed")
        .await
        .expect("query failed")
        .expect("failed issue");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Failed);
}

#[tokio::test]
async fn eval_failure_stays_in_opencode_repair_loop_without_linear_churn() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let worktree = dir.path().join("SYM-81-worktree");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "repair", "SYM-81"))
        .await
        .expect("running issue");
    store
        .upsert_opencode_session(test_session("symphony", "repair", "oc-81", &worktree))
        .await
        .expect("running session");
    let client = RecordingLinearClient::new(vec![linear_issue(
        "repair",
        "SYM-81",
        "In Progress",
        Some(1),
    )]);
    let opencode =
        ScriptedOpenCodeLauncher::new(Some(eval_failed_handoff("oc-81", "fmt-check-7f")));

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert!(client.transitions().is_empty());
    assert_eq!(
        opencode.repairs(),
        vec![("oc-81".into(), "fmt-check-7f".into())]
    );
    let issue = store
        .issue("symphony", "repair")
        .await
        .expect("query repair")
        .expect("repair issue");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Running);
    let failure = issue.failure.expect("failure");
    assert_eq!(failure.kind, "eval_failure");
    assert_eq!(failure.fingerprint.as_deref(), Some("fmt-check-7f"));
    assert_eq!(failure.occurrence_count, 1);
}

#[tokio::test]
async fn repeated_identical_eval_failure_parks_owner_input_with_typed_evidence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let worktree = dir.path().join("SYM-82-worktree");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let mut issue = test_issue("symphony", "repeat", "SYM-82");
    issue.failure = Some(FailureRecord {
        kind: "eval_failure".into(),
        message: "lint-loop".into(),
        fingerprint: Some("lint-loop".into()),
        occurrence_count: 1,
    });
    store.upsert_issue(issue).await.expect("running issue");
    store
        .upsert_opencode_session(test_session("symphony", "repeat", "oc-82", &worktree))
        .await
        .expect("running session");
    let client = RecordingLinearClient::new(vec![linear_issue(
        "repeat",
        "SYM-82",
        "In Progress",
        Some(1),
    )]);
    let opencode = ScriptedOpenCodeLauncher::new(Some(eval_failed_handoff("oc-82", "lint-loop")));

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert_eq!(
        client.transitions(),
        vec![("repeat".into(), LinearTransition::NeedOwnerInput)]
    );
    assert!(opencode.repairs().is_empty());
    let issue = store
        .issue("symphony", "repeat")
        .await
        .expect("query repeat")
        .expect("repeat issue");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Blocked);
    assert_eq!(
        issue.blocker.expect("blocker").kind,
        "repeated_eval_failure"
    );
}

#[tokio::test]
async fn provider_blocker_and_owner_question_park_with_owner_visible_question() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");

    let cases = [
        (
            "provider",
            "SYM-83",
            OpenCodeHandoff {
                session_id: "oc-provider".into(),
                lifecycle_stages: vec![OpenCodeStage::Running, OpenCodeStage::Failed],
                subagents: vec!["rust-engineer".into()],
                eval_results: Vec::new(),
                changed_files: Vec::new(),
                git: None,
                risks: vec!["provider quota exhausted".into()],
                stop_reason: OpenCodeStopReason::ProviderBlocker {
                    message: "provider quota exhausted".into(),
                },
            },
            "provider_blocker",
        ),
        (
            "owner",
            "SYM-84",
            OpenCodeHandoff {
                session_id: "oc-owner".into(),
                lifecycle_stages: vec![OpenCodeStage::Running, OpenCodeStage::Handoff],
                subagents: vec!["rust-engineer".into()],
                eval_results: Vec::new(),
                changed_files: Vec::new(),
                git: None,
                risks: Vec::new(),
                stop_reason: OpenCodeStopReason::OwnerQuestion {
                    question: "Which branch should receive the PR?".into(),
                },
            },
            "owner_question",
        ),
    ];

    for (issue_id, identifier, handoff, expected_kind) in cases {
        let worktree = dir.path().join(format!("{identifier}-worktree"));
        store
            .upsert_issue(test_issue("symphony", issue_id, identifier))
            .await
            .expect("running issue");
        store
            .upsert_opencode_session(test_session(
                "symphony",
                issue_id,
                &handoff.session_id,
                &worktree,
            ))
            .await
            .expect("running session");
        let client = RecordingLinearClient::new(vec![linear_issue(
            issue_id,
            identifier,
            "In Progress",
            Some(1),
        )]);
        let opencode = ScriptedOpenCodeLauncher::new(Some(handoff));

        daemon::run_once_with_clients(&config, &store, &client, &opencode)
            .await
            .expect("orchestrate once");

        assert_eq!(
            client.transitions(),
            vec![(issue_id.into(), LinearTransition::NeedOwnerInput)]
        );
        assert!(client.evidence().iter().any(|(_, evidence)| {
            evidence.kind == expected_kind
                && (evidence.body.contains("Owner input needed")
                    || evidence
                        .body
                        .contains("Which branch should receive the PR?"))
        }));
        let issue = store
            .issue("symphony", issue_id)
            .await
            .expect("query parked")
            .expect("parked issue");
        assert_eq!(issue.lifecycle_stage, LifecycleStage::Blocked);
        assert_eq!(issue.blocker.expect("blocker").kind, expected_kind);
    }
}

#[tokio::test]
async fn malformed_success_handoff_fails_fast_without_opencode_repair_or_owner_input() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let worktree = dir.path().join("SYM-85-worktree");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "malformed", "SYM-85"))
        .await
        .expect("running issue");
    store
        .upsert_opencode_session(test_session(
            "symphony",
            "malformed",
            "oc-malformed",
            &worktree,
        ))
        .await
        .expect("running session");
    let client = RecordingLinearClient::new(vec![linear_issue(
        "malformed",
        "SYM-85",
        "In Progress",
        Some(1),
    )]);
    let opencode = ScriptedOpenCodeLauncher::new(Some(OpenCodeHandoff {
        session_id: "oc-malformed".into(),
        lifecycle_stages: vec![OpenCodeStage::Eval, OpenCodeStage::Handoff],
        subagents: vec!["rust-engineer".into()],
        eval_results: vec![OpenCodeEvalResult {
            suite: "cargo test".into(),
            passed: true,
            failure_fingerprint: None,
            details: None,
        }],
        changed_files: vec!["crates/symphony/src/opencode.rs".into()],
        git: None,
        risks: Vec::new(),
        stop_reason: OpenCodeStopReason::Success,
    }));

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert_eq!(
        client.transitions(),
        vec![("malformed".into(), LinearTransition::Todo)]
    );
    assert!(client.evidence().iter().any(|(_, evidence)| {
        evidence.kind == "malformed_handoff"
            && evidence
                .body
                .contains("successful handoff did not include git closure evidence")
    }));
    assert!(opencode.repairs().is_empty());
    let issue = store
        .issue("symphony", "malformed")
        .await
        .expect("query issue")
        .expect("issue");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Failed);
    assert!(issue.blocker.is_none());
    assert_eq!(
        issue.failure.expect("failure").fingerprint.as_deref(),
        Some("incomplete_success_handoff")
    );
    let session = store
        .opencode_session("symphony", "malformed", "oc-malformed")
        .await
        .expect("query session")
        .expect("session");
    assert_eq!(session.lifecycle_stage, LifecycleStage::Failed);
    assert_eq!(session.stage, OpenCodeStage::Failed);
    assert_eq!(session.process_id, None);
    assert_eq!(
        session.lifecycle_marker.as_deref(),
        Some("failed:malformed_handoff")
    );
}

#[tokio::test]
async fn malformed_handoff_sidecar_fails_fast_kills_process_tree_and_does_not_repair() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let worktree = dir.path().join("SYM-86-worktree");
    store
        .upsert_issue(test_issue("symphony", "malformed-json", "SYM-86"))
        .await
        .expect("running issue");
    let mut stale_process = Command::new("bash")
        .arg("-c")
        .arg("exec -a opencode sleep 120")
        .spawn()
        .expect("spawn stale opencode-shaped process");
    thread::sleep(Duration::from_millis(100));
    let stale_process_id = stale_process.id();
    store
        .upsert_opencode_session({
            let mut session = test_session("symphony", "malformed-json", "oc-86", &worktree);
            session.process_id = Some(stale_process_id);
            session
        })
        .await
        .expect("running session");
    let client = RecordingLinearClient::new(vec![linear_issue(
        "malformed-json",
        "SYM-86",
        "In Progress",
        Some(1),
    )]);
    let opencode = MalformedHandoffOpenCodeLauncher::new(
        "opencode-handoff.json: unknown field `status`, expected `session_id`",
    );

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("malformed handoff must not fail the whole poll");

    let mut stale_terminated = false;
    for _ in 0..20 {
        if stale_process
            .try_wait()
            .expect("poll stale process")
            .is_some()
        {
            stale_terminated = true;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    if !stale_terminated {
        let _ = stale_process.kill();
        let _ = stale_process.wait();
    }
    assert!(
        stale_terminated,
        "malformed handoff must terminate the previous active ACP process"
    );
    assert_eq!(
        client.transitions(),
        vec![("malformed-json".into(), LinearTransition::Todo)]
    );
    assert!(client.evidence().iter().any(|(_, evidence)| {
        evidence.kind == "malformed_handoff" && evidence.body.contains("unknown field `status`")
    }));
    let issue = store
        .issue("symphony", "malformed-json")
        .await
        .expect("query malformed")
        .expect("malformed issue");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Failed);
    assert!(issue.blocker.is_none());
    let failure = issue.failure.expect("failure");
    assert_eq!(failure.kind, "malformed_handoff");
    assert_eq!(
        failure.fingerprint.as_deref(),
        Some("malformed_handoff_sidecar")
    );
    assert!(failure.message.contains("unknown field `status`"));
    assert!(opencode.repairs().is_empty());
    let session = store
        .opencode_session("symphony", "malformed-json", "oc-86")
        .await
        .expect("query session")
        .expect("session");
    assert_eq!(session.lifecycle_stage, LifecycleStage::Failed);
    assert_eq!(session.stage, OpenCodeStage::Failed);
    assert_eq!(session.process_id, None);
    assert_eq!(
        session.lifecycle_marker.as_deref(),
        Some("failed:malformed_handoff")
    );
    assert_eq!(
        session.last_event.as_deref(),
        Some("failed:malformed_handoff_sidecar")
    );
}

#[tokio::test]
async fn dead_in_progress_session_without_handoff_sidecar_fails_fast_instead_of_reusing_session() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let worktree = dir.path().join("SYM-87-worktree");
    fs::create_dir_all(&worktree).expect("worktree");
    store
        .upsert_issue(test_issue("symphony", "missing-sidecar", "SYM-87"))
        .await
        .expect("running issue");
    store
        .upsert_opencode_session({
            let mut session = test_session("symphony", "missing-sidecar", "oc-87", &worktree);
            session.process_id = None;
            session
        })
        .await
        .expect("running session");
    let client = RecordingLinearClient::new(vec![linear_issue(
        "missing-sidecar",
        "SYM-87",
        "In Progress",
        Some(1),
    )]);
    let opencode = ScriptedOpenCodeLauncher::new(None);

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert_eq!(
        client.transitions(),
        vec![("missing-sidecar".into(), LinearTransition::Todo)]
    );
    assert!(client.evidence().iter().any(|(_, evidence)| {
        evidence.kind == "malformed_handoff"
            && evidence
                .body
                .contains(".symphony/opencode-handoff.json was not produced")
    }));
    assert!(opencode.repairs().is_empty());
    let issue = store
        .issue("symphony", "missing-sidecar")
        .await
        .expect("query issue")
        .expect("issue");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Failed);
    assert_eq!(
        issue.failure.expect("failure").fingerprint.as_deref(),
        Some("missing_handoff_sidecar")
    );
    let session = store
        .opencode_session("symphony", "missing-sidecar", "oc-87")
        .await
        .expect("query session")
        .expect("session");
    assert_eq!(session.lifecycle_stage, LifecycleStage::Failed);
    assert_eq!(session.stage, OpenCodeStage::Failed);
    assert_eq!(session.process_id, None);
}

#[tokio::test]
async fn orchestration_processes_multiple_projects_in_config_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(two_project_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");

    let client = ProjectAwareLinearClient::new([
        (
            "alpha",
            vec![linear_issue("alpha-work", "ALPHA-1", "Todo", Some(1))],
        ),
        (
            "symphony",
            vec![linear_issue("symphony-work", "SYM-70", "Todo", Some(1))],
        ),
    ]);

    let report = daemon::run_once_with_linear_client(&config, &store, &client)
        .await
        .expect("orchestrate once");

    assert_eq!(report.dispatched, vec!["ALPHA-1", "SYM-70"]);
    assert_eq!(
        client.transitions(),
        vec![
            ("alpha-work".into(), LinearTransition::InProgress),
            ("symphony-work".into(), LinearTransition::InProgress),
        ]
    );
}
