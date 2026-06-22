use super::*;

#[derive(Debug)]
struct DoneRequiresStoppedProcessLinearClient {
    issue: LinearIssue,
    process_id: u32,
    transitions: std::sync::Mutex<Vec<(String, LinearTransition)>>,
    evidence: std::sync::Mutex<Vec<(String, LinearIssueEvidence)>>,
}

impl DoneRequiresStoppedProcessLinearClient {
    const fn new(issue: LinearIssue, process_id: u32) -> Self {
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

#[derive(Debug)]
struct MismatchedHandoffOpenCodeLauncher {
    handoff: OpenCodeHandoff,
    repairs: std::sync::Mutex<Vec<(String, String)>>,
}

impl MismatchedHandoffOpenCodeLauncher {
    fn new(handoff: OpenCodeHandoff) -> Self {
        Self {
            handoff,
            repairs: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn repairs(&self) -> Vec<(String, String)> {
        self.repairs.lock().expect("repairs lock").clone()
    }
}

#[async_trait::async_trait]
impl OpenCodeLauncher for MismatchedHandoffOpenCodeLauncher {
    async fn launch(
        &self,
        _spec: &opencode::OpenCodeLaunchSpec,
    ) -> Result<opencode::OpenCodeStartedSession, opencode::OpenCodeError> {
        unreachable!("mismatched handoff test should not launch OpenCode")
    }

    async fn latest_handoff(
        &self,
        _session: &OpenCodeSessionRecord,
    ) -> Result<Option<OpenCodeHandoff>, opencode::OpenCodeError> {
        Ok(Some(self.handoff.clone()))
    }

    async fn continue_repair(
        &self,
        _spec: &opencode::OpenCodeLaunchSpec,
        session: &OpenCodeSessionRecord,
        failure_fingerprint: &str,
        _repair_message: &str,
    ) -> Result<opencode::OpenCodeStartedSession, opencode::OpenCodeError> {
        self.repairs
            .lock()
            .expect("repairs lock")
            .push((session.session_id.clone(), failure_fingerprint.to_string()));
        Ok(opencode::OpenCodeStartedSession {
            session_id: session.session_id.clone(),
            process_id: session.process_id,
            acp_frame_count: 0,
            session_evidence_refs: Vec::new(),
        })
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
        ["checkout", "agent-server/opencode-runner-extension"],
    );
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
    let handoff_comment = client
        .evidence()
        .into_iter()
        .find_map(|(_, evidence)| {
            (evidence.kind == "opencode_git_closure").then_some(evidence.body)
        })
        .expect("accepted handoff comment");
    assert!(handoff_comment.contains("## OpenCode Handoff Accepted"));
    assert!(handoff_comment.contains("### Validation"));
    assert!(handoff_comment.contains("### Changed Files"));
    assert!(handoff_comment.contains(&head_sha));
    assert!(handoff_comment.contains("agent-server/opencode-runner-extension"));
    assert!(handoff_comment.contains("`cargo test` passed - ok"));
    assert!(
        !handoff_comment.contains("session_id:"),
        "{handoff_comment}"
    );
    assert!(
        !handoff_comment.contains("changed_files:"),
        "{handoff_comment}"
    );
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
async fn todo_issue_with_recoverable_failed_success_handoff_closes_without_new_launch() {
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
        ["checkout", "agent-server/opencode-runner-extension"],
    );
    run_git(
        &repo,
        ["push", "origin", "agent-server/opencode-runner-extension"],
    );
    let worktree_root = dir.path().join("allowed-worktrees");
    let worktree = worktree_root.join("SYM-211");
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
    run_git(&worktree, ["commit", "-m", "SYM-211 implementation"]);
    let head_sha = git_output(&worktree, ["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    let issue_branch = "symphony/SYM-211";
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
    let mut issue_record = test_issue("symphony", "recoverable", "SYM-211");
    issue_record.lifecycle_stage = LifecycleStage::Failed;
    issue_record.failure = Some(FailureRecord {
        kind: "malformed_handoff".into(),
        message: "eval `opencode-evaluation` did not pass".into(),
        fingerprint: Some("incomplete_success_handoff".into()),
        occurrence_count: 1,
    });
    store.upsert_issue(issue_record).await.expect("issue");
    let mut session = test_session("symphony", "recoverable", "oc-211", &worktree);
    session.lifecycle_stage = LifecycleStage::Failed;
    session.stage = OpenCodeStage::Failed;
    session.lifecycle_marker = Some("failed:malformed_handoff".into());
    session.last_event = Some(format!(
        "failed:incomplete_success_handoff:git_head:{}",
        &head_sha[..12]
    ));
    store
        .upsert_opencode_session(session)
        .await
        .expect("failed session");
    store
        .record_self_defect_occurrence(&SelfDefectOccurrenceRecord {
            fingerprint: "incomplete_success_handoff".into(),
            defect_kind: "malformed_handoff".into(),
            category: "handoff".into(),
            severity: "p0".into(),
            initial_routing_decision: "managed_self_defect".into(),
            source_project_id: "symphony".into(),
            source_issue_id: "recoverable".into(),
            source_issue_identifier: "SYM-211".into(),
            source_session_id: Some("oc-211".into()),
            source_process_id: Some(1234),
            managed_issue_id: "managed-self-defect".into(),
            managed_issue_identifier: "SYM-212".into(),
            latest_evidence_summary: "stale self-defect registry row".into(),
            relation_mode: SelfDefectRelationMode::Blocking,
        })
        .await
        .expect("self-defect registry row");

    let client = RecordingLinearClient::new(vec![linear_issue(
        "recoverable",
        "SYM-211",
        "Todo",
        Some(1),
    )]);
    let opencode = ScriptedOpenCodeLauncher::new(Some(success_handoff(
        "oc-211",
        &worktree,
        issue_branch,
        &head_sha,
    )));

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert!(opencode.launches().is_empty());
    assert_eq!(
        client.transitions(),
        vec![("recoverable".into(), LinearTransition::Done)]
    );
    let completed = store
        .issue("symphony", "recoverable")
        .await
        .expect("query completed")
        .expect("completed issue");
    assert_eq!(completed.lifecycle_stage, LifecycleStage::Completed);
    assert_eq!(completed.cleanup_status, CleanupStatus::Complete);
    assert!(
        !worktree.exists(),
        "accepted recovered handoff must remove worktree"
    );
    assert!(
        store
            .open_self_defect_by_fingerprint("incomplete_success_handoff")
            .await
            .expect("query open self-defect")
            .is_none(),
        "accepted recovered handoff must resolve the stale managed self-defect blocker"
    );
    let resolved = store
        .latest_self_defect_by_fingerprint("incomplete_success_handoff")
        .await
        .expect("query resolved self-defect")
        .expect("resolved self-defect row");
    assert_eq!(
        resolved.resolution_state,
        symphony::state::SelfDefectResolutionState::Done
    );
}

#[tokio::test]
async fn passing_handoff_closes_when_canonical_checkout_has_unrelated_dirty_files() {
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
        ["checkout", "agent-server/opencode-runner-extension"],
    );
    run_git(
        &repo,
        ["push", "origin", "agent-server/opencode-runner-extension"],
    );

    let worktree_root = dir.path().join("allowed-worktrees");
    let worktree = worktree_root.join("SYM-83");
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
    run_git(&worktree, ["commit", "-m", "SYM-83 implementation"]);
    let head_sha = git_output(&worktree, ["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    let issue_branch = "symphony/SYM-83";
    let issue_refspec = format!("HEAD:refs/heads/{issue_branch}");
    run_git(&worktree, ["push", "origin", &issue_refspec]);

    fs::write(repo.join("README.md"), "unrelated operator edits").expect("dirty readme");

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
        .upsert_issue(test_issue("symphony", "dirty-canonical", "SYM-83"))
        .await
        .expect("running issue");
    store
        .upsert_opencode_session(test_session(
            "symphony",
            "dirty-canonical",
            "oc-83",
            &worktree,
        ))
        .await
        .expect("running session");

    let client = RecordingLinearClient::new(vec![linear_issue(
        "dirty-canonical",
        "SYM-83",
        "In Progress",
        Some(1),
    )]);
    let opencode = ScriptedOpenCodeLauncher::new(Some(success_handoff(
        "oc-83",
        &worktree,
        issue_branch,
        &head_sha,
    )));

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert_eq!(
        client.transitions(),
        vec![("dirty-canonical".into(), LinearTransition::Done)]
    );
    assert!(
        client
            .evidence()
            .iter()
            .all(|(_, evidence)| evidence.kind != "malformed_handoff"),
        "unrelated canonical dirt must not become git closure failure"
    );
    let completed = store
        .issue("symphony", "dirty-canonical")
        .await
        .expect("query dirty canonical issue")
        .expect("dirty canonical issue");
    assert_eq!(completed.lifecycle_stage, LifecycleStage::Completed);
    assert_eq!(completed.cleanup_status, CleanupStatus::Complete);
    assert!(!worktree.exists(), "accepted worktree should be cleaned up");
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
async fn passing_handoff_merges_pushed_issue_branch_when_base_advanced() {
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
        ["checkout", "agent-server/opencode-runner-extension"],
    );
    run_git(
        &repo,
        ["push", "origin", "agent-server/opencode-runner-extension"],
    );

    let worktree_root = dir.path().join("allowed-worktrees");
    let worktree = worktree_root.join("SYM-89");
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
    let issue_branch = "symphony/SYM-89";
    fs::write(worktree.join("artifact.txt"), "implementation").expect("artifact");
    run_git(&worktree, ["add", "artifact.txt"]);
    run_git(&worktree, ["commit", "-m", "SYM-89 implementation"]);
    let head_sha = git_output(&worktree, ["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    let issue_refspec = format!("HEAD:refs/heads/{issue_branch}");
    run_git(&worktree, ["push", "origin", &issue_refspec]);

    fs::write(repo.join("base-advanced.txt"), "new base work").expect("base advance");
    run_git(&repo, ["add", "base-advanced.txt"]);
    run_git(&repo, ["commit", "-m", "advance base"]);
    run_git(
        &repo,
        ["push", "origin", "agent-server/opencode-runner-extension"],
    );
    assert_ne!(
        git_output(
            &origin,
            ["rev-parse", "agent-server/opencode-runner-extension"]
        )
        .trim(),
        head_sha
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
        .upsert_issue(test_issue("symphony", "stale-base", "SYM-89"))
        .await
        .expect("running issue");
    store
        .upsert_opencode_session(test_session("symphony", "stale-base", "oc-89", &worktree))
        .await
        .expect("running session");

    let client = RecordingLinearClient::new(vec![linear_issue(
        "stale-base",
        "SYM-89",
        "In Progress",
        Some(1),
    )]);
    let opencode = ScriptedOpenCodeLauncher::new(Some(success_handoff(
        "oc-89",
        &worktree,
        issue_branch,
        &head_sha,
    )));

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert_eq!(
        client.transitions(),
        vec![("stale-base".into(), LinearTransition::Done)]
    );
    let base_head = git_output(
        &origin,
        ["rev-parse", "agent-server/opencode-runner-extension"],
    );
    assert_ne!(base_head.trim(), head_sha);
    assert!(
        git_output(
            &repo,
            ["merge-base", "--is-ancestor", &head_sha, base_head.trim()]
        )
        .trim()
        .is_empty(),
        "integrated base must contain issue commit"
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

    assert!(client.transitions().is_empty());
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
    assert_eq!(
        opencode.repairs(),
        vec![("oc-81".into(), "git_closure_unverified".into())]
    );
    let issue = store
        .issue("symphony", "unpushed")
        .await
        .expect("query unpushed")
        .expect("unpushed issue");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Running);
    assert!(issue.blocker.is_none());
    assert_eq!(
        issue.failure.expect("failure").fingerprint.as_deref(),
        Some("git_closure_unverified")
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
            evidence_ref: None,
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
async fn no_change_handoff_with_unreported_commit_does_not_close_or_cleanup() {
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
    let worktree = worktree_root.join("SYM-82");
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
    fs::write(worktree.join("artifact.txt"), "unreported").expect("artifact");
    run_git(&worktree, ["add", "artifact.txt"]);
    run_git(
        &worktree,
        ["commit", "-m", "SYM-82 unreported implementation"],
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
        .upsert_issue(test_issue("symphony", "unreported", "SYM-82"))
        .await
        .expect("running issue");
    store
        .upsert_opencode_session(test_session("symphony", "unreported", "oc-82", &worktree))
        .await
        .expect("running session");

    let client = RecordingLinearClient::new(vec![linear_issue(
        "unreported",
        "SYM-82",
        "In Progress",
        Some(1),
    )]);
    let opencode = ScriptedOpenCodeLauncher::new(Some(OpenCodeHandoff {
        session_id: "oc-82".into(),
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
            details: Some("claimed no git changes".into()),
            evidence_ref: None,
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

    assert!(client.transitions().is_empty());
    assert!(
        worktree.exists(),
        "unreported committed work must not be cleaned up"
    );
    assert!(client.evidence().iter().any(|(_, evidence)| {
        evidence.kind == "malformed_handoff"
            && evidence
                .body
                .contains("no-change handoff omitted git.head_sha")
    }));
    assert_eq!(
        opencode.repairs(),
        vec![("oc-82".into(), "git_closure_unverified".into())]
    );
    let issue = store
        .issue("symphony", "unreported")
        .await
        .expect("query unreported")
        .expect("unreported issue");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Running);
    assert!(issue.blocker.is_none());
    let failure = issue.failure.expect("failure");
    assert_eq!(failure.kind, "malformed_handoff");
    assert_eq!(
        failure.fingerprint.as_deref(),
        Some("git_closure_unverified")
    );
    assert_eq!(failure.occurrence_count, 1);
    let session = store
        .opencode_session("symphony", "unreported", "oc-82")
        .await
        .expect("query unreported session")
        .expect("unreported session");
    assert_eq!(session.lifecycle_stage, LifecycleStage::Running);
    assert_eq!(session.stage, OpenCodeStage::Running);
    assert_eq!(session.lifecycle_marker.as_deref(), Some("repair_prompted"));
    assert!(
        session
            .last_event
            .as_deref()
            .is_some_and(|event| event.starts_with("repair_prompted:git_closure_unverified")),
        "last_event={:?}",
        session.last_event
    );
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

    assert_backlog_transition(&client.transitions(), "completed");
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
        .expect("query repair")
        .expect("repair issue");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Failed);
    assert_eq!(
        issue.blocker.as_ref().expect("runtime defect blocker").kind,
        "runtime_defect"
    );
    assert_eq!(
        issue.failure.expect("failure").fingerprint.as_deref(),
        Some("unsafe_worktree_path")
    );
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

    assert_backlog_transition(&client.transitions(), "completed");
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
        .expect("query repair")
        .expect("repair issue");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Failed);
    assert_eq!(
        issue.blocker.as_ref().expect("runtime defect blocker").kind,
        "runtime_defect"
    );
    assert_eq!(
        issue.failure.expect("failure").fingerprint.as_deref(),
        Some("unsafe_worktree_path")
    );
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

    assert_backlog_transition(&client.transitions(), "completed");
    assert!(active.exists(), "active worktree must not be removed");
    assert!(client.evidence().iter().any(|(_, evidence)| {
        evidence.kind == "malformed_handoff"
            && evidence.body.contains("leading or trailing whitespace")
    }));
    assert!(opencode.repairs().is_empty());
    let issue = store
        .issue("symphony", "completed")
        .await
        .expect("query repair")
        .expect("repair issue");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Failed);
    assert_eq!(
        issue.blocker.as_ref().expect("runtime defect blocker").kind,
        "runtime_defect"
    );
    assert_eq!(
        issue.failure.expect("failure").fingerprint.as_deref(),
        Some("unsafe_worktree_path")
    );
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
    assert!(client.managed_issues().is_empty());
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
async fn repeated_identical_eval_failure_parks_typed_blocker_with_typed_evidence() {
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

    assert!(client.transitions().is_empty());
    assert!(client.managed_issues().is_empty());
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

    let client =
        RecordingLinearClient::new(vec![linear_issue("repeat", "SYM-82", "Todo", Some(1))]);
    let opencode = ScriptedOpenCodeLauncher::new(None);

    let report = daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate requeued repeated eval failure");

    assert_eq!(report.blocked, vec!["SYM-82"]);
    assert!(client.transitions().is_empty());
    assert!(client.managed_issues().is_empty());
    assert!(opencode.repairs().is_empty());
    let issue = store
        .issue("symphony", "repeat")
        .await
        .expect("query retained repeat")
        .expect("retained repeat issue");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Blocked);
    assert_eq!(
        issue.blocker.expect("retained blocker").kind,
        "repeated_eval_failure"
    );
}

#[tokio::test]
async fn repeated_session_id_mismatch_hits_runtime_repair_threshold() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let worktree = dir.path().join("SYM-83-worktree");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let mut issue = test_issue("symphony", "session-mismatch", "SYM-83");
    issue.failure = Some(FailureRecord {
        kind: "malformed_handoff".into(),
        message: "session id mismatch".into(),
        fingerprint: Some("session_id_mismatch".into()),
        occurrence_count: 1,
    });
    store.upsert_issue(issue).await.expect("running issue");
    store
        .upsert_opencode_session(test_session(
            "symphony",
            "session-mismatch",
            "oc-83",
            &worktree,
        ))
        .await
        .expect("running session");
    let client = RecordingLinearClient::new(vec![linear_issue(
        "session-mismatch",
        "SYM-83",
        "In Progress",
        Some(1),
    )]);
    let opencode = MismatchedHandoffOpenCodeLauncher::new(success_handoff(
        "stale-oc-83",
        &worktree,
        "agent-server/opencode-runner-extension",
        "abc123",
    ));

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert!(opencode.repairs().is_empty());
    assert_backlog_transition(&client.transitions(), "session-mismatch");
    assert!(client.evidence().iter().any(|(_, evidence)| {
        evidence.kind == "malformed_handoff"
            && evidence.body.contains("reached bounded repair threshold")
    }));
    let issue = store
        .issue("symphony", "session-mismatch")
        .await
        .expect("query session mismatch")
        .expect("session mismatch issue");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Failed);
    assert_eq!(
        issue.blocker.as_ref().expect("blocker").kind,
        "runtime_defect"
    );
    let failure = issue.failure.expect("failure");
    assert_eq!(failure.kind, "malformed_handoff");
    assert_eq!(failure.fingerprint.as_deref(), Some("session_id_mismatch"));
    assert_eq!(failure.occurrence_count, 2);
}

#[tokio::test]
async fn provider_blocker_parks_without_need_owner_input() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");

    let issue_id = "provider";
    let identifier = "SYM-83";
    let worktree = dir.path().join(format!("{identifier}-worktree"));
    store
        .upsert_issue(test_issue("symphony", issue_id, identifier))
        .await
        .expect("running issue");
    let mut provider_process = Command::new("bash")
        .arg("-c")
        .arg("exec -a opencode sleep 120")
        .spawn()
        .expect("spawn provider blocker opencode-shaped process");
    thread::sleep(Duration::from_millis(100));
    let provider_process_id = provider_process.id();
    store
        .upsert_opencode_session({
            let mut session = test_session("symphony", issue_id, "oc-provider", &worktree);
            session.process_id = Some(provider_process_id);
            session
        })
        .await
        .expect("running session");
    let client = RecordingLinearClient::new(vec![linear_issue(
        issue_id,
        identifier,
        "In Progress",
        Some(1),
    )]);
    let opencode = ScriptedOpenCodeLauncher::new(Some(OpenCodeHandoff {
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
    }));

    let report = daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");
    let provider_process_still_alive = test_process_is_alive(provider_process_id);
    if provider_process_still_alive {
        provider_process
            .kill()
            .expect("cleanup provider blocker process");
    }
    let _ = provider_process.wait();

    assert!(report.parked_owner_input.is_empty());
    assert!(client.transitions().is_empty());
    let evidence = client.evidence();
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].1.kind, "provider_blocker");
    assert_eq!(evidence[0].1.body, "provider quota exhausted");
    assert!(!evidence[0].1.body.contains("Owner input needed"));
    let issue = store
        .issue("symphony", issue_id)
        .await
        .expect("query parked")
        .expect("parked issue");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Blocked);
    assert_eq!(
        issue.blocker.as_ref().expect("blocker").kind,
        "provider_blocker"
    );
    let parked_session = store
        .opencode_session("symphony", issue_id, "oc-provider")
        .await
        .expect("query parked session")
        .expect("parked session");
    assert_eq!(parked_session.process_id, None);
    assert!(
        !provider_process_still_alive,
        "provider blocker parking must terminate the active OpenCode process tree"
    );
}

#[tokio::test]
async fn owner_question_parks_with_owner_visible_question() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");

    let issue_id = "owner";
    let identifier = "SYM-84";
    let worktree = dir.path().join(format!("{identifier}-worktree"));
    store
        .upsert_issue(test_issue("symphony", issue_id, identifier))
        .await
        .expect("running issue");
    store
        .upsert_opencode_session(test_session("symphony", issue_id, "oc-owner", &worktree))
        .await
        .expect("running session");
    let client = RecordingLinearClient::new(vec![linear_issue(
        issue_id,
        identifier,
        "In Progress",
        Some(1),
    )]);
    let opencode = ScriptedOpenCodeLauncher::new(Some(OpenCodeHandoff {
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
    }));

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert_eq!(
        client.transitions(),
        vec![(issue_id.into(), LinearTransition::NeedOwnerInput)]
    );
    assert!(client.managed_issues().is_empty());
    assert!(client.evidence().iter().any(|(_, evidence)| {
        evidence.kind == "owner_question"
            && evidence
                .body
                .contains("Which branch should receive the PR?")
    }));
    let issue = store
        .issue("symphony", issue_id)
        .await
        .expect("query parked")
        .expect("parked issue");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Blocked);
    assert_eq!(issue.blocker.expect("blocker").kind, "owner_question");
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
            evidence_ref: None,
        }],
        changed_files: vec!["crates/symphony/src/opencode.rs".into()],
        git: None,
        risks: Vec::new(),
        stop_reason: OpenCodeStopReason::Success,
    }));

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert_backlog_transition(&client.transitions(), "malformed");
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
    assert_eq!(
        issue.blocker.as_ref().expect("runtime defect blocker").kind,
        "runtime_defect"
    );
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
    assert!(
        session
            .last_event
            .as_deref()
            .is_some_and(|event| event.starts_with("failed:incomplete_success_handoff")),
        "last_event={:?}",
        session.last_event
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
        "opencode-handoff.json: unknown field `next_action`, expected `session_id`",
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
    assert_backlog_transition(&client.transitions(), "malformed-json");
    assert!(client.evidence().iter().any(|(_, evidence)| {
        evidence.kind == "malformed_handoff"
            && evidence.body.contains("unknown field `next_action`")
    }));
    let issue = store
        .issue("symphony", "malformed-json")
        .await
        .expect("query malformed")
        .expect("malformed issue");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Failed);
    assert_eq!(
        issue.blocker.as_ref().expect("runtime defect blocker").kind,
        "runtime_defect"
    );
    let failure = issue.failure.expect("failure");
    assert_eq!(failure.kind, "malformed_handoff");
    assert_eq!(
        failure.fingerprint.as_deref(),
        Some("malformed_handoff_sidecar")
    );
    assert!(failure.message.contains("unknown field `next_action`"));
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

    assert_backlog_transition(&client.transitions(), "missing-sidecar");
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
async fn dead_acp_process_with_active_opencode_child_session_resumes_instead_of_parking_capacity() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let opencode_db_path = dir.path().join("opencode.sqlite3");
    super::opencode_runtime::seed_opencode_session_tree(&opencode_db_path).await;
    let config = RootConfig::from_toml_str(&valid_config_toml().replacen(
        "[[projects]]",
        &format!(
            "[opencode_storage]\ndatabase_path = \"{}\"\narchive_root = \"{}\"\n\n[[projects]]",
            opencode_db_path.display(),
            dir.path().join("archives").display()
        ),
        1,
    ))
    .expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let worktree = dir.path().join("SYM-91-worktree");
    fs::create_dir_all(&worktree).expect("worktree");
    store
        .upsert_issue(test_issue("symphony", "active-child", "SYM-91"))
        .await
        .expect("running issue");
    store
        .upsert_opencode_session({
            let mut session = test_session("symphony", "active-child", "ses-root", &worktree);
            session.process_id = None;
            session
        })
        .await
        .expect("running session");
    let client = RecordingLinearClient::new(vec![linear_issue(
        "active-child",
        "SYM-91",
        "In Progress",
        Some(1),
    )]);
    let opencode = ResumeRecordingOpenCodeLauncher::new(4242);

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert!(
        client.evidence().is_empty(),
        "fresh OpenCode child activity must not be reported as missing handoff"
    );
    assert!(opencode.repairs().is_empty());
    assert_eq!(
        opencode.continuations(),
        vec![("SYM-91".into(), "ses-root".into())]
    );
    let issue = store
        .issue("symphony", "active-child")
        .await
        .expect("query issue")
        .expect("issue");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Running);
    assert!(issue.failure.is_none());
    let session = store
        .opencode_session("symphony", "active-child", "ses-root")
        .await
        .expect("query session")
        .expect("session");
    assert_eq!(session.lifecycle_stage, LifecycleStage::Running);
    assert_eq!(session.stage, OpenCodeStage::Running);
    assert_eq!(session.process_id, Some(4242));
    assert_eq!(session.subagent_count, 1);
    assert_eq!(
        session.lifecycle_marker.as_deref(),
        Some("opencode_db_activity")
    );
    assert!(
        session
            .last_event
            .as_deref()
            .is_some_and(|event| event.starts_with("opencode_db_updated")),
        "last_event={:?}",
        session.last_event
    );
}

