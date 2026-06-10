use super::*;

#[tokio::test]
async fn multiproject_toml_config_loads_deterministically_and_validates_required_fields() {
    let first = RootConfig::from_toml_str(valid_config_toml()).expect("valid root config");
    let second = RootConfig::from_toml_str(valid_config_toml()).expect("valid root config");

    assert_eq!(first, second);
    assert_eq!(first.projects().len(), 1);

    let project = first.project("symphony").expect("project lookup");
    assert_eq!(project.linear.team_key, "SYM");
    assert_eq!(
        project.opencode.command,
        PathBuf::from("/usr/local/bin/opencode")
    );
    assert_eq!(project.opencode.args, vec!["acp"]);
    assert_eq!(project.concurrency.max_sessions, 2);
    assert!(first.cleanup.enabled);
    assert_eq!(first.cleanup.interval_secs, 300);
    assert_eq!(first.cleanup.retention_secs, 86_400);
    assert!(first.opencode_storage.is_none());

    let missing_required =
        valid_config_toml().replace("repo_path = \"/home/agent/proj/symphony\"\n", "");
    let err = RootConfig::from_toml_str(&missing_required).expect_err("repo_path is required");
    assert!(err.to_string().contains("repo_path"), "{err}");
}

#[tokio::test]
async fn opencode_storage_config_loads_and_validates_archive_paths() {
    let configured = valid_config_toml().replace(
        "[[projects]]\n",
        "[opencode_storage]\ndatabase_path = \"/home/agent/.local/share/opencode/opencode.db\"\narchive_root = \"/home/agent/mnemesh-benchmark-runs/symphony-opencode-session-archives\"\n\n[[projects]]\n",
    );
    let config = RootConfig::from_toml_str(&configured).expect("opencode storage config");
    let storage = config.opencode_storage.expect("storage config");

    assert_eq!(
        storage.database_path,
        PathBuf::from("/home/agent/.local/share/opencode/opencode.db")
    );
    assert_eq!(
        storage.archive_root,
        PathBuf::from("/home/agent/mnemesh-benchmark-runs/symphony-opencode-session-archives")
    );

    let invalid = configured.replace(
        "database_path = \"/home/agent/.local/share/opencode/opencode.db\"",
        "database_path = \"\"",
    );
    let err =
        RootConfig::from_toml_str(&invalid).expect_err("empty OpenCode database path rejected");
    assert!(
        err.to_string().contains("opencode_storage.database_path"),
        "{err}"
    );
}

#[tokio::test]
async fn cleanup_config_loads_and_validates_runtime_retention() {
    let configured = valid_config_toml().replace(
        "[[projects]]\n",
        "[cleanup]\nenabled = true\ninterval_secs = 60\nretention_secs = 3600\n\n[[projects]]\n",
    );
    let config = RootConfig::from_toml_str(&configured).expect("cleanup config");

    assert!(config.cleanup.enabled);
    assert_eq!(config.cleanup.interval_secs, 60);
    assert_eq!(config.cleanup.retention_secs, 3600);

    let invalid = configured.replace("interval_secs = 60", "interval_secs = 0");
    let err = RootConfig::from_toml_str(&invalid).expect_err("zero interval must be rejected");
    assert!(err.to_string().contains("cleanup.interval_secs"), "{err}");
}

