use super::*;

#[tokio::test]
async fn daemon_once_entrypoint_validates_config_migrates_and_reconciles_projects() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("projects.toml");
    let db_path = dir.path().join("runtime.sqlite3");
    fs::write(&config_path, valid_config_toml()).expect("write config");

    cli::run_with_args([
        "symphony-vnext",
        "daemon",
        "--config",
        config_path.to_str().expect("utf8 config path"),
        "--database",
        db_path.to_str().expect("utf8 db path"),
        "--once",
    ])
    .await
    .expect("daemon bootstrap");

    let store = SqliteStore::open(&db_path).await.expect("reopen sqlite");
    store.migrate().await.expect("migrate idempotently");

    let project = store
        .project("symphony")
        .await
        .expect("query project")
        .expect("project row");
    assert_eq!(project.name, "Symphony");
    assert_eq!(project.lifecycle_stage, LifecycleStage::Queued);
    assert_eq!(project.cleanup_status, CleanupStatus::Clean);
}

#[tokio::test]
async fn orchestration_dispatches_one_eligible_todo_by_project_capacity_and_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "running-1", "SYM-21", "In Progress"))
        .await
        .expect("running issue");

    let client = RecordingLinearClient::new(vec![
        linear_issue("backlog-1", "SYM-20", "Backlog", Some(1)),
        linear_issue("todo-low-priority", "SYM-30", "Todo", Some(3)),
        linear_issue("todo-high-priority", "SYM-22", "Todo", Some(1)),
    ]);

    let report = daemon::run_once_with_linear_client(&config, &store, &client)
        .await
        .expect("orchestrate once");

    assert_eq!(report.dispatched, vec!["SYM-22"]);
    assert_eq!(
        client.transitions(),
        vec![("todo-high-priority".into(), LinearTransition::InProgress)]
    );
    assert_eq!(
        store
            .issue("symphony", "todo-high-priority")
            .await
            .expect("query dispatched")
            .expect("dispatched")
            .lifecycle_stage,
        LifecycleStage::Running
    );
    assert!(
        store
            .issue("symphony", "backlog-1")
            .await
            .expect("backlog")
            .is_none()
    );
}

#[tokio::test]
async fn orchestration_never_dispatches_nonterminal_blockers_or_backlog() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");

    let blocked =
        linear_issue("blocked", "SYM-40", "Todo", Some(1)).blocked_by(vec![LinearBlocker {
            id: Some("blocker-1".into()),
            identifier: Some("SYM-39".into()),
            state: Some("In Progress".into()),
        }]);
    let unblocked =
        linear_issue("unblocked", "SYM-41", "Todo", Some(2)).blocked_by(vec![LinearBlocker {
            id: Some("blocker-2".into()),
            identifier: Some("SYM-38".into()),
            state: Some("Done".into()),
        }]);
    let client = RecordingLinearClient::new(vec![
        linear_issue("backlog", "SYM-35", "Backlog", Some(0)),
        blocked,
        unblocked,
    ]);

    let report = daemon::run_once_with_linear_client(&config, &store, &client)
        .await
        .expect("orchestrate once");

    assert_eq!(report.dispatched, vec!["SYM-41"]);
    assert_eq!(
        client.transitions(),
        vec![("unblocked".into(), LinearTransition::InProgress)]
    );
    let blocked_row = store
        .issue("symphony", "blocked")
        .await
        .expect("query blocked")
        .expect("blocked row");
    assert_eq!(blocked_row.lifecycle_stage, LifecycleStage::Blocked);
    assert_eq!(blocked_row.blocker.expect("blocker").kind, "linear_blocker");
    assert!(
        store
            .issue("symphony", "backlog")
            .await
            .expect("backlog")
            .is_none()
    );
}