#[tokio::test]
async fn live_acp_process_with_fresh_opencode_activity_keeps_process_id_without_resume() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let opencode_db_path = dir.path().join("opencode.sqlite3");
    super::opencode_runtime::seed_opencode_session_tree(&opencode_db_path).await;
    let config = RootConfig::from_toml_str(&valid_config_toml().replacen(
        "[[projects]]",
        &format!(
            "[opencode_storage]\ndatabase_path = \"{}\"\narchive_root = \"{}\"\n\n[[projects]]",
            opencode_db_path.display(),
            dir.path().join("archives").display()
        ),
        1,
    ))
    .expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let worktree = dir.path().join("SYM-93-worktree");
    fs::create_dir_all(&worktree).expect("worktree");
    store
        .upsert_issue(test_issue("symphony", "fresh-live", "SYM-93"))
        .await
        .expect("running issue");
    let mut live_process = Command::new("bash")
        .arg("-c")
        .arg("exec -a opencode sleep 120")
        .spawn()
        .expect("spawn opencode-shaped process");
    thread::sleep(Duration::from_millis(100));
    let live_process_id = live_process.id();
    store
        .upsert_opencode_session({
            let mut session = test_session("symphony", "fresh-live", "ses-root", &worktree);
            session.process_id = Some(live_process_id);
            session
        })
        .await
        .expect("running session");
    let client = RecordingLinearClient::new(vec![linear_issue(
        "fresh-live",
        "SYM-93",
        "In Progress",
        Some(1),
    )]);
    let opencode = ResumeRecordingOpenCodeLauncher::new(4243);

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert!(opencode.continuations().is_empty());
    let session = store
        .opencode_session("symphony", "fresh-live", "ses-root")
        .await
        .expect("query session")
        .expect("session");
    assert_eq!(session.lifecycle_stage, LifecycleStage::Running);
    assert_eq!(session.stage, OpenCodeStage::Running);
    assert_eq!(session.process_id, Some(live_process_id));

    let _ = live_process.kill();
    let _ = live_process.wait();
}

