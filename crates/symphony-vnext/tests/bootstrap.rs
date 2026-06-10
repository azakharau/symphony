use std::{
    fs,
    io::{BufRead, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use symphony_vnext::{
    api::{RuntimeDashboardApi, RuntimeReadModel},
    cli,
    config::RootConfig,
    daemon,
    linear::{
        LinearBlocker, LinearClient, LinearClientError, LinearGraphqlClient,
        LinearGraphqlTransport, LinearIssue, LinearIssueEvidence, LinearTransition,
    },
    opencode::{
        self, GitClosureEvidence, OpenCodeEvalResult, OpenCodeHandoff, OpenCodeLauncher,
        OpenCodeSessionEvent, OpenCodeStopReason, PermissionPolicy,
    },
    state::{
        BlockerRecord, CleanupStatus, EvalRunRecord, FailureRecord, GitRefRecord, IssueStateRecord,
        LifecycleStage, OpenCodeSessionRecord, OpenCodeStage, OpenCodeStageEventRecord,
        ProjectStateRecord,
    },
    storage::SqliteStore,
};

fn valid_config_yaml() -> &'static str {
    r#"
server:
  host: 127.0.0.1
  port: 4110
projects:
  - id: symphony
    name: Symphony
    enabled: true
    workflow_path: /home/agent/proj/symphony/WORKFLOW.md
    repo_path: /home/agent/proj/symphony
    branch:
      base: agent-server/opencode-runner-extension
      worktree_root: /home/agent/.symphony/workspaces/opencode/symphony
    linear:
      team_key: SYM
      project_id: 07df87ce-4e93-4d2c-a73d-84aee1f27e07
      project_milestone_id: 7a04f8cf-dece-48b9-a2ec-0356ed639943
    opencode:
      command: /usr/local/bin/opencode
      args: ["acp"]
      agent: build
      model: openai/gpt-5.5
      effort: high
      permission_policy: reject
    eval:
      default_suite: symphony-vnext-smoke
    concurrency:
      max_sessions: 2
"#
}

#[tokio::test]
async fn multiproject_config_loads_deterministically_and_validates_required_fields() {
    let first = RootConfig::from_yaml_str(valid_config_yaml()).expect("valid root config");
    let second = RootConfig::from_yaml_str(valid_config_yaml()).expect("valid root config");

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

    let missing_required =
        valid_config_yaml().replace("    repo_path: /home/agent/proj/symphony\n", "");
    let err = RootConfig::from_yaml_str(&missing_required).expect_err("repo_path is required");
    assert!(err.to_string().contains("repo_path"), "{err}");
}

#[tokio::test]
async fn config_rejects_codex_compatibility_fields() {
    let with_codex = valid_config_yaml().replace(
        "    opencode:\n",
        "    codex:\n      command: codex\n    opencode:\n",
    );

    let err =
        RootConfig::from_yaml_str(&with_codex).expect_err("codex config must not be accepted");
    assert!(err.to_string().contains("codex"), "{err}");
}

#[tokio::test]
async fn linear_graphql_client_fetches_project_candidates_transitions_and_records_evidence() {
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
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
                            "labels": { "nodes": [{ "name": "vnext" }] },
                            "comments": {
                                "nodes": [
                                    {
                                        "body": "Codex repair handoff for SYM-100",
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
    assert_eq!(
        requests[0]["variables"]["projectMilestoneId"],
        "7a04f8cf-dece-48b9-a2ec-0356ed639943"
    );
    assert!(
        requests[0]["variables"]["states"]
            .as_array()
            .expect("states")
            .contains(&serde_json::json!("Preparing"))
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
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
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
                    worktree_path: "/home/agent/.symphony/workspaces/codex/symphony/SYM-25".into(),
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
async fn dashboard_api_snapshots_aggregate_project_drilldown_and_issue_detail() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
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
    assert!(
        !aggregate_json.contains("Preparing")
            && !aggregate_json.contains("In Review")
            && !aggregate_json.contains("RCA Required")
            && !aggregate_json.contains("Codex")
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
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
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

#[tokio::test]
async fn opencode_acp_launch_spec_uses_stdio_command_isolated_worktree_and_full_issue_prompt() {
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
    let project = config.project("symphony").expect("project");
    let issue = linear_issue("issue-27", "SYM-27", "Todo", Some(1))
        .with_description("Implement the OpenCode ACP lifecycle runner with stage telemetry.");

    let spec = opencode::build_acp_launch_spec(project, &issue);

    assert_eq!(spec.command, PathBuf::from("/usr/local/bin/opencode"));
    assert_eq!(spec.args, vec!["acp"]);
    assert_eq!(
        spec.cwd,
        PathBuf::from("/home/agent/.symphony/workspaces/opencode/symphony/SYM-27")
    );
    assert!(spec.prompt.contains("SYM-27"), "{}", spec.prompt);
    assert!(
        spec.prompt.contains("symphony-vnext-smoke"),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt
            .contains("Implement the OpenCode ACP lifecycle runner"),
        "{}",
        spec.prompt
    );
}

#[tokio::test]
async fn stdio_launcher_uses_acp_json_rpc_session_lifecycle() {
    let dir = tempfile::tempdir().expect("tempdir");
    let transcript_path = dir.path().join("acp-transcript.jsonl");
    let script_path = write_fake_acp_script(dir.path(), &transcript_path);
    let worktree = dir.path().join("worktree");
    let spec = opencode::OpenCodeLaunchSpec {
        command: script_path,
        args: Vec::new(),
        cwd: worktree.clone(),
        worktree_root: None,
        issue_identifier: "SYM-200".into(),
        repo_path: None,
        base_ref: None,
        agent: "build".into(),
        model: Some("openai/gpt-5.5".into()),
        effort: Some("high".into()),
        prompt: "Full Linear issue spec with eval defaults".into(),
        permission_policy: PermissionPolicy::Reject,
    };
    let launcher = opencode::StdioOpenCodeLauncher;

    let started = launcher.launch(&spec).await.expect("launch stdio child");

    assert_eq!(started.session_id, "ses-test");
    for _ in 0..50 {
        if let Ok(transcript) = fs::read_to_string(&transcript_path)
            && transcript.contains(r#""method": "session/prompt""#)
        {
            assert!(
                transcript.contains(r#""method": "initialize""#),
                "{transcript}"
            );
            assert!(
                transcript.contains(r#""method": "session/new""#),
                "{transcript}"
            );
            assert!(
                transcript.contains(r#""method": "session/set_config_option""#),
                "{transcript}"
            );
            assert!(
                transcript.contains(r#""configId": "mode""#)
                    && transcript.contains(r#""value": "build""#),
                "{transcript}"
            );
            assert!(
                transcript.contains(r#""configId": "model""#)
                    && transcript.contains(r#""value": "openai/gpt-5.5""#),
                "{transcript}"
            );
            assert!(
                transcript.contains(r#""configId": "effort""#)
                    && transcript.contains(r#""value": "high""#),
                "{transcript}"
            );
            assert!(
                transcript.find(r#""configId": "effort""#)
                    < transcript.find(r#""method": "session/prompt""#),
                "{transcript}"
            );
            assert!(
                transcript.contains("Full Linear issue spec"),
                "{transcript}"
            );
            assert!(
                transcript.contains("OpenCode ACP session id: ses-test"),
                "{transcript}"
            );

            let session = test_session("symphony", "issue-27", "ses-test", &worktree);
            let handoff = launcher
                .latest_handoff(&session)
                .await
                .expect("handoff read")
                .expect("fake acp handoff");
            assert_eq!(handoff.session_id, "ses-test");
            assert_eq!(handoff.stop_reason, OpenCodeStopReason::Success);
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }

    panic!(
        "ACP JSON-RPC lifecycle was not observed; transcript={:?}",
        fs::read_to_string(transcript_path)
    );
}

#[tokio::test]
async fn stdio_launcher_removes_stale_handoff_before_prompting_new_session() {
    let dir = tempfile::tempdir().expect("tempdir");
    let transcript_path = dir.path().join("acp-transcript.jsonl");
    let script_path = write_fake_acp_script_without_handoff(dir.path(), &transcript_path);
    let worktree_root = dir.path().join("worktrees");
    let worktree = worktree_root.join("SYM-201");
    let sidecar_dir = worktree.join(".symphony");
    let sidecar_path = sidecar_dir.join("opencode-handoff.json");
    fs::create_dir_all(&sidecar_dir).expect("sidecar dir");
    fs::write(
        &sidecar_path,
        r#"{"session_id":"stale-session","stop_reason":{"type":"success"}}"#,
    )
    .expect("stale handoff");
    let spec = opencode::OpenCodeLaunchSpec {
        command: script_path,
        args: Vec::new(),
        cwd: worktree,
        worktree_root: Some(worktree_root),
        issue_identifier: "SYM-201".into(),
        repo_path: None,
        base_ref: None,
        agent: "build".into(),
        model: Some("openai/gpt-5.5".into()),
        effort: Some("high".into()),
        prompt: "Full Linear issue spec with eval defaults".into(),
        permission_policy: PermissionPolicy::Reject,
    };
    let launcher = opencode::StdioOpenCodeLauncher;

    let started = launcher.launch(&spec).await.expect("launch stdio child");

    assert_eq!(started.session_id, "ses-test");
    assert!(
        !sidecar_path.exists(),
        "stale handoff must not survive a new ACP launch"
    );
    for _ in 0..50 {
        if let Ok(transcript) = fs::read_to_string(&transcript_path)
            && transcript.contains(r#""method": "session/prompt""#)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    panic!(
        "ACP prompt was not observed after stale handoff cleanup; transcript={:?}",
        fs::read_to_string(transcript_path)
    );
}

#[tokio::test]
async fn installed_opencode_acp_supports_ndjson_config_options_without_prompting() {
    if std::env::var("SYMPHONY_VNEXT_LIVE_OPENCODE_ACP")
        .ok()
        .as_deref()
        != Some("1")
    {
        eprintln!("set SYMPHONY_VNEXT_LIVE_OPENCODE_ACP=1 to run installed OpenCode ACP smoke");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let mut child = Command::new("/usr/local/bin/opencode")
        .args(["acp", "--pure", "--cwd"])
        .arg(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn installed opencode acp");
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut stdout = std::io::BufReader::new(stdout);

    let initialized = acp_test_request(
        &mut stdin,
        &mut stdout,
        1,
        "initialize",
        serde_json::json!({
            "protocolVersion": 1,
            "clientInfo": {"name": "symphony-vnext-test", "version": "0"},
            "capabilities": {}
        }),
    );
    assert_eq!(initialized["protocolVersion"], serde_json::json!(1));

    let created = acp_test_request(
        &mut stdin,
        &mut stdout,
        2,
        "session/new",
        serde_json::json!({
            "cwd": dir.path(),
            "mcpServers": [],
            "title": "Symphony vNext ACP contract smoke"
        }),
    );
    let session_id = created["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();
    assert!(
        created["configOptions"]
            .as_array()
            .expect("config options")
            .iter()
            .any(|option| option["id"] == "model"),
        "{created}"
    );

    let mode = acp_test_request(
        &mut stdin,
        &mut stdout,
        3,
        "session/set_config_option",
        serde_json::json!({
            "sessionId": session_id,
            "configId": "mode",
            "value": "build"
        }),
    );
    assert_config_option_value(&mode, "mode", "build");

    let model = acp_test_request(
        &mut stdin,
        &mut stdout,
        4,
        "session/set_config_option",
        serde_json::json!({
            "sessionId": session_id,
            "configId": "model",
            "value": "openai/gpt-5.5"
        }),
    );
    assert_config_option_value(&model, "model", "openai/gpt-5.5");

    let effort = acp_test_request(
        &mut stdin,
        &mut stdout,
        5,
        "session/set_config_option",
        serde_json::json!({
            "sessionId": session_id,
            "configId": "effort",
            "value": "high"
        }),
    );
    assert_config_option_value(&effort, "effort", "high");

    let _ = child.kill();
    let _ = child.wait();
}

#[tokio::test]
async fn stdio_launcher_creates_git_worktree_from_project_repo_and_base_ref() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    run_git(&repo, ["init"]);
    run_git(&repo, ["config", "user.email", "symphony@example.test"]);
    run_git(&repo, ["config", "user.name", "Symphony Test"]);
    fs::write(repo.join("README.md"), "base checkout").expect("readme");
    run_git(&repo, ["add", "README.md"]);
    run_git(&repo, ["commit", "-m", "base"]);
    run_git(&repo, ["branch", "agent-server/opencode-runner-extension"]);

    let worktree = dir.path().join("worktrees").join("SYM-200");
    let transcript_path = dir.path().join("acp-transcript.jsonl");
    let script_path = write_fake_acp_script(dir.path(), &transcript_path);
    let spec = opencode::OpenCodeLaunchSpec {
        command: script_path,
        args: Vec::new(),
        cwd: worktree.clone(),
        worktree_root: Some(dir.path().join("worktrees")),
        issue_identifier: "SYM-200".into(),
        repo_path: Some(repo.clone()),
        base_ref: Some("agent-server/opencode-runner-extension".into()),
        agent: "build".into(),
        model: Some("openai/gpt-5.5".into()),
        effort: Some("high".into()),
        prompt: "Full Linear issue spec with eval defaults".into(),
        permission_policy: PermissionPolicy::Reject,
    };
    let launcher = opencode::StdioOpenCodeLauncher;

    let started = launcher.launch(&spec).await.expect("launch stdio child");

    assert_eq!(started.session_id, "ses-test");
    for _ in 0..50 {
        if let Ok(transcript) = fs::read_to_string(&transcript_path)
            && transcript.contains(r#""method": "session/prompt""#)
        {
            assert_eq!(
                git_output(&worktree, ["rev-parse", "--is-inside-work-tree"]).trim(),
                "true"
            );
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }

    panic!(
        "ACP prompt was not sent from a git worktree; transcript={:?}",
        fs::read_to_string(transcript_path)
    );
}

#[tokio::test]
async fn stdio_launcher_rejects_issue_identifier_path_separators_before_worktree_creation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("worktrees");
    let nested = root.join("SYM").join("200");
    let spec = opencode::OpenCodeLaunchSpec {
        command: PathBuf::from("/bin/false"),
        args: Vec::new(),
        cwd: nested.clone(),
        worktree_root: Some(root.clone()),
        issue_identifier: "SYM/200".into(),
        repo_path: Some(dir.path().join("repo")),
        base_ref: Some("main".into()),
        agent: "build".into(),
        model: Some("openai/gpt-5.5".into()),
        effort: Some("high".into()),
        prompt: "Full Linear issue spec with eval defaults".into(),
        permission_policy: PermissionPolicy::Reject,
    };
    let launcher = opencode::StdioOpenCodeLauncher;

    let error = launcher
        .launch(&spec)
        .await
        .expect_err("unsafe identifier must be rejected before spawn");

    assert!(
        matches!(error, opencode::OpenCodeError::InvalidWorktree(_)),
        "{error:?}"
    );
    assert!(
        !nested.exists(),
        "unsafe nested worktree must not be created"
    );
}

#[tokio::test]
async fn opencode_event_ingestion_updates_stage_telemetry_without_losing_session_linkage() {
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
    let project = config.project("symphony").expect("project");
    let issue = linear_issue("issue-27", "SYM-27", "In Progress", Some(1));
    let spec = opencode::build_acp_launch_spec(project, &issue);
    let mut session = opencode::new_session_record(
        project,
        &issue,
        opencode::OpenCodeStartedSession {
            session_id: "oc-session-27".into(),
        },
        &spec,
    );

    opencode::ingest_session_event(
        &mut session,
        OpenCodeSessionEvent {
            stage: Some(OpenCodeStage::Eval),
            active_agent: Some("evaluator".into()),
            active_model: Some("gpt-5".into()),
            message_delta: 2,
            todo_delta: 1,
            part_delta: 4,
            token_delta: 2048,
            cost_micros_delta: 325_000,
            subagent_delta: 1,
            eval_stage: Some("targeted-tests".into()),
            lifecycle_marker: Some("eval".into()),
            last_event: Some("tests_started".into()),
        },
    );

    assert_eq!(session.session_id, "oc-session-27");
    assert_eq!(session.stage, OpenCodeStage::Eval);
    assert_eq!(session.active_agent.as_deref(), Some("evaluator"));
    assert_eq!(session.active_model.as_deref(), Some("gpt-5"));
    assert_eq!(session.message_count, 2);
    assert_eq!(session.todo_count, 1);
    assert_eq!(session.part_count, 4);
    assert_eq!(session.token_count, 2048);
    assert_eq!(session.cost_micros, 325_000);
    assert_eq!(session.subagent_count, 1);
    assert_eq!(session.eval_stage.as_deref(), Some("targeted-tests"));
    assert_eq!(session.lifecycle_marker.as_deref(), Some("eval"));
    assert_eq!(session.last_event.as_deref(), Some("tests_started"));
}

#[tokio::test]
async fn opencode_silence_is_observable_without_marking_session_failed() {
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
    let project = config.project("symphony").expect("project");
    let issue = linear_issue("issue-27", "SYM-27", "In Progress", Some(1));
    let spec = opencode::build_acp_launch_spec(project, &issue);
    let mut session = opencode::new_session_record(
        project,
        &issue,
        opencode::OpenCodeStartedSession {
            session_id: "oc-session-27".into(),
        },
        &spec,
    );

    opencode::mark_session_silence(&mut session, "read_timeout_without_acp_event");

    assert_eq!(session.lifecycle_stage, LifecycleStage::Running);
    assert_eq!(session.stage, OpenCodeStage::Silent);
    assert_eq!(
        session.last_event.as_deref(),
        Some("silence:read_timeout_without_acp_event")
    );
    assert!(session.silence_observed);
    assert!(session.failure_marker().is_none());
}

#[tokio::test]
async fn daemon_once_entrypoint_validates_config_migrates_and_reconciles_projects() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("projects.yml");
    let db_path = dir.path().join("runtime.sqlite3");
    fs::write(&config_path, valid_config_yaml()).expect("write config");

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
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
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
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
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
async fn orchestration_reconciles_persisted_backlog_without_counting_capacity() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
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
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
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
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
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
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
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
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
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
async fn terminal_reconciliation_marks_cleanup_complete_when_worktree_is_already_absent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let missing_worktree = dir.path().join("already-removed").join("SYM-63");
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
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
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
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

fn linear_issue(
    id: impl Into<String>,
    identifier: impl Into<String>,
    state: impl Into<String>,
    priority: Option<i64>,
) -> LinearIssue {
    LinearIssue {
        id: id.into(),
        identifier: identifier.into(),
        title: "Test issue".into(),
        description: None,
        state: state.into(),
        priority,
        branch_name: None,
        url: None,
        labels: Vec::new(),
        blocked_by: Vec::new(),
        has_new_owner_answer: false,
        owner_answer_created_at: None,
        created_at: None,
        updated_at: None,
    }
}

fn test_issue(
    project_id: impl Into<String>,
    issue_id: impl Into<String>,
    identifier: impl Into<String>,
    state: impl Into<String>,
) -> IssueStateRecord {
    IssueStateRecord {
        project_id: project_id.into(),
        issue_id: issue_id.into(),
        identifier: identifier.into(),
        title: "Test issue".into(),
        state: state.into(),
        lifecycle_stage: LifecycleStage::Running,
        blocker: None,
        failure: None,
        git_ref: None,
        cleanup_status: CleanupStatus::Clean,
    }
}

fn test_session(
    project_id: impl Into<String>,
    issue_id: impl Into<String>,
    session_id: impl Into<String>,
    worktree_path: impl AsRef<std::path::Path>,
) -> OpenCodeSessionRecord {
    OpenCodeSessionRecord {
        project_id: project_id.into(),
        issue_id: issue_id.into(),
        session_id: session_id.into(),
        agent: "build".into(),
        model: None,
        worktree_path: worktree_path.as_ref().display().to_string(),
        lifecycle_stage: LifecycleStage::Running,
        stage: OpenCodeStage::Running,
        active_agent: Some("rust-engineer".into()),
        active_model: None,
        message_count: 1,
        todo_count: 1,
        part_count: 1,
        token_count: 100,
        cost_micros: 10,
        subagent_count: 1,
        eval_stage: Some("symphony-vnext-smoke".into()),
        lifecycle_marker: Some("implementation".into()),
        last_event: Some("working".into()),
        silence_observed: false,
    }
}

fn success_handoff(
    session_id: &str,
    worktree_path: impl AsRef<std::path::Path>,
    branch: &str,
    head_sha: &str,
) -> OpenCodeHandoff {
    OpenCodeHandoff {
        session_id: session_id.into(),
        lifecycle_stages: vec![
            OpenCodeStage::Running,
            OpenCodeStage::Eval,
            OpenCodeStage::Handoff,
            OpenCodeStage::Completed,
        ],
        subagents: vec!["rust-engineer".into(), "evaluator".into()],
        eval_results: vec![OpenCodeEvalResult {
            suite: "cargo test".into(),
            passed: true,
            failure_fingerprint: None,
            details: Some("ok".into()),
        }],
        changed_files: vec!["crates/symphony-vnext/src/opencode.rs".into()],
        git: Some(GitClosureEvidence {
            branch: branch.into(),
            head_sha: Some(head_sha.into()),
            pr_url: Some("https://example.test/pr/80".into()),
            worktree_path: worktree_path.as_ref().display().to_string(),
        }),
        risks: Vec::new(),
        stop_reason: OpenCodeStopReason::Success,
    }
}

fn eval_failed_handoff(session_id: &str, fingerprint: &str) -> OpenCodeHandoff {
    OpenCodeHandoff {
        session_id: session_id.into(),
        lifecycle_stages: vec![OpenCodeStage::Running, OpenCodeStage::Eval],
        subagents: vec!["rust-engineer".into(), "evaluator".into()],
        eval_results: vec![OpenCodeEvalResult {
            suite: "cargo clippy".into(),
            passed: false,
            failure_fingerprint: Some(fingerprint.into()),
            details: Some("clippy failed".into()),
        }],
        changed_files: vec!["crates/symphony-vnext/src/daemon.rs".into()],
        git: None,
        risks: vec!["repair pending".into()],
        stop_reason: OpenCodeStopReason::EvalFailed {
            failure_fingerprint: fingerprint.into(),
        },
    }
}

fn run_git<const N: usize>(repo: &std::path::Path, args: [&str; N]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git command");
    assert!(
        output.status.success(),
        "git failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_output<const N: usize>(repo: &std::path::Path, args: [&str; N]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git command");
    assert!(
        output.status.success(),
        "git failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("git stdout utf8")
}

fn write_fake_acp_script(dir: &Path, transcript_path: &Path) -> PathBuf {
    let script_path = dir.join("fake-opencode-acp.py");
    let transcript_literal =
        serde_json::to_string(&transcript_path.display().to_string()).expect("json path");
    fs::write(
        &script_path,
        format!(
            r#"#!/usr/bin/env python3
import json
import pathlib
import sys

transcript_path = pathlib.Path({transcript_literal})
cwd = None
config = {{"mode": "build", "model": "opencode/big-pickle", "effort": "none"}}

def config_options():
    return [
        {{
            "id": "mode",
            "name": "Session Mode",
            "category": "mode",
            "type": "select",
            "currentValue": config["mode"],
            "options": [{{"value": "build", "name": "build"}}],
        }},
        {{
            "id": "model",
            "name": "Model",
            "category": "model",
            "type": "select",
            "currentValue": config["model"],
            "options": [{{"value": "openai/gpt-5.5", "name": "OpenAI/GPT-5.5"}}],
        }},
        {{
            "id": "effort",
            "name": "Effort",
            "category": "thought_level",
            "type": "select",
            "currentValue": config["effort"],
            "options": [{{"value": "high", "name": "High"}}],
        }},
    ]

for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    with transcript_path.open("a", encoding="utf-8") as transcript:
        transcript.write(json.dumps(message, sort_keys=True) + "\n")

    if method == "initialize":
        print(json.dumps({{"jsonrpc": "2.0", "id": message["id"], "result": {{"protocolVersion": 1}}}}), flush=True)
    elif method == "session/new":
        cwd = pathlib.Path(message["params"]["cwd"])
        (cwd / ".symphony").mkdir(parents=True, exist_ok=True)
        print(json.dumps({{"jsonrpc": "2.0", "id": message["id"], "result": {{"sessionId": "ses-test", "configOptions": config_options()}}}}), flush=True)
    elif method == "session/set_config_option":
        config[message["params"]["configId"]] = message["params"]["value"]
        print(json.dumps({{"jsonrpc": "2.0", "id": message["id"], "result": {{"configOptions": config_options()}}}}), flush=True)
    elif method == "session/prompt":
        if config["model"] != "openai/gpt-5.5" or config["effort"] != "high":
            print(json.dumps({{"jsonrpc": "2.0", "id": message["id"], "error": {{"code": -32000, "message": "model/effort was not configured before prompt"}}}}), flush=True)
            break
        handoff = {{
            "session_id": "ses-test",
            "lifecycle_stages": ["starting", "running", "eval", "handoff"],
            "subagents": ["build"],
            "eval_results": [{{"suite": "fake-smoke", "passed": True, "failure_fingerprint": None, "details": "ok"}}],
            "changed_files": ["README.md"],
            "git": {{"branch": "agent-server/opencode-runner-extension", "head_sha": "abc123", "pr_url": None, "worktree_path": str(cwd)}},
            "risks": [],
            "stop_reason": {{"type": "success"}}
        }}
        (cwd / ".symphony" / "opencode-handoff.json").write_text(json.dumps(handoff), encoding="utf-8")
        print(json.dumps({{"jsonrpc": "2.0", "id": message["id"], "result": {{"stopReason": "end_turn"}}}}), flush=True)
        break
    else:
        print(json.dumps({{"jsonrpc": "2.0", "id": message.get("id"), "error": {{"code": -32601, "message": "unknown method"}}}}), flush=True)
"#
        ),
    )
    .expect("fake acp script");
    let mut permissions = fs::metadata(&script_path)
        .expect("fake acp metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("fake acp executable");
    script_path
}

fn write_fake_acp_script_without_handoff(dir: &Path, transcript_path: &Path) -> PathBuf {
    let script_path = dir.join("fake-opencode-acp-no-handoff.py");
    let transcript_literal =
        serde_json::to_string(&transcript_path.display().to_string()).expect("json path");
    fs::write(
        &script_path,
        format!(
            r#"#!/usr/bin/env python3
import json
import pathlib
import sys

transcript_path = pathlib.Path({transcript_literal})

for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    with transcript_path.open("a", encoding="utf-8") as transcript:
        transcript.write(json.dumps(message, sort_keys=True) + "\n")

    if method == "initialize":
        print(json.dumps({{"jsonrpc": "2.0", "id": message["id"], "result": {{"protocolVersion": 1}}}}), flush=True)
    elif method == "session/new":
        print(json.dumps({{"jsonrpc": "2.0", "id": message["id"], "result": {{"sessionId": "ses-test", "configOptions": []}}}}), flush=True)
    elif method == "session/set_config_option":
        print(json.dumps({{"jsonrpc": "2.0", "id": message["id"], "result": {{"configOptions": []}}}}), flush=True)
    elif method == "session/prompt":
        print(json.dumps({{"jsonrpc": "2.0", "id": message["id"], "result": {{"stopReason": "end_turn"}}}}), flush=True)
        break
    else:
        print(json.dumps({{"jsonrpc": "2.0", "id": message.get("id"), "error": {{"code": -32601, "message": "unknown method"}}}}), flush=True)
"#
        ),
    )
    .expect("fake acp script");
    let mut permissions = fs::metadata(&script_path)
        .expect("fake acp metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("fake acp executable");
    script_path
}

fn acp_test_request<R: BufRead, W: Write>(
    stdin: &mut W,
    stdout: &mut R,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    writeln!(
        stdin,
        "{}",
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        })
    )
    .expect("write acp request");
    stdin.flush().expect("flush acp request");

    for _ in 0..200 {
        let mut line = String::new();
        if stdout.read_line(&mut line).expect("read acp response") == 0 {
            panic!("ACP stdout closed before {method} response");
        }
        let message: serde_json::Value = serde_json::from_str(&line).expect("acp json response");
        if message.get("id").and_then(serde_json::Value::as_u64) == Some(id) {
            if let Some(error) = message.get("error") {
                panic!("ACP {method} failed: {error}");
            }
            return message
                .get("result")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
        }
    }

    panic!("ACP response for {method} was not observed");
}

fn assert_config_option_value(response: &serde_json::Value, id: &str, value: &str) {
    assert!(
        response["configOptions"]
            .as_array()
            .expect("config options")
            .iter()
            .any(|option| option["id"] == id && option["currentValue"] == value),
        "{response}"
    );
}

fn linear_issue_node_json(
    id: &str,
    identifier: &str,
    state: &str,
    priority: i64,
) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "identifier": identifier,
        "title": format!("{identifier} title"),
        "description": format!("{identifier} description"),
        "state": { "name": state },
        "priority": priority,
        "branchName": "agent-server/opencode-runner-extension",
        "url": format!("https://linear.example/{identifier}"),
        "labels": { "nodes": [] },
        "comments": { "nodes": [] },
        "relations": { "nodes": [] },
        "createdAt": "2026-06-10T00:00:00Z",
        "updatedAt": "2026-06-10T00:00:00Z"
    })
}

#[derive(Debug)]
struct RecordingLinearClient {
    issues: Vec<LinearIssue>,
    transitions: std::sync::Mutex<Vec<(String, LinearTransition)>>,
    evidence: std::sync::Mutex<Vec<(String, LinearIssueEvidence)>>,
}

impl RecordingLinearClient {
    fn new(issues: Vec<LinearIssue>) -> Self {
        Self {
            issues,
            transitions: std::sync::Mutex::new(Vec::new()),
            evidence: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn transitions(&self) -> Vec<(String, LinearTransition)> {
        self.transitions.lock().expect("transitions lock").clone()
    }

    fn evidence(&self) -> Vec<(String, LinearIssueEvidence)> {
        self.evidence.lock().expect("evidence lock").clone()
    }
}

#[async_trait::async_trait]
impl LinearClient for RecordingLinearClient {
    async fn fetch_candidate_issues(
        &self,
        _project: &symphony_vnext::config::ProjectConfig,
    ) -> Result<Vec<LinearIssue>, LinearClientError> {
        Ok(self.issues.clone())
    }

    async fn transition_issue(
        &self,
        issue_id: &str,
        transition: LinearTransition,
    ) -> Result<(), LinearClientError> {
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
struct ProjectAwareLinearClient {
    issues_by_project: std::collections::HashMap<String, Vec<LinearIssue>>,
    transitions: std::sync::Mutex<Vec<(String, LinearTransition)>>,
}

impl ProjectAwareLinearClient {
    fn new<const N: usize>(issues: [(&str, Vec<LinearIssue>); N]) -> Self {
        Self {
            issues_by_project: issues
                .into_iter()
                .map(|(project_id, issues)| (project_id.to_string(), issues))
                .collect(),
            transitions: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn transitions(&self) -> Vec<(String, LinearTransition)> {
        self.transitions.lock().expect("transitions lock").clone()
    }
}

#[async_trait::async_trait]
impl LinearClient for ProjectAwareLinearClient {
    async fn fetch_candidate_issues(
        &self,
        project: &symphony_vnext::config::ProjectConfig,
    ) -> Result<Vec<LinearIssue>, LinearClientError> {
        Ok(self
            .issues_by_project
            .get(&project.id)
            .cloned()
            .unwrap_or_default())
    }

    async fn transition_issue(
        &self,
        issue_id: &str,
        transition: LinearTransition,
    ) -> Result<(), LinearClientError> {
        self.transitions
            .lock()
            .expect("transitions lock")
            .push((issue_id.to_string(), transition));
        Ok(())
    }
}

#[derive(Debug, Default)]
struct ScriptedOpenCodeLauncher {
    handoff: Option<OpenCodeHandoff>,
    repairs: std::sync::Mutex<Vec<(String, String)>>,
}

impl ScriptedOpenCodeLauncher {
    fn new(handoff: Option<OpenCodeHandoff>) -> Self {
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
impl OpenCodeLauncher for ScriptedOpenCodeLauncher {
    async fn launch(
        &self,
        spec: &opencode::OpenCodeLaunchSpec,
    ) -> Result<opencode::OpenCodeStartedSession, opencode::OpenCodeError> {
        Ok(opencode::OpenCodeStartedSession {
            session_id: format!("scripted:{}", spec.cwd.display()),
        })
    }

    async fn latest_handoff(
        &self,
        session: &OpenCodeSessionRecord,
    ) -> Result<Option<OpenCodeHandoff>, opencode::OpenCodeError> {
        Ok(self
            .handoff
            .clone()
            .filter(|handoff| handoff.session_id == session.session_id))
    }

    async fn continue_repair(
        &self,
        session: &OpenCodeSessionRecord,
        failure_fingerprint: &str,
    ) -> Result<(), opencode::OpenCodeError> {
        self.repairs
            .lock()
            .expect("repairs lock")
            .push((session.session_id.clone(), failure_fingerprint.to_string()));
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct RecordingGraphqlTransport {
    responses: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<serde_json::Value>>>,
    requests: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
}

impl RecordingGraphqlTransport {
    fn new(responses: Vec<serde_json::Value>) -> Self {
        Self {
            responses: std::sync::Arc::new(std::sync::Mutex::new(responses.into())),
            requests: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn requests(&self) -> Vec<serde_json::Value> {
        self.requests.lock().expect("requests lock").clone()
    }
}

#[async_trait::async_trait]
impl LinearGraphqlTransport for RecordingGraphqlTransport {
    async fn post_graphql(
        &self,
        endpoint: &str,
        api_key: &str,
        request: serde_json::Value,
    ) -> Result<serde_json::Value, LinearClientError> {
        assert_eq!(endpoint, "https://linear.example/graphql");
        assert_eq!(api_key, "linear-token");
        self.requests.lock().expect("requests lock").push(request);
        self.responses
            .lock()
            .expect("responses lock")
            .pop_front()
            .ok_or_else(|| LinearClientError::Message("missing fake response".into()))
    }
}