#[tokio::test]
async fn linear_graphql_client_fetches_project_candidates_transitions_and_records_evidence() {
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let project = config.project("symphony").expect("project");
    let transport = RecordingGraphqlTransport::new(vec![
        serde_json::json!({
            "data": {
                "issues": {
                    "nodes": [
                        {
                            "id": "issue-1",
                            "identifier": "SYM-100",
                            "title": "Live issue",
                            "description": "Implement live polling",
                            "state": { "name": "Todo" },
                            "priority": 1,
                            "branchName": "agent-server/opencode-runner-extension",
                            "url": "https://linear.example/SYM-100",
                            "projectMilestone": {
                                "id": "milestone-from-linear",
                                "name": "Milestone From Linear"
                            },
                            "labels": { "nodes": [{ "name": "vnext" }] },
                            "comments": {
                                "nodes": [
                                    {
                                        "body": "## OpenCode Handoff\nrepair handoff for SYM-100",
                                        "parent": null,
                                        "createdAt": "2026-06-10T00:01:00Z"
                                    }
                                ]
                            },
                            "relations": {
                                "nodes": [
                                    {
                                        "type": "blocked_by",
                                        "relatedIssue": {
                                            "id": "blocker-1",
                                            "identifier": "SYM-99",
                                            "state": { "name": "Done" }
                                        }
                                    }
                                ]
                            },
                            "createdAt": "2026-06-10T00:00:00Z",
                            "updatedAt": "2026-06-10T00:01:00Z"
                        },
                        {
                            "id": "issue-2",
                            "identifier": "SYM-101",
                            "title": "Owner answered issue",
                            "description": "Resume after owner input",
                            "state": { "name": "Need Owner Input" },
                            "priority": 2,
                            "branchName": "agent-server/opencode-runner-extension",
                            "url": "https://linear.example/SYM-101",
                            "projectMilestone": {
                                "id": "milestone-from-linear",
                                "name": "Milestone From Linear"
                            },
                            "labels": { "nodes": [] },
                            "comments": {
                                "nodes": [
                                    {
                                        "body": "yes, continue",
                                        "parent": null,
                                        "createdAt": "2026-06-10T00:03:00Z"
                                    },
                                    {
                                        "body": "## OpenCode Handoff\nmachine status update",
                                        "parent": { "id": "owner-comment-thread" },
                                        "createdAt": "2026-06-10T00:04:00Z"
                                    },
                                    {
                                        "body": "kind: owner_question\n\nwaiting for owner input",
                                        "parent": null,
                                        "createdAt": "2026-06-10T00:05:00Z"
                                    }
                                ]
                            },
                            "relations": { "nodes": [] },
                            "createdAt": "2026-06-10T00:00:00Z",
                            "updatedAt": "2026-06-10T00:03:00Z"
                        }
                    ]
                }
            }
        }),
        serde_json::json!({
            "data": {
                "issue": {
                    "team": {
                        "states": {
                            "nodes": [
                                { "id": "state-in-progress", "name": "In Progress" }
                            ]
                        }
                    }
                }
            }
        }),
        serde_json::json!({ "data": { "issueUpdate": { "success": true } } }),
        serde_json::json!({ "data": { "commentCreate": { "success": true } } }),
    ]);
    let client = LinearGraphqlClient::new(
        "https://linear.example/graphql",
        "linear-token",
        transport.clone(),
    );

    let issues = client
        .fetch_candidate_issues(project)
        .await
        .expect("issues");
    client
        .transition_issue("issue-1", LinearTransition::InProgress)
        .await
        .expect("transition");
    client
        .record_issue_evidence(
            "issue-1",
            LinearIssueEvidence {
                kind: "cutover_smoke".into(),
                body: "live evidence".into(),
            },
        )
        .await
        .expect("evidence");

    assert_eq!(issues.len(), 2);
    assert_eq!(issues[0].identifier, "SYM-100");
    assert_eq!(issues[0].state, "Todo");
    assert_eq!(
        issues[0]
            .project_milestone
            .as_ref()
            .map(|milestone| milestone.id.as_str()),
        Some("milestone-from-linear")
    );
    assert_eq!(issues[0].labels, vec!["vnext"]);
    assert_eq!(
        issues[0].blocked_by[0].identifier.as_deref(),
        Some("SYM-99")
    );
    assert!(!issues[0].has_new_owner_answer);
    assert_eq!(issues[1].identifier, "SYM-101");
    assert!(issues[1].has_new_owner_answer);
    assert_eq!(
        issues[1].owner_answer_created_at.as_deref(),
        Some("2026-06-10T00:03:00Z")
    );

    let requests = transport.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0]["variables"]["teamKey"], "SYM");
    assert_eq!(
        requests[0]["variables"]["projectId"],
        "07df87ce-4e93-4d2c-a73d-84aee1f27e07"
    );
    assert!(requests[0]["variables"].get("projectMilestoneId").is_none());
    assert!(
        requests[0]["query"]
            .as_str()
            .expect("candidate query")
            .contains("projectMilestone { id name }")
    );
    let states = requests[0]["variables"]["states"]
        .as_array()
        .expect("states");
    assert_eq!(
        states,
        &[
            serde_json::json!("Backlog"),
            serde_json::json!("Todo"),
            serde_json::json!("In Progress"),
            serde_json::json!("Need Owner Input"),
            serde_json::json!("Done"),
            serde_json::json!("Canceled"),
        ]
    );
    assert!(
        requests[0]["query"]
            .as_str()
            .expect("candidate query")
            .contains("comments(last: 50, orderBy: createdAt)")
    );
    assert_eq!(requests[2]["variables"]["stateId"], "state-in-progress");
    assert!(
        requests[3]["variables"]["body"]
            .as_str()
            .unwrap()
            .contains("cutover_smoke")
    );
}