#[tokio::test]
async fn live_acp_process_with_stale_opencode_activity_fails_fast_without_resume_loop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let opencode_db_path = dir.path().join("opencode.sqlite3");
    super::opencode_runtime::seed_stale_active_opencode_session_tree(&opencode_db_path).await;
    let config = RootConfig::from_toml_str(&valid_config_toml().replacen(
        "[[projects]]",
        &format!(
            "[opencode_storage]\ndatabase_path = \"{}\"\narchive_root = \"{}\"\n\n[[projects]]",
            opencode_db_path.display(),
            dir.path().join("archives").display()
        ),
        1,
    ))
    .expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let worktree = dir.path().join("SYM-92-worktree");
    fs::create_dir_all(&worktree).expect("worktree");
    store
        .upsert_issue(test_issue("symphony", "stale-live", "SYM-92"))
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
            let mut session = test_session("symphony", "stale-live", "ses-root", &worktree);
            session.process_id = Some(stale_process_id);
            session
        })
        .await
        .expect("running session");
    let client = RecordingLinearClient::new(vec![linear_issue(
        "stale-live",
        "SYM-92",
        "In Progress",
        Some(1),
    )]);
    let opencode = ScriptedOpenCodeLauncher::new(None);

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

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
        "stale live ACP process must be terminated instead of treated as active work"
    );
    assert_backlog_transition(&client.transitions(), "stale-live");
    assert!(opencode.repairs().is_empty());
    let issue = store
        .issue("symphony", "stale-live")
        .await
        .expect("query issue")
        .expect("issue");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Failed);
    assert_eq!(
        issue.failure.expect("failure").fingerprint.as_deref(),
        Some("missing_handoff_sidecar")
    );
    let session = store
        .opencode_session("symphony", "stale-live", "ses-root")
        .await
        .expect("query session")
        .expect("session");
    assert_eq!(session.lifecycle_stage, LifecycleStage::Failed);
    assert_eq!(session.stage, OpenCodeStage::Failed);
    assert_eq!(session.process_id, None);
}

