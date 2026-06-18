use super::*;

#[tokio::test]
async fn daemon_once_entrypoint_validates_config_migrates_and_reconciles_projects() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("projects.toml");
    let db_path = dir.path().join("runtime.sqlite3");
    fs::write(&config_path, valid_config_toml()).expect("write config");

    cli::run_with_args([
        "symphony",
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

#[test]
fn systemd_user_unit_declares_restart_and_default_target_autostart() {
    let unit = include_str!("../../../../deploy/systemd/symphony.service");

    assert!(unit.starts_with("[Unit]\n"));
    assert_eq!(unit.matches("[Unit]").count(), 1);
    assert!(unit.contains("\n[Service]\n"));
    assert!(unit.contains("\nRestart=on-failure\n"));
    assert!(unit.contains("\nRestartSec=10\n"));
    assert!(unit.contains("\n[Install]\n"));
    assert!(unit.contains("\nWantedBy=default.target\n"));
}

#[tokio::test]
async fn dashboard_shows_inactive_runtime_before_daemon_poll() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");

    let api = RuntimeDashboardApi::from_store(&config, &store)
        .await
        .expect("dashboard api");
    let project = api
        .project_drilldown("symphony")
        .expect("project endpoint")
        .expect("project exists");

    assert_eq!(
        project.liveness.status,
        RuntimeLivenessStatus::InactiveRuntime
    );
    assert!(project.liveness.last_poll_at.is_none());
    assert!(project.liveness.reason.contains("has not reported"));
}

#[tokio::test]
async fn orchestration_records_no_eligible_liveness_without_launching_opencode() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let client = RecordingLinearClient::new(Vec::new());
    let opencode = ResumeRecordingOpenCodeLauncher::new(4242);

    let report = daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert!(report.dispatched.is_empty());
    assert!(opencode.launches().is_empty());
    let liveness = store
        .project_liveness("symphony")
        .await
        .expect("query liveness")
        .expect("liveness row");
    assert_eq!(liveness.status, RuntimeLivenessStatus::NoEligibleIssues);
    assert_eq!(liveness.available_sessions, 2);
    assert!(liveness.last_poll_at.is_some());
    assert!(liveness.last_successful_candidate_scan_at.is_some());
}

#[tokio::test]
async fn orchestration_continues_other_projects_when_one_project_poll_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(two_project_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let client = PartiallyFailingProjectLinearClient::new(
        "alpha",
        [(
            "symphony",
            vec![linear_issue("symphony-work", "SYM-64", "Todo", Some(1))],
        )],
    );

    let report = daemon::run_once_with_linear_client(&config, &store, &client)
        .await
        .expect("one project failure must not abort global poll");

    assert_eq!(report.dispatched, vec!["SYM-64"]);
    assert_eq!(
        client.transitions(),
        vec![("symphony-work".into(), LinearTransition::InProgress)]
    );
    let alpha_liveness = store
        .project_liveness("alpha")
        .await
        .expect("query alpha liveness")
        .expect("alpha liveness");
    assert_eq!(
        alpha_liveness.status,
        RuntimeLivenessStatus::RunnerSetupFailed
    );
    assert!(
        alpha_liveness
            .reason
            .contains("synthetic fetch failure for alpha"),
        "reason={}",
        alpha_liveness.reason
    );
}

#[tokio::test]
async fn orchestration_records_blocked_issues_liveness_when_candidates_are_blocked() {
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
    let client = RecordingLinearClient::new(vec![blocked]);

    let report = daemon::run_once_with_linear_client(&config, &store, &client)
        .await
        .expect("orchestrate once");

    assert!(report.dispatched.is_empty());
    assert_eq!(report.blocked, vec!["SYM-40"]);
    let liveness = store
        .project_liveness("symphony")
        .await
        .expect("query liveness")
        .expect("liveness row");
    assert_eq!(liveness.status, RuntimeLivenessStatus::BlockedIssues);
    assert!(liveness.reason.contains("blocked or parked"));
}

#[tokio::test]
async fn orchestration_records_healthy_capacity_liveness_before_dispatch() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "running-1", "SYM-21"))
        .await
        .expect("running issue");
    let client =
        RecordingLinearClient::new(vec![linear_issue("eligible", "SYM-22", "Todo", Some(1))]);

    daemon::run_once_with_linear_client(&config, &store, &client)
        .await
        .expect("orchestrate once");

    let liveness = store
        .project_liveness("symphony")
        .await
        .expect("query liveness")
        .expect("liveness row");
    assert_eq!(
        liveness.status,
        RuntimeLivenessStatus::HealthyCapacityAvailable
    );
    assert_eq!(liveness.running_sessions, 1);
    assert_eq!(liveness.available_sessions, 1);
}