#[tokio::test]
async fn orchestration_leaves_todo_queued_when_todo_spans_multiple_milestones() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");

    let first = linear_issue("first", "SYM-42", "Todo", Some(1));
    let mut second = linear_issue("second", "SYM-43", "Todo", Some(2));
    second.project_milestone = Some(symphony_vnext::linear::LinearMilestone {
        id: "different-milestone-id".into(),
        name: "Different Milestone".into(),
    });
    let client = RecordingLinearClient::new(vec![first, second]);

    let report = daemon::run_once_with_linear_client(&config, &store, &client)
        .await
        .expect("orchestrate once");

    assert!(report.dispatched.is_empty());
    assert!(report.blocked.is_empty());
    assert!(client.transitions().is_empty());
    let first = store
        .issue("symphony", "first")
        .await
        .expect("query first")
        .expect("first issue");
    assert_eq!(first.lifecycle_stage, LifecycleStage::Queued);
    let second = store
        .issue("symphony", "second")
        .await
        .expect("query second")
        .expect("second issue");
    assert_eq!(second.lifecycle_stage, LifecycleStage::Queued);
}

#[tokio::test]
async fn orchestration_reconciles_persisted_backlog_without_counting_capacity() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue(
            "symphony",
            "parked-plan",
            "SYM-45",
            "In Progress",
        ))
        .await
        .expect("persisted running backlog issue");
    store
        .upsert_issue(test_issue(
            "symphony",
            "still-running",
            "SYM-46",
            "In Progress",
        ))
        .await
        .expect("persisted running issue");

    let client = RecordingLinearClient::new(vec![
        linear_issue("parked-plan", "SYM-45", "Backlog", Some(1)),
        linear_issue("eligible", "SYM-47", "Todo", Some(2)),
    ]);

    let report = daemon::run_once_with_linear_client(&config, &store, &client)
        .await
        .expect("orchestrate once");

    assert_eq!(report.dispatched, vec!["SYM-47"]);
    assert_eq!(
        store
            .issue("symphony", "parked-plan")
            .await
            .expect("query backlog")
            .expect("backlog row")
            .lifecycle_stage,
        LifecycleStage::Queued
    );
    assert_eq!(
        client.transitions(),
        vec![("eligible".into(), LinearTransition::InProgress)]
    );
}

#[tokio::test]
async fn orchestration_keeps_owner_input_parked_until_answer_or_manual_todo() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");

    let parked = linear_issue("parked", "SYM-50", "Need Owner Input", Some(1));
    let answered = linear_issue("answered", "SYM-51", "Need Owner Input", Some(2))
        .with_new_owner_answer_at("2026-06-10T00:02:00Z");
    let manually_requeued = linear_issue("manual", "SYM-52", "Todo", Some(3));
    let client = RecordingLinearClient::new(vec![parked, answered, manually_requeued]);

    let report = daemon::run_once_with_linear_client(&config, &store, &client)
        .await
        .expect("orchestrate once");

    assert_eq!(report.dispatched, vec!["SYM-52"]);
    assert_eq!(
        client.transitions(),
        vec![
            ("answered".into(), LinearTransition::Todo),
            ("manual".into(), LinearTransition::InProgress),
        ]
    );
    assert_eq!(
        store
            .issue("symphony", "parked")
            .await
            .expect("query parked")
            .expect("parked")
            .lifecycle_stage,
        LifecycleStage::Blocked
    );
}

#[tokio::test]
async fn orchestration_ignores_owner_input_comments_that_predate_the_parked_record() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let mut stale = test_issue("symphony", "stale", "SYM-53", "Need Owner Input");
    stale.lifecycle_stage = LifecycleStage::Blocked;
    stale.blocker = Some(BlockerRecord {
        kind: "owner_question".into(),
        message: "waiting for owner answer".into(),
        observed_at: Some("2026-06-10T00:05:00Z".into()),
    });
    store.upsert_issue(stale).await.expect("stale issue");

    let client = RecordingLinearClient::new(vec![
        linear_issue("stale", "SYM-53", "Need Owner Input", Some(1))
            .with_new_owner_answer_at("2026-06-10T00:03:00Z"),
    ]);

    daemon::run_once_with_linear_client(&config, &store, &client)
        .await
        .expect("orchestrate once");

    assert_eq!(
        client.transitions(),
        Vec::<(String, LinearTransition)>::new()
    );
    assert_eq!(
        store
            .issue("symphony", "stale")
            .await
            .expect("query stale")
            .expect("stale")
            .lifecycle_stage,
        LifecycleStage::Blocked
    );
}