#[tokio::test]
async fn missing_handoff_after_local_commit_records_worktree_git_snapshot_and_process_ref() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");

    let worktree = dir.path().join("SYM-88-worktree");
    fs::create_dir_all(worktree.join("docs/architecture/context-bundles")).expect("docs dir");
    run_git(&worktree, ["init"]);
    run_git(&worktree, ["config", "user.email", "symphony@example.test"]);
    run_git(&worktree, ["config", "user.name", "Symphony Test"]);
    run_git(
        &worktree,
        [
            "checkout",
            "-b",
            "feature/mne-161-p0-current-wcb-code-boundary-and-assumption-audit",
        ],
    );
    fs::write(worktree.join("README.md"), "base").expect("base readme");
    run_git(&worktree, ["add", "README.md"]);
    run_git(&worktree, ["commit", "-m", "base"]);
    fs::write(
        worktree.join("docs/architecture/context-bundles/wcb-boundary-audit.md"),
        "# WCB Boundary Audit\n\nCaptured evidence.\n",
    )
    .expect("audit artifact");
    run_git(
        &worktree,
        [
            "add",
            "docs/architecture/context-bundles/wcb-boundary-audit.md",
        ],
    );
    run_git(
        &worktree,
        ["commit", "-m", "docs: add context bundle boundary audit"],
    );
    let head_sha = git_output(&worktree, ["rev-parse", "HEAD"])
        .trim()
        .to_string();

    store
        .upsert_issue(test_issue("symphony", "missing-sidecar-commit", "SYM-88"))
        .await
        .expect("running issue");
    store
        .upsert_opencode_session({
            let mut session =
                test_session("symphony", "missing-sidecar-commit", "oc-88", &worktree);
            session.process_id = Some(u32::MAX);
            session
        })
        .await
        .expect("running session");
    let client = RecordingLinearClient::new(vec![linear_issue(
        "missing-sidecar-commit",
        "SYM-88",
        "In Progress",
        Some(1),
    )]);
    let opencode = ScriptedOpenCodeLauncher::new(None);

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert_backlog_transition(&client.transitions(), "missing-sidecar-commit");
    let evidence = client
        .evidence()
        .into_iter()
        .find(|(_, evidence)| evidence.kind == "malformed_handoff")
        .map(|(_, evidence)| evidence.body)
        .expect("runtime defect evidence");
    assert!(evidence.contains(".symphony/opencode-handoff.json was not produced"));
    assert!(evidence.contains("git_snapshot:"));
    assert!(evidence.contains("process_id: 4294967295"));
    assert!(evidence.contains("docs/architecture/context-bundles/wcb-boundary-audit.md"));
    assert!(evidence.contains(&head_sha));
    assert!(evidence.contains("upstream: none"));

    let issue = store
        .issue("symphony", "missing-sidecar-commit")
        .await
        .expect("query issue")
        .expect("issue");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Failed);
    assert!(issue.git_ref.is_some());
    assert_eq!(
        issue.failure.expect("failure").fingerprint.as_deref(),
        Some("missing_handoff_sidecar")
    );
    assert!(evidence.contains("fingerprint: missing_handoff_sidecar"));
    assert!(evidence.contains("repair_attempt: 1"));
    assert!(evidence.contains("next_action: fix_runner_tooling_defect_before_retry"));
    assert!(opencode.repairs().is_empty());

    let session = store
        .opencode_session("symphony", "missing-sidecar-commit", "oc-88")
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
    assert!(
        session
            .last_event
            .as_deref()
            .is_some_and(|event| event.starts_with("failed:missing_handoff_sidecar"))
    );
}