#[tokio::test]
async fn orchestration_schedules_repair_for_dead_running_session_without_handoff_sidecar() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "work", "SYM-65"))
        .await
        .expect("issue");
    let mut session = test_session(
        "symphony",
        "work",
        "ses-existing",
        "/home/agent/.symphony/workspaces/opencode/symphony/SYM-65",
    );
    session.process_id = Some(u32::MAX);
    store
        .upsert_opencode_session(session)
        .await
        .expect("session");
    let client =
        RecordingLinearClient::new(vec![linear_issue("work", "SYM-65", "In Progress", Some(1))]);
    let opencode = ResumeRecordingOpenCodeLauncher::new(4242);

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert_todo_transition(&client.transitions(), "work");
    assert!(opencode.repairs().is_empty());
    let liveness = store
        .project_liveness("symphony")
        .await
        .expect("query liveness")
        .expect("liveness row");
    assert_eq!(liveness.status, RuntimeLivenessStatus::NoEligibleIssues);
    let issue = store
        .issue("symphony", "work")
        .await
        .expect("query work")
        .expect("work issue");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Failed);
    assert_eq!(
        issue.blocker.expect("runtime defect blocker").kind,
        "runtime_defect"
    );
    let failure = issue.failure.expect("failure");
    assert_eq!(
        failure.fingerprint.as_deref(),
        Some("missing_handoff_sidecar")
    );
    assert_eq!(failure.occurrence_count, 1);
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
        .upsert_issue(test_issue("symphony", "running-1", "SYM-21"))
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
async fn orchestration_parks_todo_issue_when_mnemesh_workspace_root_is_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config_toml = valid_config_toml().replace(
        "\n[projects.mnemesh]\nworkspace_root = \"/home/agent/proj/symphony\"\n",
        "\n",
    );
    let config = RootConfig::from_toml_str(&config_toml).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let client = RecordingLinearClient::new(vec![linear_issue(
        "missing-workspace",
        "SYM-250",
        "Todo",
        Some(1),
    )]);
    let opencode = ResumeRecordingOpenCodeLauncher::new(4242);

    let report = daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert!(report.dispatched.is_empty());
    assert!(report.parked_owner_input.is_empty());
    assert_eq!(report.blocked, vec!["SYM-250"]);
    assert!(opencode.launches().is_empty());
    assert!(client.transitions().is_empty());
    let evidence = client.evidence();
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].0, "missing-workspace");
    assert_eq!(evidence[0].1.kind, "provider_blocker");
    assert!(evidence[0].1.body.contains("mnemesh_workspace_missing"));
    assert!(
        evidence[0]
            .1
            .body
            .contains("mnemesh workspace_root is not configured")
    );
    let parked = store
        .issue("symphony", "missing-workspace")
        .await
        .expect("query parked")
        .expect("parked issue");
    assert_eq!(parked.lifecycle_stage, LifecycleStage::Blocked);
    assert_eq!(
        parked.blocker.expect("blocker").kind,
        "mnemesh_workspace_missing"
    );

    let report = daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate again");
    assert!(report.dispatched.is_empty());
    assert_eq!(report.blocked, vec!["SYM-250"]);
    assert!(opencode.launches().is_empty());
    assert!(client.transitions().is_empty());
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
    second.project_milestone = Some(symphony::linear::LinearMilestone {
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
async fn orchestration_dispatches_unblocked_todo_when_future_milestone_todo_is_blocked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");

    let current = linear_issue("current", "MNE-105", "Todo", Some(1));
    let mut future =
        linear_issue("future", "MNE-118", "Todo", Some(2)).blocked_by(vec![LinearBlocker {
            id: Some("future-blocker".into()),
            identifier: Some("MNE-130".into()),
            state: Some("Backlog".into()),
        }]);
    future.project_milestone = Some(symphony::linear::LinearMilestone {
        id: "future-milestone-id".into(),
        name: "Future Milestone".into(),
    });
    let client = RecordingLinearClient::new(vec![current, future]);

    let report = daemon::run_once_with_linear_client(&config, &store, &client)
        .await
        .expect("orchestrate once");

    assert_eq!(report.dispatched, vec!["MNE-105"]);
    assert_eq!(report.blocked, vec!["MNE-118"]);
    assert_eq!(
        client.transitions(),
        vec![("current".into(), LinearTransition::InProgress)]
    );
    let blocked = store
        .issue("symphony", "future")
        .await
        .expect("query future")
        .expect("future issue");
    assert_eq!(blocked.lifecycle_stage, LifecycleStage::Blocked);
    assert_eq!(blocked.blocker.expect("blocker").kind, "linear_blocker");
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
        .upsert_issue(test_issue("symphony", "parked-plan", "SYM-45"))
        .await
        .expect("persisted running backlog issue");
    store
        .upsert_issue(test_issue("symphony", "still-running", "SYM-46"))
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
async fn orchestration_keeps_owner_input_parked_and_blocks_manual_todo_dispatch() {
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
    store
        .upsert_issue(test_issue("symphony", "manual", "SYM-52"))
        .await
        .expect("manual issue");
    store
        .upsert_opencode_session({
            let mut session = test_session(
                "symphony",
                "manual",
                "ses-manual",
                "/home/agent/.symphony/workspaces/opencode/symphony/SYM-52",
            );
            session.process_id = Some(u32::MAX);
            session
        })
        .await
        .expect("manual stale session");
    let client = RecordingLinearClient::new(vec![parked, answered, manually_requeued]);

    let report = daemon::run_once_with_linear_client(&config, &store, &client)
        .await
        .expect("orchestrate once");

    assert!(report.dispatched.is_empty());
    assert_eq!(
        client.transitions(),
        vec![("answered".into(), LinearTransition::Todo)]
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
    assert_eq!(
        store
            .issue("symphony", "manual")
            .await
            .expect("query manual")
            .expect("manual")
            .lifecycle_stage,
        LifecycleStage::Queued
    );
    let manual_session = store
        .opencode_session("symphony", "manual", "ses-manual")
        .await
        .expect("query manual session")
        .expect("manual session");
    assert_eq!(manual_session.process_id, None);
    assert_eq!(manual_session.lifecycle_stage, LifecycleStage::Queued);
    assert_eq!(manual_session.stage, OpenCodeStage::Silent);
    assert_eq!(
        manual_session.lifecycle_marker.as_deref(),
        Some("waiting_for_project_owner_input")
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
    let mut stale = test_issue("symphony", "stale", "SYM-53");
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
    let mut parked = test_issue("symphony", "parked", "SYM-54");
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
        .upsert_issue(test_issue("symphony", "finished", "SYM-60"))
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
    assert_todo_transition(&restart_poll.transitions(), "new-work");
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
        .upsert_issue(test_issue("symphony", "work", "SYM-64"))
        .await
        .expect("issue");
    let mut active_process = Command::new("bash")
        .arg("-c")
        .arg("exec -a opencode sleep 120")
        .spawn()
        .expect("spawn active opencode-shaped process");
    thread::sleep(Duration::from_millis(100));
    store
        .upsert_opencode_session({
            let mut session = test_session(
                "symphony",
                "work",
                "ses-root",
                "/home/agent/.symphony/workspaces/opencode/symphony/SYM-64",
            );
            session.process_id = Some(active_process.id());
            session
        })
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
    let _ = active_process.kill();
    let _ = active_process.wait();
}

#[tokio::test]
async fn orchestration_repairs_stale_in_progress_session_without_handoff_sidecar() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "work", "SYM-65"))
        .await
        .expect("issue");
    let mut session = test_session(
        "symphony",
        "work",
        "ses-existing",
        "/home/agent/.symphony/workspaces/opencode/symphony/SYM-65",
    );
    session.process_id = None;
    session.stage = OpenCodeStage::Starting;
    session.lifecycle_marker = Some("acp_started".into());
    session.last_event = Some("acp_process_started".into());
    store
        .upsert_opencode_session(session)
        .await
        .expect("session");
    let client =
        RecordingLinearClient::new(vec![linear_issue("work", "SYM-65", "In Progress", Some(1))]);
    let opencode = ResumeRecordingOpenCodeLauncher::new(4242);

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert!(opencode.launches().is_empty());
    assert!(opencode.resumes().is_empty());
    assert!(opencode.continuations().is_empty());
    assert!(opencode.repairs().is_empty());
    assert_todo_transition(&client.transitions(), "work");
    assert!(client.evidence().iter().any(|(_, evidence)| {
        evidence.kind == "malformed_handoff"
            && evidence
                .body
                .contains(".symphony/opencode-handoff.json was not produced")
            && evidence
                .body
                .contains("fingerprint: missing_handoff_sidecar")
    }));
    let failed = store
        .opencode_session("symphony", "work", "ses-existing")
        .await
        .expect("query session")
        .expect("session");
    assert_eq!(failed.lifecycle_stage, LifecycleStage::Failed);
    assert_eq!(failed.stage, OpenCodeStage::Failed);
    assert_eq!(failed.process_id, None);
    assert_eq!(
        failed.lifecycle_marker.as_deref(),
        Some("failed:malformed_handoff")
    );
    assert_eq!(
        failed.last_event.as_deref(),
        Some("failed:missing_handoff_sidecar")
    );
    assert!(!failed.silence_observed);
}

#[tokio::test]
async fn orchestration_reissues_repair_prompt_for_stale_malformed_handoff_session_under_bound() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let mut issue = test_issue("symphony", "repair-stale", "SYM-66");
    issue.failure = Some(FailureRecord {
        kind: "malformed_handoff".into(),
        message: "opencode-handoff.json: unknown variant `planning`".into(),
        fingerprint: Some("malformed_handoff_sidecar".into()),
        occurrence_count: 1,
    });
    store.upsert_issue(issue).await.expect("issue");
    let mut session = test_session(
        "symphony",
        "repair-stale",
        "ses-repair-stale",
        "/home/agent/.symphony/workspaces/opencode/symphony/SYM-66",
    );
    session.process_id = None;
    session.lifecycle_marker = Some("repair_prompted".into());
    session.last_event = Some("repair_prompted:malformed_handoff_sidecar".into());
    store
        .upsert_opencode_session(session)
        .await
        .expect("session");
    let client = RecordingLinearClient::new(vec![linear_issue(
        "repair-stale",
        "SYM-66",
        "In Progress",
        Some(1),
    )]);
    let opencode = ResumeRecordingOpenCodeLauncher::new(4242);

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert!(opencode.launches().is_empty());
    assert!(opencode.resumes().is_empty());
    assert!(opencode.repairs().is_empty());
    assert_todo_transition(&client.transitions(), "repair-stale");
    let issue = store
        .issue("symphony", "repair-stale")
        .await
        .expect("query repair issue")
        .expect("repair issue");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Failed);
    let failure = issue.failure.expect("failure");
    assert_eq!(
        failure.fingerprint.as_deref(),
        Some("missing_handoff_sidecar")
    );
    assert_eq!(failure.occurrence_count, 1);

    let failed = store
        .opencode_session("symphony", "repair-stale", "ses-repair-stale")
        .await
        .expect("query session")
        .expect("session");
    assert_eq!(
        failed.last_event.as_deref(),
        Some("failed:missing_handoff_sidecar")
    );
    assert_eq!(
        failed.lifecycle_marker.as_deref(),
        Some("failed:malformed_handoff")
    );
}