#[tokio::test]
async fn orchestration_ignores_new_symphony_evidence_comments_after_parked_record() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let project = config.project("symphony").expect("project");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let mut parked = test_issue("symphony", "parked", "SYM-54", "Need Owner Input");
    parked.lifecycle_stage = LifecycleStage::Blocked;
    parked.blocker = Some(BlockerRecord {
        kind: "owner_question".into(),
        message: "waiting for owner answer".into(),
        observed_at: Some("2026-06-10T00:05:00Z".into()),
    });
    store.upsert_issue(parked).await.expect("parked issue");

    let transport = RecordingGraphqlTransport::new(vec![serde_json::json!({
        "data": {
            "issues": {
                "nodes": [
                    {
                        "id": "parked",
                        "identifier": "SYM-54",
                        "title": "Parked owner question",
                        "description": "Wait for owner input",
                        "state": { "name": "Need Owner Input" },
                        "priority": 1,
                        "branchName": "agent-server/opencode-runner-extension",
                        "url": "https://linear.example/SYM-54",
                        "labels": { "nodes": [] },
                        "comments": {
                            "nodes": [
                                {
                                    "body": "kind: owner_question\n\nwaiting for owner input",
                                    "parent": null,
                                    "createdAt": "2026-06-10T00:06:00Z"
                                }
                            ]
                        },
                        "relations": { "nodes": [] },
                        "createdAt": "2026-06-10T00:00:00Z",
                        "updatedAt": "2026-06-10T00:06:00Z"
                    }
                ],
                "pageInfo": { "hasNextPage": false, "endCursor": null }
            }
        }
    })]);
    let client = LinearGraphqlClient::new(
        "https://linear.example/graphql",
        "linear-token",
        transport.clone(),
    );

    daemon::run_once_with_linear_client(&config, &store, &client)
        .await
        .expect("orchestrate once");

    assert_eq!(transport.requests().len(), 1);
    assert_eq!(
        store
            .issue(project.id.as_str(), "parked")
            .await
            .expect("query parked")
            .expect("parked")
            .lifecycle_stage,
        LifecycleStage::Blocked
    );
}

#[tokio::test]
async fn orchestration_reconciles_terminal_issues_and_avoids_duplicate_dispatch_after_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "finished", "SYM-60", "In Progress"))
        .await
        .expect("finished issue");

    let first_poll = RecordingLinearClient::new(vec![
        linear_issue("finished", "SYM-60", "Done", Some(1)),
        linear_issue("new-work", "SYM-61", "Todo", Some(2)),
    ]);
    let restart_poll = RecordingLinearClient::new(vec![
        linear_issue("finished", "SYM-60", "Done", Some(1)),
        linear_issue("new-work", "SYM-61", "In Progress", Some(2)),
    ]);

    daemon::run_once_with_linear_client(&config, &store, &first_poll)
        .await
        .expect("first poll");
    daemon::run_once_with_linear_client(&config, &store, &restart_poll)
        .await
        .expect("restart poll");

    assert_eq!(
        first_poll.transitions(),
        vec![("new-work".into(), LinearTransition::InProgress)]
    );
    assert!(restart_poll.transitions().is_empty());
    let finished = store
        .issue("symphony", "finished")
        .await
        .expect("query finished")
        .expect("finished");
    assert_eq!(finished.lifecycle_stage, LifecycleStage::Completed);
    assert_eq!(finished.cleanup_status, CleanupStatus::Pending);
    assert_eq!(
        store
            .opencode_sessions_for_issue("symphony", "new-work")
            .await
            .expect("sessions")
            .len(),
        1
    );
}

