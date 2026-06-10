use super::*;

#[tokio::test]
async fn passing_opencode_handoff_moves_done_records_git_metadata_and_removes_worktree() {
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
    let config_yaml = valid_config_yaml()
        .replace(
            "repo_path: /home/agent/proj/symphony",
            &format!("repo_path: {}", repo.display()),
        )
        .replace(
            "/home/agent/.symphony/workspaces/opencode/symphony",
            &worktree_root.display().to_string(),
        );
    let config = RootConfig::from_yaml_str(&config_yaml).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "completed", "SYM-80", "In Progress"))
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
        "agent-server/opencode-runner-extension",
        "abc123def456",
    )));

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert_eq!(
        client.transitions(),
        vec![("completed".into(), LinearTransition::Done)]
    );
    assert!(client.evidence().iter().any(|(_, evidence)| {
        evidence.body.contains("abc123def456")
            && evidence
                .body
                .contains("agent-server/opencode-runner-extension")
    }));
    let completed = store
        .issue("symphony", "completed")
        .await
        .expect("query completed")
        .expect("completed issue");
    assert_eq!(completed.state, "Done");
    assert_eq!(completed.lifecycle_stage, LifecycleStage::Completed);
    assert_eq!(completed.cleanup_status, CleanupStatus::Complete);
    let git_ref = completed.git_ref.expect("git ref");
    assert_eq!(git_ref.branch, "agent-server/opencode-runner-extension");
    assert_eq!(git_ref.head_sha.as_deref(), Some("abc123def456"));
    assert_eq!(
        git_ref.pr_url.as_deref(),
        Some("https://example.test/pr/80")
    );
    assert!(!worktree.exists(), "accepted handoff must remove worktree");
    assert!(
        !git_output(&repo, ["worktree", "list", "--porcelain"])
            .contains(worktree.to_str().expect("worktree path utf8")),
        "accepted handoff must unregister the git worktree"
    );

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
        Some("abc123def456")
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
    let config_yaml = valid_config_yaml()
        .replace(
            "repo_path: /home/agent/proj/symphony",
            &format!("repo_path: {}", repo.display()),
        )
        .replace(
            "/home/agent/.symphony/workspaces/opencode/symphony",
            &worktree_root.display().to_string(),
        );
    let config = RootConfig::from_yaml_str(&config_yaml).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "no-code", "SYM-79", "In Progress"))
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
            suite: "symphony-vnext-smoke".into(),
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
    assert_eq!(completed.state, "Done");
    assert_eq!(completed.lifecycle_stage, LifecycleStage::Completed);
    assert_eq!(completed.cleanup_status, CleanupStatus::Complete);
    assert_eq!(
        completed.git_ref.expect("git ref").head_sha.as_deref(),
        None
    );
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
    let config = RootConfig::from_yaml_str(&valid_config_yaml().replace(
        "/home/agent/.symphony/workspaces/opencode/symphony",
        allowed_root.to_str().expect("allowed root utf8"),
    ))
    .expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "completed", "SYM-80", "In Progress"))
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
        vec![("completed".into(), LinearTransition::NeedOwnerInput)]
    );
    assert!(outside.exists(), "outside path must not be removed");
    assert!(
        client
            .evidence()
            .iter()
            .any(|(_, evidence)| evidence.kind == "malformed_handoff"
                && evidence.body.contains("outside configured worktree root"))
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
    let config = RootConfig::from_yaml_str(&valid_config_yaml().replace(
        "/home/agent/.symphony/workspaces/opencode/symphony",
        allowed_root.to_str().expect("allowed root utf8"),
    ))
    .expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "completed", "SYM-80", "In Progress"))
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
        vec![("completed".into(), LinearTransition::NeedOwnerInput)]
    );
    assert!(active.exists(), "active worktree must not be removed");
    assert!(sibling.exists(), "sibling worktree must not be removed");
    assert!(client.evidence().iter().any(|(_, evidence)| {
        evidence.kind == "malformed_handoff"
            && evidence
                .body
                .contains("does not match active session worktree")
    }));
}

#[tokio::test]
async fn successful_handoff_with_whitespace_worktree_path_is_parked_without_cleanup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let allowed_root = dir.path().join("allowed-worktrees");
    let active = allowed_root.join("SYM-80");
    fs::create_dir_all(&active).expect("active worktree");
    fs::write(active.join("artifact.txt"), "must survive").expect("artifact");
    let config = RootConfig::from_yaml_str(&valid_config_yaml().replace(
        "/home/agent/.symphony/workspaces/opencode/symphony",
        allowed_root.to_str().expect("allowed root utf8"),
    ))
    .expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "completed", "SYM-80", "In Progress"))
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
        vec![("completed".into(), LinearTransition::NeedOwnerInput)]
    );
    assert!(active.exists(), "active worktree must not be removed");
    assert!(client.evidence().iter().any(|(_, evidence)| {
        evidence.kind == "malformed_handoff"
            && evidence.body.contains("leading or trailing whitespace")
    }));
}