#[tokio::test]
async fn orchestration_keeps_requeued_provider_blocker_blocked_without_continuation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");

    let mut issue = test_issue("symphony", "answered", "SYM-67");
    issue.lifecycle_stage = LifecycleStage::Blocked;
    issue.blocker = Some(BlockerRecord {
        kind: "provider_blocker".into(),
        message: "workspace-not-found".into(),
        observed_at: Some("2026-06-11T15:14:00Z".into()),
    });
    issue.failure = Some(FailureRecord {
        kind: "provider_blocker".into(),
        message: "workspace-not-found".into(),
        fingerprint: Some("workspace-not-found".into()),
        occurrence_count: 1,
    });
    store.upsert_issue(issue).await.expect("issue");
    let mut session = test_session(
        "symphony",
        "answered",
        "ses-owner-input",
        "/home/agent/.symphony/workspaces/opencode/symphony/SYM-67",
    );
    session.process_id = None;
    session.lifecycle_stage = LifecycleStage::Blocked;
    session.stage = OpenCodeStage::Failed;
    session.lifecycle_marker = Some("parked".into());
    session.last_event = Some("parked:provider_blocker".into());
    store
        .upsert_opencode_session(session)
        .await
        .expect("session");
    let client =
        RecordingLinearClient::new(vec![linear_issue("answered", "SYM-67", "Todo", Some(1))]);
    let opencode = ResumeRecordingOpenCodeLauncher::new(4242);

    let report = daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert_eq!(report.blocked, vec!["SYM-67"]);
    assert!(opencode.launches().is_empty());
    assert!(opencode.continuations().is_empty());
    assert!(client.transitions().is_empty());
    let parked = store
        .opencode_session("symphony", "answered", "ses-owner-input")
        .await
        .expect("query session")
        .expect("session");
    assert_eq!(parked.lifecycle_stage, LifecycleStage::Blocked);
    assert_eq!(parked.stage, OpenCodeStage::Failed);
    assert_eq!(parked.process_id, None);
    assert_eq!(parked.lifecycle_marker.as_deref(), Some("parked"));
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
    let mut closed = test_issue("symphony", "closed", "SYM-63");
    closed.cleanup_status = CleanupStatus::Pending;
    closed.git_ref = Some(GitRefRecord {
        branch: "agent-server/opencode-runner-extension".into(),
        worktree_path: missing_worktree.display().to_string(),
        head_sha: None,
        pr_url: None,
    });
    store.upsert_issue(closed).await.expect("closed issue");
    let mut stale_session = test_session("symphony", "closed", "oc-63", &missing_worktree);
    stale_session.process_id = Some(4242);
    store
        .upsert_opencode_session(stale_session)
        .await
        .expect("stale session");

    let client =
        RecordingLinearClient::new(vec![linear_issue("closed", "SYM-63", "Done", Some(1))]);

    let report = daemon::run_once_with_linear_client(&config, &store, &client)
        .await
        .expect("terminal reconciliation");

    assert_eq!(report.terminal_reconciled, vec!["SYM-63"]);
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
    let session = store
        .opencode_session("symphony", "closed", "oc-63")
        .await
        .expect("query reconciled session")
        .expect("reconciled session");
    assert_eq!(session.lifecycle_stage, LifecycleStage::Completed);
    assert_eq!(session.stage, OpenCodeStage::Completed);
    assert_eq!(session.process_id, None);
    assert_eq!(
        session.lifecycle_marker.as_deref(),
        Some("linear_terminal_reconciled")
    );
}