#[tokio::test]
async fn orchestration_refreshes_active_opencode_session_metrics_from_persisted_database() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let opencode_db_path = dir.path().join("opencode.db");
    opencode_runtime::seed_opencode_session_tree(&opencode_db_path).await;
    let config_toml = valid_config_toml().replace(
        "[[projects]]\n",
        &format!(
            "[opencode_storage]\ndatabase_path = \"{}\"\narchive_root = \"{}\"\n\n[[projects]]\n",
            opencode_db_path.display(),
            dir.path().join("archives").display()
        ),
    );
    let config = RootConfig::from_toml_str(&config_toml).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "work", "SYM-64", "In Progress"))
        .await
        .expect("issue");
    store
        .upsert_opencode_session(test_session(
            "symphony",
            "work",
            "ses-root",
            "/home/agent/.symphony/workspaces/opencode/symphony/SYM-64",
        ))
        .await
        .expect("session");
    let client =
        RecordingLinearClient::new(vec![linear_issue("work", "SYM-64", "In Progress", Some(1))]);

    daemon::run_once_with_linear_client(&config, &store, &client)
        .await
        .expect("orchestrate once");

    let session = store
        .opencode_session("symphony", "work", "ses-root")
        .await
        .expect("query session")
        .expect("session");
    assert_eq!(session.stage, OpenCodeStage::Running);
    assert_eq!(session.message_count, 2);
    assert_eq!(session.part_count, 2);
    assert_eq!(session.todo_count, 1);
    assert_eq!(session.token_count, 784);
    assert_eq!(session.subagent_count, 1);
    assert_eq!(session.active_agent.as_deref(), Some("rust-engineer"));
    assert_eq!(session.active_model.as_deref(), Some("gpt-5.5"));
    assert_eq!(
        session.lifecycle_marker.as_deref(),
        Some("opencode_db_activity")
    );
    assert_eq!(
        session.last_event.as_deref(),
        Some("opencode_db_updated:2000")
    );
    assert!(!session.silence_observed);
}

#[tokio::test]
async fn terminal_reconciliation_marks_cleanup_complete_when_worktree_is_already_absent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let missing_worktree = dir.path().join("already-removed").join("SYM-63");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let mut closed = test_issue("symphony", "closed", "SYM-63", "Done");
    closed.cleanup_status = CleanupStatus::Pending;
    closed.git_ref = Some(GitRefRecord {
        branch: "agent-server/opencode-runner-extension".into(),
        worktree_path: missing_worktree.display().to_string(),
        head_sha: None,
        pr_url: None,
    });
    store.upsert_issue(closed).await.expect("closed issue");

    let client =
        RecordingLinearClient::new(vec![linear_issue("closed", "SYM-63", "Done", Some(1))]);

    daemon::run_once_with_linear_client(&config, &store, &client)
        .await
        .expect("terminal reconciliation");

    let closed = store
        .issue("symphony", "closed")
        .await
        .expect("query closed")
        .expect("closed issue");
    assert_eq!(closed.lifecycle_stage, LifecycleStage::Completed);
    assert_eq!(closed.cleanup_status, CleanupStatus::Complete);
    assert_eq!(
        closed.git_ref.expect("git ref").worktree_path,
        missing_worktree.display().to_string()
    );
}

#[tokio::test]
async fn orchestration_restores_requeued_issue_with_existing_session_without_duplicate_launch() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let worktree = dir.path().join("SYM-62-worktree");
    fs::create_dir_all(&worktree).expect("worktree");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "requeued", "SYM-62", "In Progress"))
        .await
        .expect("running issue");
    store
        .upsert_opencode_session(test_session("symphony", "requeued", "oc-62", &worktree))
        .await
        .expect("running session");

    let client =
        RecordingLinearClient::new(vec![linear_issue("requeued", "SYM-62", "Todo", Some(1))]);

    daemon::run_once_with_linear_client(&config, &store, &client)
        .await
        .expect("poll");

    assert_eq!(
        client.transitions(),
        vec![("requeued".into(), LinearTransition::InProgress)]
    );
    let issue = store
        .issue("symphony", "requeued")
        .await
        .expect("query requeued")
        .expect("requeued");
    assert_eq!(issue.state, "In Progress");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Running);
    assert_eq!(
        store
            .opencode_sessions_for_issue("symphony", "requeued")
            .await
            .expect("sessions")
            .len(),
        1
    );
}

#[tokio::test]
async fn orchestration_returns_in_progress_issue_without_session_to_todo() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let client = RecordingLinearClient::new(vec![linear_issue(
        "lost-session",
        "SYM-64",
        "In Progress",
        Some(1),
    )]);

    daemon::run_once_with_linear_client(&config, &store, &client)
        .await
        .expect("poll");

    assert_eq!(
        client.transitions(),
        vec![("lost-session".into(), LinearTransition::Todo)]
    );
    let issue = store
        .issue("symphony", "lost-session")
        .await
        .expect("query issue")
        .expect("issue");
    assert_eq!(issue.state, "Todo");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Queued);
}