#[tokio::test]
async fn eval_failure_stays_in_opencode_repair_loop_without_linear_churn() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let worktree = dir.path().join("SYM-81-worktree");
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "repair", "SYM-81", "In Progress"))
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
    assert_eq!(issue.state, "In Progress");
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
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let mut issue = test_issue("symphony", "repeat", "SYM-82", "In Progress");
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
    assert_eq!(issue.state, "Need Owner Input");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Blocked);
    assert_eq!(
        issue.blocker.expect("blocker").kind,
        "repeated_eval_failure"
    );
}

#[tokio::test]
async fn provider_blocker_owner_question_and_malformed_handoff_park_without_closing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
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
        (
            "malformed",
            "SYM-85",
            OpenCodeHandoff {
                session_id: "oc-malformed".into(),
                lifecycle_stages: vec![OpenCodeStage::Eval, OpenCodeStage::Handoff],
                subagents: vec!["rust-engineer".into()],
                eval_results: vec![OpenCodeEvalResult {
                    suite: "cargo test".into(),
                    passed: true,
                    failure_fingerprint: None,
                    details: None,
                }],
                changed_files: vec!["crates/symphony-vnext/src/opencode.rs".into()],
                git: None,
                risks: Vec::new(),
                stop_reason: OpenCodeStopReason::Success,
            },
            "malformed_handoff",
        ),
    ];

    for (issue_id, identifier, handoff, expected_kind) in cases {
        let worktree = dir.path().join(format!("{identifier}-worktree"));
        store
            .upsert_issue(test_issue("symphony", issue_id, identifier, "In Progress"))
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
        let issue = store
            .issue("symphony", issue_id)
            .await
            .expect("query parked")
            .expect("parked issue");
        assert_eq!(issue.state, "Need Owner Input");
        assert_eq!(issue.lifecycle_stage, LifecycleStage::Blocked);
        assert_eq!(issue.blocker.expect("blocker").kind, expected_kind);
    }
}

#[tokio::test]
async fn rust_path_parks_legacy_steward_states_instead_of_preserving_them() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let client = RecordingLinearClient::new(vec![
        linear_issue("preparing", "SYM-85", "Preparing", Some(0)),
        linear_issue("review", "SYM-86", "In Review", Some(1)),
        linear_issue("rca", "SYM-87", "RCA Required", Some(2)),
    ]);

    daemon::run_once_with_linear_client(&config, &store, &client)
        .await
        .expect("orchestrate once");

    assert_eq!(
        client.transitions(),
        vec![
            ("preparing".into(), LinearTransition::NeedOwnerInput),
            ("review".into(), LinearTransition::NeedOwnerInput),
            ("rca".into(), LinearTransition::NeedOwnerInput),
        ]
    );
    for issue_id in ["preparing", "review", "rca"] {
        let issue = store
            .issue("symphony", issue_id)
            .await
            .expect("query parked")
            .expect("parked issue");
        assert_eq!(issue.state, "Need Owner Input");
        assert_eq!(issue.lifecycle_stage, LifecycleStage::Blocked);
        assert_eq!(issue.blocker.expect("blocker").kind, "legacy_runtime_state");
    }
}

#[tokio::test]
async fn orchestration_processes_multiple_projects_in_config_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_yaml_str(&valid_config_yaml().replace(
        "  - id: symphony\n",
        "  - id: alpha\n    name: Alpha\n    enabled: true\n    workflow_path: /home/agent/proj/alpha/WORKFLOW.md\n    repo_path: /home/agent/proj/alpha\n    branch:\n      base: main\n      worktree_root: /home/agent/.symphony/workspaces/opencode/alpha\n    linear:\n      team_key: ALPHA\n      project_id: alpha-project\n      project_milestone_id: alpha-milestone\n    opencode:\n      command: /usr/local/bin/opencode\n      args: [\"acp\"]\n      agent: build\n      model: openai/gpt-5.5\n      effort: high\n      permission_policy: reject\n    eval:\n      default_suite: alpha-smoke\n    concurrency:\n      max_sessions: 1\n  - id: symphony\n",
    ))
    .expect("config");
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