#[tokio::test]
async fn terminal_reconciliation_skips_unchanged_issue_and_session_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let missing_worktree = dir.path().join("already-removed").join("SYM-68");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let mut closed = test_issue("symphony", "closed-quiet", "SYM-68");
    closed.lifecycle_stage = LifecycleStage::Completed;
    closed.cleanup_status = CleanupStatus::Complete;
    closed.git_ref = Some(GitRefRecord {
        branch: "agent-server/opencode-runner-extension".into(),
        worktree_path: missing_worktree.display().to_string(),
        head_sha: None,
        pr_url: None,
    });
    store.upsert_issue(closed).await.expect("closed issue");
    let mut terminal_session = test_session("symphony", "closed-quiet", "oc-68", &missing_worktree);
    terminal_session.process_id = None;
    terminal_session.lifecycle_stage = LifecycleStage::Completed;
    terminal_session.stage = OpenCodeStage::Completed;
    terminal_session.lifecycle_marker = Some("linear_terminal_reconciled".into());
    terminal_session.last_event = Some("linear_terminal_reconciled".into());
    terminal_session.silence_observed = false;
    store
        .upsert_opencode_session(terminal_session)
        .await
        .expect("terminal session");
    set_issue_and_session_updated_at(&db_path, "closed-quiet", "oc-68", "2000-01-01 00:00:00")
        .await;

    let client = RecordingLinearClient::new(vec![linear_issue(
        "closed-quiet",
        "SYM-68",
        "Done",
        Some(1),
    )]);

    let report = daemon::run_once_with_linear_client(&config, &store, &client)
        .await
        .expect("quiet terminal reconciliation");

    assert!(
        report.terminal_reconciled.is_empty(),
        "unchanged terminal issue should not emit per-poll reconciliation events"
    );
    let (issue_updated_at, session_updated_at) =
        issue_and_session_updated_at(&db_path, "closed-quiet", "oc-68").await;
    assert_eq!(issue_updated_at, "2000-01-01 00:00:00");
    assert_eq!(session_updated_at, "2000-01-01 00:00:00");
}

async fn set_issue_and_session_updated_at(
    db_path: &Path,
    issue_id: &str,
    session_id: &str,
    updated_at: &str,
) {
    let database = libsql::Builder::new_local(db_path.display().to_string())
        .build()
        .await
        .expect("open database");
    let conn = database.connect().expect("connect");
    conn.execute(
        "UPDATE issues SET updated_at = ?1 WHERE project_id = 'symphony' AND issue_id = ?2",
        libsql::params![updated_at, issue_id],
    )
    .await
    .expect("set issue updated_at");
    conn.execute(
        "UPDATE opencode_sessions SET updated_at = ?1 WHERE project_id = 'symphony' AND issue_id = ?2 AND session_id = ?3",
        libsql::params![updated_at, issue_id, session_id],
    )
    .await
    .expect("set session updated_at");
}