#[tokio::test]
async fn linear_graphql_client_paginates_candidate_issues_until_exhausted() {
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let project = config.project("symphony").expect("project");
    let transport = RecordingGraphqlTransport::new(vec![
        serde_json::json!({
            "data": {
                "issues": {
                    "nodes": [linear_issue_node_json("issue-1", "SYM-100", "Todo", 1)],
                    "pageInfo": {
                        "hasNextPage": true,
                        "endCursor": "cursor-1"
                    }
                }
            }
        }),
        serde_json::json!({
            "data": {
                "issues": {
                    "nodes": [linear_issue_node_json("issue-2", "SYM-101", "Todo", 2)],
                    "pageInfo": {
                        "hasNextPage": false,
                        "endCursor": null
                    }
                }
            }
        }),
    ]);
    let client = LinearGraphqlClient::new(
        "https://linear.example/graphql",
        "linear-token",
        transport.clone(),
    );

    let issues = client
        .fetch_candidate_issues(project)
        .await
        .expect("issues");

    assert_eq!(
        issues
            .iter()
            .map(|issue| issue.identifier.as_str())
            .collect::<Vec<_>>(),
        vec!["SYM-100", "SYM-101"]
    );
    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0]["variables"]["after"].is_null());
    assert_eq!(requests[1]["variables"]["after"], "cursor-1");
}

#[tokio::test]
async fn sqlite_migrations_initialize_empty_runtime_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");

    store.migrate().await.expect("migrate");

    assert_eq!(
        store.applied_migrations().await.expect("migrations"),
        vec!["001_runtime_state"]
    );
    assert_eq!(
        store.projects().await.expect("empty projects"),
        Vec::<ProjectStateRecord>::new()
    );
}