#[tokio::test]
async fn missing_handoff_after_dirty_worktree_records_dirty_salvage_snapshot() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    run_git(&repo, ["init"]);
    run_git(&repo, ["config", "user.email", "symphony@example.test"]);
    run_git(&repo, ["config", "user.name", "Symphony Test"]);
    fs::write(repo.join("README.md"), "base\n").expect("base readme");
    run_git(&repo, ["add", "README.md"]);
    run_git(&repo, ["commit", "-m", "base"]);
    run_git(&repo, ["branch", "agent-server/opencode-runner-extension"]);
    run_git(&repo, ["checkout", "-b", "symphony/SYM-89"]);
    fs::write(repo.join("dirty.txt"), "uncommitted repair evidence\n").expect("dirty file");
    let config = config_for_repo_and_worktree_root(&repo, dir.path());
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "missing-sidecar-dirty", "SYM-89"))
        .await
        .expect("running issue");
    store
        .upsert_opencode_session(test_session(
            "symphony",
            "missing-sidecar-dirty",
            "oc-89",
            &repo,
        ))
        .await
        .expect("running session");
    let client = RecordingLinearClient::new(vec![linear_issue(
        "missing-sidecar-dirty",
        "SYM-89",
        "In Progress",
        Some(1),
    )]);
    let opencode = ScriptedOpenCodeLauncher::new(None);

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    let evidence = malformed_handoff_evidence_body(&client);
    assert!(evidence.contains("salvage_state: dirty_worktree"));
    assert!(evidence.contains("status_short:"));
    assert!(evidence.contains("dirty.txt"));
    assert!(opencode.repairs().is_empty());
    assert!(repo.exists(), "dirty worktree must be preserved for repair");
}