async fn issue_and_session_updated_at(
    db_path: &Path,
    issue_id: &str,
    session_id: &str,
) -> (String, String) {
    let database = libsql::Builder::new_local(db_path.display().to_string())
        .build()
        .await
        .expect("open database");
    let conn = database.connect().expect("connect");
    let mut issue_rows = conn
        .query(
            "SELECT updated_at FROM issues WHERE project_id = 'symphony' AND issue_id = ?1",
            libsql::params![issue_id],
        )
        .await
        .expect("query issue updated_at");
    let issue_updated_at = issue_rows
        .next()
        .await
        .expect("issue row")
        .expect("issue exists")
        .get::<String>(0)
        .expect("issue updated_at");
    let mut session_rows = conn
        .query(
            "SELECT updated_at FROM opencode_sessions WHERE project_id = 'symphony' AND issue_id = ?1 AND session_id = ?2",
            libsql::params![issue_id, session_id],
        )
        .await
        .expect("query session updated_at");
    let session_updated_at = session_rows
        .next()
        .await
        .expect("session row")
        .expect("session exists")
        .get::<String>(0)
        .expect("session updated_at");
    (issue_updated_at, session_updated_at)
}

#[tokio::test]
async fn orchestration_treats_canceled_blocker_as_not_accepted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");

    let blocked =
        linear_issue("blocked", "SYM-42", "Todo", Some(1)).blocked_by(vec![LinearBlocker {
            id: Some("blocker-1".into()),
            identifier: Some("SYM-41".into()),
            state: Some("Canceled".into()),
        }]);
    let client = RecordingLinearClient::new(vec![blocked]);

    let report = daemon::run_once_with_linear_client(&config, &store, &client)
        .await
        .expect("orchestrate once");

    assert!(report.dispatched.is_empty());
    assert!(client.transitions().is_empty());
    assert_eq!(report.blocked, vec!["SYM-42"]);
    let blocked_row = store
        .issue("symphony", "blocked")
        .await
        .expect("query blocked")
        .expect("blocked row");
    assert_eq!(blocked_row.lifecycle_stage, LifecycleStage::Blocked);
    assert_eq!(
        blocked_row.blocker.expect("blocker").message,
        "SYM-41 is Canceled"
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
        .upsert_issue(test_issue("symphony", "requeued", "SYM-62"))
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
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Running);
    let sessions = store
        .opencode_sessions_for_issue("symphony", "requeued")
        .await
        .expect("sessions");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].lifecycle_stage, LifecycleStage::Running);
    assert_eq!(
        sessions[0].lifecycle_marker.as_deref(),
        Some("continuation_prompted")
    );
}

#[tokio::test]
async fn orchestration_launches_fresh_session_after_explicit_runtime_failure_cleanup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let worktree = dir.path().join("SYM-62-worktree");
    fs::create_dir_all(&worktree).expect("worktree");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "requeued", "SYM-62"))
        .await
        .expect("clean issue");
    store
        .upsert_opencode_session({
            let mut session = test_session("symphony", "requeued", "oc-62", &worktree);
            session.lifecycle_stage = LifecycleStage::Failed;
            session.stage = OpenCodeStage::Failed;
            session.process_id = None;
            session.lifecycle_marker = Some("failed:malformed_handoff".into());
            session.last_event = Some("failed:missing_git_closure".into());
            session
        })
        .await
        .expect("failed session");

    let client =
        RecordingLinearClient::new(vec![linear_issue("requeued", "SYM-62", "Todo", Some(1))]);
    let opencode = ResumeRecordingOpenCodeLauncher::new(4242);

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("poll");

    assert_eq!(
        client.transitions(),
        vec![("requeued".into(), LinearTransition::InProgress)]
    );
    assert_eq!(opencode.launches(), vec!["SYM-62"]);
    assert!(opencode.continuations().is_empty());
    assert!(opencode.repairs().is_empty());
    let sessions = store
        .opencode_sessions_for_issue("symphony", "requeued")
        .await
        .expect("sessions");
    assert_eq!(sessions.len(), 2);
    assert!(sessions.iter().any(|session| {
        session.session_id == "oc-62" && session.lifecycle_stage == LifecycleStage::Failed
    }));
    assert!(sessions.iter().any(|session| {
        session.session_id == "new:SYM-62" && session.lifecycle_stage == LifecycleStage::Running
    }));
}

#[tokio::test]
async fn orchestration_records_process_while_acp_session_new_is_still_pending() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let transcript_path = dir.path().join("acp-transcript.jsonl");
    let script_path = write_hanging_before_session_new_acp_script(dir.path(), &transcript_path);
    let configured = valid_config_toml().replace(
        "command = \"/usr/local/bin/opencode\"",
        &format!("command = \"{}\"", script_path.display()),
    );
    let config = RootConfig::from_toml_str(&configured).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let client = RecordingLinearClient::new(vec![linear_issue(
        "pending-session-new",
        "SYM-161",
        "Todo",
        Some(1),
    )]);
    let opencode = opencode::StdioOpenCodeLauncher;
    let poll = daemon::run_once_with_clients(&config, &store, &client, &opencode);
    tokio::pin!(poll);

    tokio::select! {
        result = &mut poll => panic!("poll finished before fake session/new hang: {result:?}"),
        () = tokio::time::sleep(Duration::from_millis(500)) => {}
    }

    assert_eq!(
        client.transitions(),
        vec![("pending-session-new".into(), LinearTransition::InProgress)]
    );
    let issue = store
        .issue("symphony", "pending-session-new")
        .await
        .expect("query issue")
        .expect("issue row");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Running);
    let sessions = store
        .opencode_sessions_for_issue("symphony", "pending-session-new")
        .await
        .expect("sessions");
    assert_eq!(sessions.len(), 1);
    let session = sessions.last().expect("provisional session");
    assert!(session.session_id.starts_with("starting:SYM-161:"));
    assert_eq!(session.lifecycle_stage, LifecycleStage::Running);
    assert_eq!(session.stage, OpenCodeStage::Starting);
    assert_eq!(
        session.lifecycle_marker.as_deref(),
        Some("acp_process_spawned")
    );
    assert!(
        session.process_id.is_some(),
        "process_id should be recorded before session/new returns"
    );

    if let Some(process_id) = session.process_id {
        let _ = Command::new("kill")
            .args(["-TERM", &process_id.to_string()])
            .status();
    }
}