#[tokio::test]
async fn runtime_state_persists_and_reloads_by_project_issue_and_session() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");

    {
        let store = SqliteStore::open(&db_path).await.expect("open sqlite");
        store.migrate().await.expect("migrate");
        store
            .upsert_project(ProjectStateRecord {
                project_id: "symphony".into(),
                name: "Symphony".into(),
                enabled: true,
                lifecycle_stage: LifecycleStage::Running,
                cleanup_status: CleanupStatus::Clean,
            })
            .await
            .expect("project");
        store
            .upsert_issue(IssueStateRecord {
                project_id: "symphony".into(),
                issue_id: "0af8ad67-37b9-412a-9869-82ca96b418e1".into(),
                identifier: "SYM-25".into(),
                title: "Bootstrap Rust service, project registry, and SQLite state store".into(),
                state: "In Progress".into(),
                lifecycle_stage: LifecycleStage::Running,
                blocker: None,
                failure: Some(FailureRecord {
                    kind: "validation".into(),
                    message: "last run pending".into(),
                    fingerprint: None,
                    occurrence_count: 1,
                }),
                git_ref: Some(GitRefRecord {
                    branch: "agent-server/opencode-runner-extension".into(),
                    worktree_path: "/home/agent/.symphony/workspaces/opencode/symphony/SYM-25"
                        .into(),
                    head_sha: None,
                    pr_url: None,
                }),
                cleanup_status: CleanupStatus::Pending,
            })
            .await
            .expect("issue");
        store
            .upsert_opencode_session(OpenCodeSessionRecord {
                project_id: "symphony".into(),
                issue_id: "0af8ad67-37b9-412a-9869-82ca96b418e1".into(),
                session_id: "oc-session-1".into(),
                agent: "build".into(),
                model: None,
                worktree_path: "/home/agent/.symphony/workspaces/opencode/symphony/SYM-25".into(),
                lifecycle_stage: LifecycleStage::Running,
                stage: OpenCodeStage::Running,
                active_agent: Some("rust-engineer".into()),
                active_model: Some("claude-sonnet-4".into()),
                message_count: 2,
                todo_count: 1,
                part_count: 3,
                token_count: 1440,
                cost_micros: 250_000,
                subagent_count: 1,
                eval_stage: Some("unit".into()),
                lifecycle_marker: Some("implementation".into()),
                last_event: Some("started".into()),
                silence_observed: false,
            })
            .await
            .expect("session");
    }

    let reloaded = SqliteStore::open(&db_path).await.expect("reopen sqlite");
    reloaded.migrate().await.expect("migrate idempotently");

    let project = reloaded
        .project("symphony")
        .await
        .expect("query project")
        .expect("project row");
    assert_eq!(project.lifecycle_stage, LifecycleStage::Running);

    let issues = reloaded
        .issues_for_project("symphony")
        .await
        .expect("query project issues");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].identifier, "SYM-25");
    assert_eq!(
        issues[0].failure.as_ref().expect("failure").kind,
        "validation"
    );

    let issue = reloaded
        .issue("symphony", "0af8ad67-37b9-412a-9869-82ca96b418e1")
        .await
        .expect("query issue")
        .expect("issue row");
    assert_eq!(issue.cleanup_status, CleanupStatus::Pending);

    let session = reloaded
        .opencode_session(
            "symphony",
            "0af8ad67-37b9-412a-9869-82ca96b418e1",
            "oc-session-1",
        )
        .await
        .expect("query session")
        .expect("session row");
    assert_eq!(session.agent, "build");
    assert_eq!(session.stage, OpenCodeStage::Running);
    assert_eq!(
        session.worktree_path,
        "/home/agent/.symphony/workspaces/opencode/symphony/SYM-25"
    );
    assert_eq!(session.active_agent.as_deref(), Some("rust-engineer"));
    assert_eq!(session.token_count, 1440);

    let read_model = RuntimeReadModel::from_store(&reloaded)
        .await
        .expect("read model");
    assert_eq!(read_model.projects[0].project_id, "symphony");
    assert_eq!(
        read_model.projects[0].issues[0].opencode_sessions[0].session_id,
        "oc-session-1"
    );
    assert_eq!(
        read_model.projects[0].issues[0].opencode_sessions[0]
            .eval_stage
            .as_deref(),
        Some("unit")
    );
}

