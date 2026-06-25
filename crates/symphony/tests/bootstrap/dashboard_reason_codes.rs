use super::*;

#[tokio::test]
async fn dashboard_api_surfaces_primary_execution_reason_codes_and_root_is_json_404() {
    async fn project_for(
        configure: impl FnOnce(&mut IssueStateRecord, &mut Option<RunnerSessionRecord>),
        liveness: Option<(RuntimeLivenessStatus, &'static str, u32, u32)>,
    ) -> symphony::api::ProjectDashboardResponse {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime.sqlite3");
        let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
        let store = SqliteStore::open(&db_path).await.expect("open sqlite");
        store.migrate().await.expect("migrate");
        store.reconcile_projects(&config).await.expect("projects");
        let mut issue = test_issue("symphony", "reason", "SYM-134");
        let mut session = None;
        configure(&mut issue, &mut session);
        store.upsert_issue(issue).await.expect("issue");
        if let Some(session) = session {
            store.upsert_runner_session(session).await.expect("session");
        }
        if let Some((status, reason, max_sessions, running_sessions)) = liveness {
            store
                .mark_project_liveness_poll(
                    "symphony",
                    status,
                    reason,
                    max_sessions,
                    running_sessions,
                    true,
                )
                .await
                .expect("liveness");
        }
        RuntimeDashboardApi::from_store(&config, &store)
            .await
            .expect("dashboard api")
            .project_drilldown("symphony")
            .expect("project endpoint")
            .expect("project exists")
            .clone()
    }

    let disabled = {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime.sqlite3");
        let config = RootConfig::from_toml_str(&valid_config_toml().replacen(
            "enabled = true",
            "enabled = false",
            1,
        ))
        .expect("config");
        let store = SqliteStore::open(&db_path).await.expect("open sqlite");
        store.migrate().await.expect("migrate");
        store.reconcile_projects(&config).await.expect("projects");
        RuntimeDashboardApi::from_store(&config, &store)
            .await
            .expect("dashboard api")
            .project_drilldown("symphony")
            .expect("project endpoint")
            .expect("project exists")
            .clone()
    };
    assert_eq!(disabled.liveness.primary_reason_code, "disabled_project");

    let inactive = {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime.sqlite3");
        let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
        let store = SqliteStore::open(&db_path).await.expect("open sqlite");
        store.migrate().await.expect("migrate");
        store.reconcile_projects(&config).await.expect("projects");
        RuntimeDashboardApi::from_store(&config, &store)
            .await
            .expect("dashboard api")
            .project_drilldown("symphony")
            .expect("project endpoint")
            .expect("project exists")
            .clone()
    };
    assert_eq!(inactive.liveness.primary_reason_code, "inactive_runtime");

    let no_eligible = project_for(
        |issue, _| issue.lifecycle_stage = LifecycleStage::Completed,
        Some((
            RuntimeLivenessStatus::NoEligibleIssues,
            "candidate scan found no eligible issues",
            2,
            0,
        )),
    )
    .await;
    assert_eq!(no_eligible.liveness.primary_reason_code, "idle");

    let linear_blocked = project_for(
        |issue, _| {
            issue.lifecycle_stage = LifecycleStage::Blocked;
            issue.blocker = Some(BlockerRecord {
                kind: "linear_blocker".into(),
                message: "SYM-1 blocks dispatch".into(),
                observed_at: None,
            });
        },
        Some((
            RuntimeLivenessStatus::BlockedIssues,
            "candidate issues exist but are blocked or parked",
            2,
            0,
        )),
    )
    .await;
    assert_eq!(
        linear_blocked.liveness.primary_reason_code,
        "linear_blockers"
    );

    let owner_input = project_for(
        |issue, _| {
            issue.lifecycle_stage = LifecycleStage::Blocked;
            issue.blocker = Some(BlockerRecord {
                kind: "owner_input".into(),
                message: "owner answer required".into(),
                observed_at: None,
            });
        },
        Some((
            RuntimeLivenessStatus::BlockedIssues,
            "candidate issues exist but are blocked or parked",
            2,
            0,
        )),
    )
    .await;
    assert_eq!(
        owner_input.liveness.primary_reason_code,
        "owner_input_parked"
    );

    let provider_blocker = project_for(
        |issue, _| {
            issue.lifecycle_stage = LifecycleStage::Blocked;
            issue.blocker = Some(BlockerRecord {
                kind: "provider_blocker".into(),
                message: "runner ProviderAuthError".into(),
                observed_at: None,
            });
            issue.failure = Some(FailureRecord {
                kind: "provider_blocker".into(),
                message: "runner provider auth failed".into(),
                fingerprint: Some("opencode_providerautherror_api_key_missing".into()),
                occurrence_count: 1,
            });
        },
        Some((
            RuntimeLivenessStatus::BlockedIssues,
            "candidate issues exist but are blocked or parked",
            2,
            0,
        )),
    )
    .await;
    assert_eq!(
        provider_blocker.liveness.primary_reason_code,
        "provider_blocker"
    );

    let capacity_full = project_for(
        |_, _| {},
        Some((
            RuntimeLivenessStatus::CapacityFull,
            "dispatch capacity is full",
            2,
            2,
        )),
    )
    .await;
    assert_eq!(capacity_full.liveness.primary_reason_code, "capacity_full");

    let capacity_available = project_for(
        |_, _| {},
        Some((
            RuntimeLivenessStatus::HealthyCapacityAvailable,
            "eligible issue exists and dispatch capacity is available",
            2,
            0,
        )),
    )
    .await;
    assert_eq!(
        capacity_available.liveness.primary_reason_code,
        "capacity_available"
    );

    let active_session = project_for(
        |issue, session| {
            *session = Some(test_session(
                &issue.project_id,
                &issue.issue_id,
                "oc-active",
                "/tmp/symphony-active",
            ));
        },
        Some((
            RuntimeLivenessStatus::NoEligibleIssues,
            "active runner has no additional runnable issue",
            2,
            1,
        )),
    )
    .await;
    assert_eq!(
        active_session.liveness.primary_reason_code,
        "active_runner_session"
    );
    assert_eq!(active_session.liveness.capacity.available_sessions, 1);
    assert!(
        active_session
            .liveness
            .primary_reason_detail
            .contains("no additional runnable candidate")
    );

    let no_second_runnable = project_for(
        |issue, session| {
            *session = Some(test_session(
                &issue.project_id,
                &issue.issue_id,
                "oc-no-second",
                "/tmp/symphony-no-second",
            ));
        },
        Some((
            RuntimeLivenessStatus::BlockedIssues,
            "candidate issues exist but are blocked or parked",
            2,
            1,
        )),
    )
    .await;
    assert_eq!(
        no_second_runnable.liveness.primary_reason_code,
        "active_runner_session"
    );
    assert_eq!(no_second_runnable.liveness.capacity.available_sessions, 1);
    assert!(
        no_second_runnable
            .liveness
            .primary_reason_detail
            .contains("no additional runnable candidate")
    );

    let runner_dead = project_for(
        |issue, session| {
            let mut dead = test_session(
                &issue.project_id,
                &issue.issue_id,
                "oc-dead",
                "/tmp/symphony-dead",
            );
            dead.process_id = Some(u32::MAX);
            *session = Some(dead);
        },
        Some((
            RuntimeLivenessStatus::RunnerProcessDead,
            "at least one running runner session has no live runner process",
            2,
            1,
        )),
    )
    .await;
    assert_eq!(runner_dead.liveness.primary_reason_code, "runner_dead");

    let waiting_handoff = project_for(
        |issue, session| {
            let mut handoff = test_session(
                &issue.project_id,
                &issue.issue_id,
                "oc-handoff",
                "/tmp/symphony-handoff",
            );
            handoff.stage = RunnerStage::Handoff;
            *session = Some(handoff);
        },
        Some((
            RuntimeLivenessStatus::HealthyCapacityAvailable,
            "eligible issue exists and dispatch capacity is available",
            2,
            1,
        )),
    )
    .await;
    assert_eq!(
        waiting_handoff.liveness.primary_reason_code,
        "waiting_for_handoff"
    );

    let cleanup_pending = project_for(
        |issue, _| {
            issue.lifecycle_stage = LifecycleStage::Completed;
            issue.cleanup_status = CleanupStatus::Pending;
        },
        Some((
            RuntimeLivenessStatus::NoEligibleIssues,
            "candidate scan found no eligible issues",
            2,
            0,
        )),
    )
    .await;
    assert_eq!(
        cleanup_pending.liveness.primary_reason_code,
        "cleanup_pending"
    );

    let worktree_blocked = project_for(
        |issue, _| {
            issue.lifecycle_stage = LifecycleStage::Failed;
            issue.failure = Some(FailureRecord {
                kind: "runtime_launch_failed".into(),
                message: "runner worktree validation failed: existing worktree is on wrong branch"
                    .into(),
                fingerprint: Some("launch_failed".into()),
                occurrence_count: 1,
            });
        },
        Some((
            RuntimeLivenessStatus::BlockedIssues,
            "candidate issues exist but are blocked or parked",
            2,
            0,
        )),
    )
    .await;
    assert_eq!(
        worktree_blocked.liveness.primary_reason_code,
        "worktree_blocked"
    );
    assert!(
        worktree_blocked
            .liveness
            .primary_reason_detail
            .contains("worktree validation failed")
    );

    let git_closure_blocked = project_for(
        |issue, _| {
            issue.lifecycle_stage = LifecycleStage::Failed;
            issue.failure = Some(FailureRecord {
                kind: "handoff_git_closure_failed".into(),
                message: "runner git closure validation failed: missing pushed branch evidence"
                    .into(),
                fingerprint: Some("git_closure_unverified".into()),
                occurrence_count: 1,
            });
        },
        Some((
            RuntimeLivenessStatus::BlockedIssues,
            "candidate issues exist but are blocked or parked",
            2,
            0,
        )),
    )
    .await;
    assert_eq!(
        git_closure_blocked.liveness.primary_reason_code,
        "git_closure_blocked"
    );
    assert!(
        git_closure_blocked
            .liveness
            .primary_reason_detail
            .contains("git closure validation failed")
    );

    let runtime_defect_blocked = project_for(
        |issue, _| {
            issue.lifecycle_stage = LifecycleStage::Failed;
            issue.failure = Some(FailureRecord {
                kind: "malformed_handoff".into(),
                message: "successful handoff did not include git closure evidence".into(),
                fingerprint: Some("missing_git_closure".into()),
                occurrence_count: 1,
            });
        },
        Some((
            RuntimeLivenessStatus::BlockedIssues,
            "candidate issues exist but are blocked or parked",
            2,
            0,
        )),
    )
    .await;
    assert_eq!(
        runtime_defect_blocked.liveness.primary_reason_code,
        "runtime_defect_blocked"
    );
    assert!(
        runtime_defect_blocked
            .liveness
            .primary_reason_detail
            .contains("git closure evidence")
    );
    let runtime_defect = runtime_defect_blocked.active_issues[0]
        .runtime_defect
        .as_ref()
        .expect("runtime defect projection");
    assert_eq!(runtime_defect.classification, "malformed_handoff");
    assert_eq!(
        runtime_defect.fingerprint.as_deref(),
        Some("missing_git_closure")
    );
    assert_eq!(runtime_defect.repair_attempt_count, 1);
    assert_eq!(runtime_defect.next_action, "queue_repair");

    let active_runtime_repair = project_for(
        |issue, session| {
            issue.lifecycle_stage = LifecycleStage::Running;
            issue.failure = Some(FailureRecord {
                kind: "malformed_handoff".into(),
                message: "repairing malformed handoff from previous runner session".into(),
                fingerprint: Some("session_id_mismatch".into()),
                occurrence_count: 2,
            });
            *session = Some(test_session(
                &issue.project_id,
                &issue.issue_id,
                "oc-repair-runtime-defect",
                "/tmp/symphony-runtime-repair",
            ));
        },
        Some((
            RuntimeLivenessStatus::BlockedIssues,
            "downstream blocked issues should not mask active repair",
            2,
            1,
        )),
    )
    .await;
    assert_eq!(
        active_runtime_repair.liveness.primary_reason_code,
        "active_runner_session"
    );
    assert_eq!(
        active_runtime_repair.active_issues[0]
            .runtime_defect
            .as_ref()
            .expect("runtime defect")
            .next_action,
        "continue_repair"
    );
    assert_eq!(
        active_runtime_repair.active_issues[0].display_status,
        "runtime repair"
    );

    let stale_session = project_for(
        |issue, session| {
            let mut stale = test_session(
                &issue.project_id,
                &issue.issue_id,
                "oc-stale-killed",
                "/tmp/symphony-stale",
            );
            stale.stage = RunnerStage::Silent;
            stale.process_id = Some(u32::MAX);
            *session = Some(stale);
        },
        Some((
            RuntimeLivenessStatus::RunnerStaleKilled,
            "stale runner session was killed",
            2,
            1,
        )),
    )
    .await;
    assert_eq!(stale_session.liveness.primary_reason_code, "runner_dead");

    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let json = symphony::api::runtime_api_json_response(&config, &store, "/api/dashboard")
        .await
        .expect("json response");
    let root = symphony::api::runtime_api_json_response(&config, &store, "/")
        .await
        .expect("root response");
    assert!(
        json.body
            .contains(r#""primary_reason_code":"inactive_runtime""#)
    );
    assert!(
        json.body
            .contains(r#""primary_reason_detail":"runtime has not reported"#)
    );
    assert_eq!(root.status, 404);
    assert_eq!(root.content_type, "application/json");
    assert!(!root.body.contains("<html"));
}