#[tokio::test]
async fn orchestration_blocks_in_progress_issue_when_linear_blocker_is_not_done() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let worktree = dir.path().join("SYM-116-worktree");
    fs::create_dir_all(&worktree).expect("worktree");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let mut running = test_issue("symphony", "blocked-running", "SYM-116");
    running.lifecycle_stage = LifecycleStage::Running;
    store.upsert_issue(running).await.expect("running issue");
    let mut session = test_session("symphony", "blocked-running", "oc-116", &worktree);
    session.process_id = None;
    session.lifecycle_stage = LifecycleStage::Running;
    session.stage = OpenCodeStage::Running;
    store
        .upsert_opencode_session(session)
        .await
        .expect("session");

    let blocked =
        linear_issue("blocked-running", "SYM-116", "In Progress", Some(1)).blocked_by(vec![
            LinearBlocker {
                id: Some("blocker-115".into()),
                identifier: Some("MNE-115".into()),
                state: Some("In Progress".into()),
            },
        ]);
    let client = RecordingLinearClient::new(vec![blocked]);
    let opencode = ResumeRecordingOpenCodeLauncher::new(4242);

    let report = daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate once");

    assert!(report.dispatched.is_empty());
    assert_eq!(report.blocked, vec!["SYM-116"]);
    assert_eq!(
        client.transitions(),
        vec![("blocked-running".into(), LinearTransition::Todo)]
    );
    assert!(opencode.continuations().is_empty());
    assert!(opencode.resumes().is_empty());
    let issue = store
        .issue("symphony", "blocked-running")
        .await
        .expect("query blocked")
        .expect("blocked row");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Blocked);
    assert_eq!(
        issue.blocker.expect("blocker").message,
        "MNE-115 is In Progress"
    );
    let sessions = store
        .opencode_sessions_for_issue("symphony", "blocked-running")
        .await
        .expect("sessions");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].lifecycle_stage, LifecycleStage::Queued);
    assert_eq!(sessions[0].process_id, None);
    assert_eq!(
        sessions[0].lifecycle_marker.as_deref(),
        Some("waiting_for_blocker")
    );
}

#[tokio::test]
async fn orchestration_capacity_gates_requeued_issue_with_existing_session() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let worktree = dir.path().join("SYM-65-worktree");
    fs::create_dir_all(&worktree).expect("worktree");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "running-1", "SYM-60"))
        .await
        .expect("running issue 1");
    store
        .upsert_issue(test_issue("symphony", "running-2", "SYM-61"))
        .await
        .expect("running issue 2");
    let mut requeued = test_issue("symphony", "requeued", "SYM-65");
    requeued.lifecycle_stage = LifecycleStage::Queued;
    store.upsert_issue(requeued).await.expect("requeued issue");
    store
        .upsert_opencode_session(test_session("symphony", "requeued", "oc-65", &worktree))
        .await
        .expect("running session");

    let client =
        RecordingLinearClient::new(vec![linear_issue("requeued", "SYM-65", "Todo", Some(1))]);

    let report = daemon::run_once_with_linear_client(&config, &store, &client)
        .await
        .expect("poll");

    assert!(report.dispatched.is_empty());
    assert!(client.transitions().is_empty());
    let issue = store
        .issue("symphony", "requeued")
        .await
        .expect("query requeued")
        .expect("requeued");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Queued);
    let sessions = store
        .opencode_sessions_for_issue("symphony", "requeued")
        .await
        .expect("sessions");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].lifecycle_stage, LifecycleStage::Queued);
    assert_eq!(sessions[0].process_id, None);
    assert_eq!(
        sessions[0].lifecycle_marker.as_deref(),
        Some("waiting_for_capacity")
    );
    let liveness = store
        .project_liveness("symphony")
        .await
        .expect("query liveness")
        .expect("liveness row");
    assert_eq!(liveness.status, RuntimeLivenessStatus::CapacityFull);
    assert_eq!(liveness.running_sessions, 2);
    assert_eq!(liveness.available_sessions, 0);
}

#[tokio::test]
async fn orchestration_blocks_in_progress_issue_without_session_as_runtime_defect() {
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

    assert_todo_transition(&client.transitions(), "lost-session");
    let issue = store
        .issue("symphony", "lost-session")
        .await
        .expect("query issue")
        .expect("issue");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Failed);
    assert_eq!(
        issue.failure.expect("failure").fingerprint.as_deref(),
        Some("missing_active_session")
    );

    let report = daemon::run_once_with_linear_client(&config, &store, &client)
        .await
        .expect("second poll");
    assert_eq!(
        report.blocked,
        vec!["SYM-64"],
        "recorded runtime defects should be retained instead of reprocessed as missing sessions"
    );
    let retained = store
        .issue("symphony", "lost-session")
        .await
        .expect("query retained issue")
        .expect("retained issue");
    assert_eq!(retained.lifecycle_stage, LifecycleStage::Failed);
    assert_eq!(
        retained.blocker.expect("runtime blocker").kind,
        "runtime_defect"
    );
}