#[tokio::test]
async fn runtime_cleanup_removes_stale_completed_rows_and_keeps_active_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store
        .upsert_project(ProjectStateRecord {
            project_id: "symphony".into(),
            name: "Symphony".into(),
            enabled: true,
            lifecycle_stage: LifecycleStage::Running,
            cleanup_status: CleanupStatus::Clean,
        })
        .await
        .expect("project");

    let mut completed = test_issue("symphony", "done", "SYM-1", "Done");
    completed.lifecycle_stage = LifecycleStage::Completed;
    completed.cleanup_status = CleanupStatus::Complete;
    store
        .upsert_issue(completed)
        .await
        .expect("completed issue");
    let mut completed_session = test_session(
        "symphony",
        "done",
        "session-done",
        "/home/agent/.symphony/workspaces/opencode/symphony/SYM-1",
    );
    completed_session.lifecycle_stage = LifecycleStage::Completed;
    completed_session.stage = OpenCodeStage::Completed;
    store
        .upsert_opencode_session(completed_session)
        .await
        .expect("completed session");
    store
        .upsert_opencode_stage_event(OpenCodeStageEventRecord {
            project_id: "symphony".into(),
            issue_id: "done".into(),
            session_id: "session-done".into(),
            sequence: 1,
            stage: OpenCodeStage::Completed,
            event: Some("handoff accepted".into()),
        })
        .await
        .expect("completed event");
    store
        .upsert_eval_run(EvalRunRecord {
            project_id: "symphony".into(),
            issue_id: "done".into(),
            run_id: "eval-done".into(),
            suite: "cargo test".into(),
            status: "passed".into(),
            details_json: None,
        })
        .await
        .expect("completed eval");

    let running = test_issue("symphony", "running", "SYM-2", "In Progress");
    store.upsert_issue(running).await.expect("running issue");
    store
        .upsert_opencode_session(test_session(
            "symphony",
            "running",
            "session-running",
            "/home/agent/.symphony/workspaces/opencode/symphony/SYM-2",
        ))
        .await
        .expect("running session");

    let mut blocked = test_issue("symphony", "blocked", "SYM-3", "Need Owner Input");
    blocked.lifecycle_stage = LifecycleStage::Blocked;
    store.upsert_issue(blocked).await.expect("blocked issue");

    let report = store
        .cleanup_runtime_state(Duration::from_secs(0))
        .await
        .expect("cleanup");

    assert_eq!(report.eval_runs_deleted, 1);
    assert_eq!(report.stage_events_deleted, 1);
    assert_eq!(report.sessions_deleted, 1);
    assert_eq!(report.issues_deleted, 1);
    assert!(
        store
            .issue("symphony", "done")
            .await
            .expect("done")
            .is_none()
    );
    assert!(
        store
            .issue("symphony", "running")
            .await
            .expect("running")
            .is_some()
    );
    assert!(
        store
            .opencode_session("symphony", "running", "session-running")
            .await
            .expect("running session")
            .is_some()
    );
    assert!(
        store
            .issue("symphony", "blocked")
            .await
            .expect("blocked")
            .is_some()
    );
}