#[tokio::test]
async fn missing_handoff_after_no_diff_records_explicit_no_change_salvage_snapshot() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    run_git(&repo, ["init"]);
    run_git(&repo, ["config", "user.email", "symphony@example.test"]);
    run_git(&repo, ["config", "user.name", "Symphony Test"]);
    fs::write(repo.join("README.md"), "base\n").expect("base readme");
    run_git(&repo, ["add", "README.md"]);
    run_git(&repo, ["commit", "-m", "base"]);
    run_git(&repo, ["branch", "agent-server/opencode-runner-extension"]);
    run_git(&repo, ["checkout", "-b", "symphony/SYM-91"]);
    let config = config_for_repo_and_worktree_root(&repo, dir.path());
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "missing-sidecar-no-diff", "SYM-91"))
        .await
        .expect("running issue");
    store
        .upsert_opencode_session(test_session(
            "symphony",
            "missing-sidecar-no-diff",
            "oc-91",
            &repo,
        ))
        .await
        .expect("running session");
    let client = RecordingLinearClient::new(vec![linear_issue(
        "missing-sidecar-no-diff",
        "SYM-91",
        "In Progress",
        Some(1),
    )]);
    let opencode = ScriptedOpenCodeLauncher::new(None);

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    let evidence = malformed_handoff_evidence_body(&client);
    assert!(evidence.contains("salvage_state: no_local_changes"));
    assert!(evidence.contains("base_changed_files:\nnone"));
    assert!(evidence.contains("explicit no-change handoff instead of a fake commit"));
    assert_backlog_transition(&client.transitions(), "missing-sidecar-no-diff");
    assert!(opencode.repairs().is_empty());
}