#[tokio::test]
async fn orchestration_records_launch_failure_without_aborting_poll_or_owner_input() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let client = RecordingLinearClient::new(vec![
        linear_issue("launch-fails", "SYM-201", "Todo", Some(1)),
        linear_issue("still-runs", "SYM-202", "Todo", Some(2)),
    ]);
    let opencode = FailingLaunchOpenCodeLauncher::new("existing worktree is dirty");

    let report = daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("launch failure must not abort poll");

    assert!(report.dispatched.is_empty());
    assert_eq!(
        client.transitions(),
        vec![
            ("launch-fails".into(), LinearTransition::InProgress),
            ("still-runs".into(), LinearTransition::InProgress),
        ]
    );
    let evidence = client.evidence();
    assert_eq!(evidence.len(), 4);
    assert!(
        evidence
            .iter()
            .all(|(_, evidence)| evidence.kind == "runtime_defect")
    );
    let launch_evidence = evidence
        .iter()
        .find(|(issue_id, evidence)| {
            issue_id == "launch-fails" && evidence.body.contains("runtime_defect: launch_failed")
        })
        .expect("source launch failure evidence");
    assert!(launch_evidence.1.body.contains("issue_id: launch-fails"));
    assert!(launch_evidence.1.body.contains("attempted_worktree_path:"));
    assert!(
        launch_evidence
            .1
            .body
            .contains("expected_branch: feature/sym-201")
    );
    assert!(launch_evidence.1.body.contains("elapsed_seconds: unknown"));
    assert!(
        launch_evidence
            .1
            .body
            .contains("existing worktree is dirty")
    );
    let managed = client.managed_issues();
    assert_eq!(managed.len(), 1);
    assert!(
        managed
            .iter()
            .all(|issue| { issue.priority == 1 && issue.state == ManagedLinearIssueState::Todo })
    );
    assert!(managed.iter().any(|issue| {
        issue.source_issue_id == "launch-fails" && issue.fingerprint == "launch_failed"
    }));
    assert!(client.relations().iter().any(|relation| {
        relation
            == &(
                "launch-fails".into(),
                "managed-1".into(),
                ManagedLinearRelation::Blocks,
            )
    }));
    let issue = store
        .issue("symphony", "launch-fails")
        .await
        .expect("query issue")
        .expect("issue");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Failed);
    assert_eq!(
        issue.failure.expect("failure").fingerprint.as_deref(),
        Some("launch_failed")
    );
}

#[tokio::test]
async fn orchestration_persists_setup_failure_session_for_liveness_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let client = RecordingLinearClient::new(vec![linear_issue(
        "setup-fails",
        "SYM-210",
        "Todo",
        Some(1),
    )]);

    daemon::run_once_with_clients(&config, &store, &client, &SetupFailingOpenCodeLauncher)
        .await
        .expect("setup failure must not abort poll");

    let sessions = store
        .opencode_sessions_for_issue("symphony", "setup-fails")
        .await
        .expect("sessions");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].lifecycle_stage, LifecycleStage::Failed);
    assert_eq!(sessions[0].stage, OpenCodeStage::Failed);
    assert_eq!(sessions[0].process_id, Some(4242));
    assert_eq!(
        sessions[0].lifecycle_marker.as_deref(),
        Some("setup_failed:setup failed before session attachment")
    );
    assert!(
        sessions[0]
            .last_event
            .as_deref()
            .expect("last event")
            .starts_with("setup_failed:4242:")
    );

    let no_candidates = RecordingLinearClient::new(Vec::new());
    daemon::run_once_with_linear_client(&config, &store, &no_candidates)
        .await
        .expect("second liveness poll");
    let liveness = store
        .project_liveness("symphony")
        .await
        .expect("query liveness")
        .expect("liveness row");
    assert_eq!(liveness.status, RuntimeLivenessStatus::RunnerSetupFailed);
}

#[tokio::test]
async fn orchestration_persists_stale_killed_session_event_through_continuation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "stale-live", "SYM-211"))
        .await
        .expect("issue");
    let opencode_sleep = dir.path().join("opencode-sleep");
    std::os::unix::fs::symlink("/bin/sleep", &opencode_sleep).expect("opencode sleep symlink");
    let mut stale_process = Command::new(&opencode_sleep)
        .arg("120")
        .spawn()
        .expect("spawn stale opencode-shaped process");
    thread::sleep(Duration::from_millis(100));
    let mut session = test_session(
        "symphony",
        "stale-live",
        "ses-stale-live",
        "/home/agent/.symphony/workspaces/opencode/symphony/SYM-211",
    );
    session.process_id = Some(stale_process.id());
    store
        .upsert_opencode_session(session)
        .await
        .expect("session");
    let client =
        RecordingLinearClient::new(vec![linear_issue("stale-live", "SYM-211", "Todo", Some(1))]);
    let opencode = ResumeRecordingOpenCodeLauncher::new(5151);

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("orchestrate stale continuation");

    let resumed = store
        .opencode_session("symphony", "stale-live", "ses-stale-live")
        .await
        .expect("query session")
        .expect("session");
    assert_eq!(resumed.lifecycle_stage, LifecycleStage::Running);
    assert_eq!(resumed.stage, OpenCodeStage::Running);
    assert_eq!(
        resumed.lifecycle_marker.as_deref(),
        Some("continuation_prompted")
    );
    assert!(
        resumed
            .last_event
            .as_deref()
            .expect("last event")
            .starts_with(&format!("stale_killed:{}:", stale_process.id())),
        "last_event={:?}",
        resumed.last_event
    );

    let no_candidates = RecordingLinearClient::new(Vec::new());
    daemon::run_once_with_linear_client(&config, &store, &no_candidates)
        .await
        .expect("second liveness poll");
    let liveness = store
        .project_liveness("symphony")
        .await
        .expect("query liveness")
        .expect("liveness row");
    assert_eq!(liveness.status, RuntimeLivenessStatus::RunnerStaleKilled);
    let _ = stale_process.kill();
    let _ = stale_process.wait();
}