#[tokio::test]
async fn dashboard_api_snapshots_aggregate_project_drilldown_and_issue_detail() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");

    let mut repair = test_issue("symphony", "repair", "SYM-91", "In Progress");
    repair.failure = Some(FailureRecord {
        kind: "eval_failure".into(),
        message: "clippy::needless_collect".into(),
        fingerprint: Some("clippy-needless-collect".into()),
        occurrence_count: 1,
    });
    repair.git_ref = Some(GitRefRecord {
        branch: "agent-server/opencode-runner-extension".into(),
        worktree_path: "/home/agent/.symphony/workspaces/opencode/symphony/SYM-91".into(),
        head_sha: Some("abc123".into()),
        pr_url: Some("https://example.test/pr/91".into()),
    });
    store.upsert_issue(repair).await.expect("repair issue");

    let mut provider_blocked = test_issue("symphony", "provider", "SYM-92", "Need Owner Input");
    provider_blocked.lifecycle_stage = LifecycleStage::Blocked;
    provider_blocked.blocker = Some(BlockerRecord {
        kind: "provider_blocker".into(),
        message: "provider quota exhausted".into(),
        observed_at: None,
    });
    store
        .upsert_issue(provider_blocked)
        .await
        .expect("provider issue");

    let mut completed = test_issue("symphony", "done", "SYM-93", "Done");
    completed.lifecycle_stage = LifecycleStage::Completed;
    completed.cleanup_status = CleanupStatus::Complete;
    store.upsert_issue(completed).await.expect("done issue");

    let mut session = test_session(
        "symphony",
        "repair",
        "oc-repair",
        "/home/agent/.symphony/workspaces/opencode/symphony/SYM-91",
    );
    session.stage = OpenCodeStage::Eval;
    session.active_agent = Some("evaluator".into());
    session.active_model = Some("gpt-5".into());
    session.token_count = 4096;
    session.cost_micros = 123_456;
    session.subagent_count = 2;
    session.eval_stage = Some("cargo clippy".into());
    session.lifecycle_marker = Some("repair_loop".into());
    session.last_event = Some("eval_failed:clippy-needless-collect".into());
    store
        .upsert_opencode_session(session)
        .await
        .expect("session");
    store
        .upsert_opencode_stage_event(OpenCodeStageEventRecord {
            project_id: "symphony".into(),
            issue_id: "repair".into(),
            session_id: "oc-repair".into(),
            sequence: 1,
            stage: OpenCodeStage::Running,
            event: Some("implementation_started".into()),
        })
        .await
        .expect("running stage event");
    store
        .upsert_opencode_stage_event(OpenCodeStageEventRecord {
            project_id: "symphony".into(),
            issue_id: "repair".into(),
            session_id: "oc-repair".into(),
            sequence: 2,
            stage: OpenCodeStage::Eval,
            event: Some("eval_failed:clippy-needless-collect".into()),
        })
        .await
        .expect("eval stage event");
    store
        .upsert_eval_run(EvalRunRecord {
            project_id: "symphony".into(),
            issue_id: "repair".into(),
            run_id: "eval-1".into(),
            suite: "cargo clippy".into(),
            status: "failed".into(),
            details_json: Some(r#"{"fingerprint":"clippy-needless-collect"}"#.into()),
        })
        .await
        .expect("eval run");

    let api = RuntimeDashboardApi::from_store(&config, &store)
        .await
        .expect("dashboard api");
    let aggregate_json = serde_json::to_string_pretty(&api.aggregate()).expect("aggregate json");
    let project_json = serde_json::to_string_pretty(
        &api.project_drilldown("symphony")
            .expect("project endpoint")
            .expect("project exists"),
    )
    .expect("project json");
    let issue_detail = api
        .issue_detail("symphony", "repair")
        .expect("issue endpoint")
        .expect("issue exists");
    let issue_json = serde_json::to_string_pretty(issue_detail).expect("issue json");

    assert_eq!(
        aggregate_json,
        r#"{
  "projects": [
    {
      "project_id": "symphony",
      "name": "Symphony",
      "enabled": true,
      "active_count": 1,
      "parked_count": 1,
      "terminal_count": 1,
      "runner_health": "repair loop",
      "last_event": "eval_failed:clippy-needless-collect",
      "capacity": {
        "max_sessions": 2,
        "running_sessions": 1,
        "available_sessions": 1
      },
      "cleanup_status": "clean"
    }
  ]
}"#
    );
    assert!(project_json.contains(r#""display_status": "provider/infra blocker""#));
    assert!(project_json.contains(r#""history_issues""#));
    assert!(issue_json.contains(r#""opencode_session_id": "oc-repair""#));
    assert_eq!(
        issue_detail.opencode_sessions[0].stage_history,
        vec![OpenCodeStage::Running, OpenCodeStage::Eval]
    );
    assert!(issue_json.contains(r#""subagents_used": 2"#));
    assert!(issue_json.contains(r#""eval_results""#));
    assert!(issue_json.contains(r#""pr_url": "https://example.test/pr/91""#));
}

#[tokio::test]
async fn dashboard_api_json_routes_aggregate_project_and_issue_paths() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let issue = test_issue("symphony", "api-issue", "SYM-94", "In Progress");
    store.upsert_issue(issue).await.expect("issue");

    let aggregate =
        symphony_vnext::api::runtime_api_json_response(&config, &store, "/api/dashboard")
            .await
            .expect("aggregate response");
    let project =
        symphony_vnext::api::runtime_api_json_response(&config, &store, "/api/projects/symphony")
            .await
            .expect("project response");
    let issue = symphony_vnext::api::runtime_api_json_response(
        &config,
        &store,
        "/api/projects/symphony/issues/api-issue",
    )
    .await
    .expect("issue response");
    let missing =
        symphony_vnext::api::runtime_api_json_response(&config, &store, "/api/projects/missing")
            .await
            .expect("missing response");

    assert_eq!(aggregate.status, 200);
    assert_eq!(project.status, 200);
    assert_eq!(issue.status, 200);
    assert_eq!(missing.status, 404);
    assert!(aggregate.body.contains(r#""project_id":"symphony""#));
    assert!(project.body.contains(r#""active_issues""#));
    assert!(issue.body.contains(r#""identifier":"SYM-94""#));
}