#[tokio::test]
async fn missing_handoff_after_unpushed_branch_records_unpushed_salvage_snapshot() {
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
    fs::write(repo.join("README.md"), "base\n").expect("base readme");
    run_git(&repo, ["add", "README.md"]);
    run_git(&repo, ["commit", "-m", "base"]);
    run_git(&repo, ["branch", "agent-server/opencode-runner-extension"]);
    run_git(
        &repo,
        ["push", "origin", "agent-server/opencode-runner-extension"],
    );
    run_git(&repo, ["checkout", "-b", "symphony/SYM-92"]);
    run_git(&repo, ["push", "-u", "origin", "symphony/SYM-92"]);
    fs::write(repo.join("artifact.txt"), "unpushed implementation\n").expect("artifact");
    run_git(&repo, ["add", "artifact.txt"]);
    run_git(&repo, ["commit", "-m", "SYM-92 implementation"]);
    let head_sha = git_output(&repo, ["rev-parse", "HEAD"]).trim().to_owned();
    let config = config_for_repo_and_worktree_root(&repo, dir.path());
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "missing-sidecar-unpushed", "SYM-92"))
        .await
        .expect("running issue");
    store
        .upsert_opencode_session(test_session(
            "symphony",
            "missing-sidecar-unpushed",
            "oc-92",
            &repo,
        ))
        .await
        .expect("running session");
    let client = RecordingLinearClient::new(vec![linear_issue(
        "missing-sidecar-unpushed",
        "SYM-92",
        "In Progress",
        Some(1),
    )]);
    let opencode = ScriptedOpenCodeLauncher::new(None);

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    let evidence = malformed_handoff_evidence_body(&client);
    assert!(evidence.contains("salvage_state: unpushed_commits"));
    assert!(evidence.contains("unpushed_commits: 1"));
    assert!(evidence.contains(&head_sha));
    assert!(evidence.contains("artifact.txt"));
    assert_backlog_transition(&client.transitions(), "missing-sidecar-unpushed");
    assert!(opencode.repairs().is_empty());
}

fn config_for_repo_and_worktree_root(
    repo: &std::path::Path,
    worktree_root: &std::path::Path,
) -> RootConfig {
    let config_toml = valid_config_toml()
        .replace(
            "repo_path = \"/home/agent/proj/symphony\"",
            &format!("repo_path = \"{}\"", repo.display()),
        )
        .replace(
            "/home/agent/.symphony/workspaces/opencode/symphony",
            &worktree_root.display().to_string(),
        );
    RootConfig::from_toml_str(&config_toml).expect("config")
}

fn malformed_handoff_evidence_body(client: &RecordingLinearClient) -> String {
    client
        .evidence()
        .into_iter()
        .find(|(_, evidence)| evidence.kind == "malformed_handoff")
        .map(|(_, evidence)| evidence.body)
        .expect("runtime defect evidence")
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