#[tokio::test]
async fn orchestration_ignores_historical_failed_session_for_in_progress_reconciliation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let worktree = dir.path().join("SYM-203-worktree");
    fs::create_dir_all(&worktree).expect("worktree");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let mut issue = test_issue("symphony", "historical", "SYM-203");
    issue.lifecycle_stage = LifecycleStage::Running;
    store.upsert_issue(issue).await.expect("issue");
    store
        .upsert_opencode_session({
            let mut session = test_session("symphony", "historical", "zz-failed", &worktree);
            session.lifecycle_stage = LifecycleStage::Failed;
            session.stage = OpenCodeStage::Failed;
            session.lifecycle_marker = Some("failed:malformed_handoff".into());
            session.last_event = Some("failed:missing_handoff_sidecar".into());
            session
        })
        .await
        .expect("failed session");
    let client = RecordingLinearClient::new(vec![linear_issue(
        "historical",
        "SYM-203",
        "In Progress",
        Some(1),
    )]);
    let opencode = ScriptedOpenCodeLauncher::new(Some(success_handoff(
        "zz-failed",
        &worktree,
        "feature/sym-203",
        "abc",
    )));

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("poll");

    assert_todo_transition(&client.transitions(), "historical");
    assert!(client.evidence().is_empty());
    let issue = store
        .issue("symphony", "historical")
        .await
        .expect("query issue")
        .expect("issue");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Failed);
    assert_eq!(
        issue.failure.expect("failure").fingerprint.as_deref(),
        Some("missing_active_session")
    );
    let sessions = store
        .opencode_sessions_for_issue("symphony", "historical")
        .await
        .expect("sessions");
    assert_eq!(sessions[0].process_id, None);
    assert_eq!(
        sessions[0].last_event.as_deref(),
        Some("stale_failed_session_ignored")
    );
}

#[tokio::test]
async fn orchestration_does_not_reuse_failed_stage_session_for_todo_dispatch() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let worktree = dir.path().join("SYM-204-worktree");
    fs::create_dir_all(&worktree).expect("worktree");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let mut issue = test_issue("symphony", "failed-requeue", "SYM-204");
    issue.lifecycle_stage = LifecycleStage::Queued;
    store.upsert_issue(issue).await.expect("issue");
    let mut failed = test_session("symphony", "failed-requeue", "ses-failed", &worktree);
    failed.lifecycle_stage = LifecycleStage::Queued;
    failed.stage = OpenCodeStage::Failed;
    failed.lifecycle_marker = Some("failed:launch_failed".into());
    failed.last_event = Some("failed:launch_failed".into());
    store
        .upsert_opencode_session(failed)
        .await
        .expect("failed session");
    let client = RecordingLinearClient::new(vec![linear_issue(
        "failed-requeue",
        "SYM-204",
        "Todo",
        Some(1),
    )]);
    let opencode = ResumeRecordingOpenCodeLauncher::new(6204);

    daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("poll");

    assert_eq!(opencode.launches(), vec!["SYM-204"]);
    assert!(opencode.continuations().is_empty());
    let sessions = store
        .opencode_sessions_for_issue("symphony", "failed-requeue")
        .await
        .expect("sessions");
    assert!(sessions.iter().any(|session| {
        session.session_id == "ses-failed"
            && session.lifecycle_stage == LifecycleStage::Queued
            && session.stage == OpenCodeStage::Failed
    }));
    assert!(sessions.iter().any(|session| {
        session.session_id == "new:SYM-204"
            && session.lifecycle_stage == LifecycleStage::Running
            && session.stage == OpenCodeStage::Starting
    }));
}

#[tokio::test]
async fn orchestration_blocker_does_not_reactivate_failed_session() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let worktree = dir.path().join("SYM-205-worktree");
    fs::create_dir_all(&worktree).expect("worktree");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let mut issue = test_issue("symphony", "blocked-failed", "SYM-205");
    issue.lifecycle_stage = LifecycleStage::Queued;
    store.upsert_issue(issue).await.expect("issue");
    let mut failed = test_session("symphony", "blocked-failed", "ses-failed", &worktree);
    failed.lifecycle_stage = LifecycleStage::Queued;
    failed.stage = OpenCodeStage::Failed;
    failed.lifecycle_marker = Some("failed:launch_failed".into());
    failed.last_event = Some("failed:launch_failed".into());
    store
        .upsert_opencode_session(failed)
        .await
        .expect("failed session");
    let blocked = linear_issue("blocked-failed", "SYM-205", "Todo", Some(1)).blocked_by(vec![
        LinearBlocker {
            id: Some("blocker-205".into()),
            identifier: Some("SYM-204".into()),
            state: Some("In Progress".into()),
        },
    ]);
    let client = RecordingLinearClient::new(vec![blocked]);
    let opencode = ResumeRecordingOpenCodeLauncher::new(6205);

    let report = daemon::run_once_with_clients(&config, &store, &client, &opencode)
        .await
        .expect("poll");

    assert_eq!(report.blocked, vec!["SYM-205"]);
    assert!(client.transitions().is_empty());
    assert!(opencode.launches().is_empty());
    assert!(opencode.continuations().is_empty());
    let session = store
        .opencode_session("symphony", "blocked-failed", "ses-failed")
        .await
        .expect("query session")
        .expect("session");
    assert_eq!(session.lifecycle_stage, LifecycleStage::Queued);
    assert_eq!(session.stage, OpenCodeStage::Failed);
    assert_eq!(
        session.lifecycle_marker.as_deref(),
        Some("failed:launch_failed")
    );
}
