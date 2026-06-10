use std::{fs, path::PathBuf, thread, time::Duration};

use symphony_vnext::{
    api::{RuntimeDashboardApi, RuntimeReadModel},
    cli,
    config::RootConfig,
    daemon,
    linear::{
        LinearBlocker, LinearClient, LinearClientError, LinearIssue, LinearIssueEvidence,
        LinearTransition,
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
    workflow_path: /home/agent/proj/symphony/elixir/WORKFLOW.md
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
      model: null
      permission_policy: reject
    eval:
      default_suite: symphony-vnext-smoke
    concurrency:
      max_sessions: 2
"#
}

#[test]
fn multiproject_config_loads_deterministically_and_validates_required_fields() {
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

#[test]
fn config_rejects_codex_compatibility_fields() {
    let with_codex = valid_config_yaml().replace(
        "    opencode:\n",
        "    codex:\n      command: codex\n    opencode:\n",
    );

    let err =
        RootConfig::from_yaml_str(&with_codex).expect_err("codex config must not be accepted");
    assert!(err.to_string().contains("codex"), "{err}");
}

#[test]
fn sqlite_migrations_initialize_empty_runtime_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let store = SqliteStore::open(&db_path).expect("open sqlite");

    store.migrate().expect("migrate");

    assert_eq!(
        store.applied_migrations().expect("migrations"),
        vec!["001_runtime_state"]
    );
    assert_eq!(
        store.projects().expect("empty projects"),
        Vec::<ProjectStateRecord>::new()
    );
}

#[test]
fn runtime_state_persists_and_reloads_by_project_issue_and_session() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");

    {
        let store = SqliteStore::open(&db_path).expect("open sqlite");
        store.migrate().expect("migrate");
        store
            .upsert_project(ProjectStateRecord {
                project_id: "symphony".into(),
                name: "Symphony".into(),
                enabled: true,
                lifecycle_stage: LifecycleStage::Running,
                cleanup_status: CleanupStatus::Clean,
            })
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
            .expect("session");
    }

    let reloaded = SqliteStore::open(&db_path).expect("reopen sqlite");
    reloaded.migrate().expect("migrate idempotently");

    let project = reloaded
        .project("symphony")
        .expect("query project")
        .expect("project row");
    assert_eq!(project.lifecycle_stage, LifecycleStage::Running);

    let issues = reloaded
        .issues_for_project("symphony")
        .expect("query project issues");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].identifier, "SYM-25");
    assert_eq!(
        issues[0].failure.as_ref().expect("failure").kind,
        "validation"
    );

    let issue = reloaded
        .issue("symphony", "0af8ad67-37b9-412a-9869-82ca96b418e1")
        .expect("query issue")
        .expect("issue row");
    assert_eq!(issue.cleanup_status, CleanupStatus::Pending);

    let session = reloaded
        .opencode_session(
            "symphony",
            "0af8ad67-37b9-412a-9869-82ca96b418e1",
            "oc-session-1",
        )
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

    let read_model = RuntimeReadModel::from_store(&reloaded).expect("read model");
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

#[test]
fn dashboard_api_snapshots_aggregate_project_drilldown_and_issue_detail() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
    let store = SqliteStore::open(&db_path).expect("open sqlite");
    store.migrate().expect("migrate");
    store.reconcile_projects(&config).expect("projects");

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
    store.upsert_issue(repair).expect("repair issue");

    let mut provider_blocked = test_issue("symphony", "provider", "SYM-92", "Need Owner Input");
    provider_blocked.lifecycle_stage = LifecycleStage::Blocked;
    provider_blocked.blocker = Some(BlockerRecord {
        kind: "provider_blocker".into(),
        message: "provider quota exhausted".into(),
    });
    store
        .upsert_issue(provider_blocked)
        .expect("provider issue");

    let mut completed = test_issue("symphony", "done", "SYM-93", "Done");
    completed.lifecycle_stage = LifecycleStage::Completed;
    completed.cleanup_status = CleanupStatus::Complete;
    store.upsert_issue(completed).expect("done issue");

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
    store.upsert_opencode_session(session).expect("session");
    store
        .upsert_opencode_stage_event(OpenCodeStageEventRecord {
            project_id: "symphony".into(),
            issue_id: "repair".into(),
            session_id: "oc-repair".into(),
            sequence: 1,
            stage: OpenCodeStage::Running,
            event: Some("implementation_started".into()),
        })
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
        .expect("eval run");

    let api = RuntimeDashboardApi::from_store(&config, &store).expect("dashboard api");
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

#[test]
fn opencode_acp_launch_spec_uses_stdio_command_isolated_worktree_and_full_issue_prompt() {
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

#[test]
fn stdio_launcher_writes_prompt_to_child_stdin() {
    let dir = tempfile::tempdir().expect("tempdir");
    let captured_prompt_path = dir.path().join("captured_prompt.txt");
    let spec = opencode::OpenCodeLaunchSpec {
        command: PathBuf::from("/bin/sh"),
        args: vec!["-c".into(), "cat > captured_prompt.txt".into()],
        cwd: dir.path().to_path_buf(),
        prompt: "Full Linear issue spec with eval defaults".into(),
        permission_policy: PermissionPolicy::Reject,
    };
    let launcher = opencode::StdioOpenCodeLauncher;

    let started = launcher.launch(&spec).expect("launch stdio child");

    assert!(started.session_id.starts_with("pid:"));
    for _ in 0..50 {
        if let Ok(captured) = fs::read_to_string(&captured_prompt_path)
            && captured == spec.prompt
        {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }

    panic!(
        "prompt was not written to child stdin; captured={:?}",
        fs::read_to_string(captured_prompt_path)
    );
}

#[test]
fn opencode_event_ingestion_updates_stage_telemetry_without_losing_session_linkage() {
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

#[test]
fn opencode_silence_is_observable_without_marking_session_failed() {
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

#[test]
fn daemon_once_entrypoint_validates_config_migrates_and_reconciles_projects() {
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
    .expect("daemon bootstrap");

    let store = SqliteStore::open(&db_path).expect("reopen sqlite");
    store.migrate().expect("migrate idempotently");

    let project = store
        .project("symphony")
        .expect("query project")
        .expect("project row");
    assert_eq!(project.name, "Symphony");
    assert_eq!(project.lifecycle_stage, LifecycleStage::Queued);
    assert_eq!(project.cleanup_status, CleanupStatus::Clean);
}

#[test]
fn orchestration_dispatches_one_eligible_todo_by_project_capacity_and_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
    let store = SqliteStore::open(&db_path).expect("open sqlite");
    store.migrate().expect("migrate");
    store.reconcile_projects(&config).expect("projects");
    store
        .upsert_issue(test_issue("symphony", "running-1", "SYM-21", "In Progress"))
        .expect("running issue");

    let client = RecordingLinearClient::new(vec![
        linear_issue("backlog-1", "SYM-20", "Backlog", Some(1)),
        linear_issue("todo-low-priority", "SYM-30", "Todo", Some(3)),
        linear_issue("todo-high-priority", "SYM-22", "Todo", Some(1)),
    ]);

    let report =
        daemon::run_once_with_linear_client(&config, &store, &client).expect("orchestrate once");

    assert_eq!(report.dispatched, vec!["SYM-22"]);
    assert_eq!(
        client.transitions(),
        vec![("todo-high-priority".into(), LinearTransition::InProgress)]
    );
    assert_eq!(
        store
            .issue("symphony", "todo-high-priority")
            .expect("query dispatched")
            .expect("dispatched")
            .lifecycle_stage,
        LifecycleStage::Running
    );
    assert!(
        store
            .issue("symphony", "backlog-1")
            .expect("backlog")
            .is_none()
    );
}

#[test]
fn orchestration_never_dispatches_nonterminal_blockers_or_backlog() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
    let store = SqliteStore::open(&db_path).expect("open sqlite");
    store.migrate().expect("migrate");
    store.reconcile_projects(&config).expect("projects");

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

    let report =
        daemon::run_once_with_linear_client(&config, &store, &client).expect("orchestrate once");

    assert_eq!(report.dispatched, vec!["SYM-41"]);
    assert_eq!(
        client.transitions(),
        vec![("unblocked".into(), LinearTransition::InProgress)]
    );
    let blocked_row = store
        .issue("symphony", "blocked")
        .expect("query blocked")
        .expect("blocked row");
    assert_eq!(blocked_row.lifecycle_stage, LifecycleStage::Blocked);
    assert_eq!(blocked_row.blocker.expect("blocker").kind, "linear_blocker");
    assert!(
        store
            .issue("symphony", "backlog")
            .expect("backlog")
            .is_none()
    );
}

#[test]
fn orchestration_reconciles_persisted_backlog_without_counting_capacity() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
    let store = SqliteStore::open(&db_path).expect("open sqlite");
    store.migrate().expect("migrate");
    store.reconcile_projects(&config).expect("projects");
    store
        .upsert_issue(test_issue(
            "symphony",
            "parked-plan",
            "SYM-45",
            "In Progress",
        ))
        .expect("persisted running backlog issue");
    store
        .upsert_issue(test_issue(
            "symphony",
            "still-running",
            "SYM-46",
            "In Progress",
        ))
        .expect("persisted running issue");

    let client = RecordingLinearClient::new(vec![
        linear_issue("parked-plan", "SYM-45", "Backlog", Some(1)),
        linear_issue("eligible", "SYM-47", "Todo", Some(2)),
    ]);

    let report =
        daemon::run_once_with_linear_client(&config, &store, &client).expect("orchestrate once");

    assert_eq!(report.dispatched, vec!["SYM-47"]);
    assert_eq!(
        store
            .issue("symphony", "parked-plan")
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

#[test]
fn orchestration_keeps_owner_input_parked_until_answer_or_manual_todo() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
    let store = SqliteStore::open(&db_path).expect("open sqlite");
    store.migrate().expect("migrate");
    store.reconcile_projects(&config).expect("projects");

    let parked = linear_issue("parked", "SYM-50", "Need Owner Input", Some(1));
    let answered =
        linear_issue("answered", "SYM-51", "Need Owner Input", Some(2)).with_new_owner_answer(true);
    let manually_requeued = linear_issue("manual", "SYM-52", "Todo", Some(3));
    let client = RecordingLinearClient::new(vec![parked, answered, manually_requeued]);

    let report =
        daemon::run_once_with_linear_client(&config, &store, &client).expect("orchestrate once");

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
            .expect("query parked")
            .expect("parked")
            .lifecycle_stage,
        LifecycleStage::Blocked
    );
}

#[test]
fn orchestration_reconciles_terminal_issues_and_avoids_duplicate_dispatch_after_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
    let store = SqliteStore::open(&db_path).expect("open sqlite");
    store.migrate().expect("migrate");
    store.reconcile_projects(&config).expect("projects");
    store
        .upsert_issue(test_issue("symphony", "finished", "SYM-60", "In Progress"))
        .expect("finished issue");

    let client = RecordingLinearClient::new(vec![
        linear_issue("finished", "SYM-60", "Done", Some(1)),
        linear_issue("new-work", "SYM-61", "Todo", Some(2)),
    ]);

    daemon::run_once_with_linear_client(&config, &store, &client).expect("first poll");
    daemon::run_once_with_linear_client(&config, &store, &client).expect("restart poll");

    assert_eq!(
        client.transitions(),
        vec![("new-work".into(), LinearTransition::InProgress)]
    );
    let finished = store
        .issue("symphony", "finished")
        .expect("query finished")
        .expect("finished");
    assert_eq!(finished.lifecycle_stage, LifecycleStage::Completed);
    assert_eq!(finished.cleanup_status, CleanupStatus::Pending);
    assert_eq!(
        store
            .opencode_sessions_for_issue("symphony", "new-work")
            .expect("sessions")
            .len(),
        1
    );
}

#[test]
fn passing_opencode_handoff_moves_done_records_git_metadata_and_removes_worktree() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let worktree = dir.path().join("SYM-80-worktree");
    fs::create_dir_all(&worktree).expect("worktree");
    fs::write(worktree.join("artifact.txt"), "done").expect("artifact");
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
    let store = SqliteStore::open(&db_path).expect("open sqlite");
    store.migrate().expect("migrate");
    store.reconcile_projects(&config).expect("projects");
    store
        .upsert_issue(test_issue("symphony", "completed", "SYM-80", "In Progress"))
        .expect("running issue");
    store
        .upsert_opencode_session(test_session("symphony", "completed", "oc-80", &worktree))
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

    daemon::run_once_with_clients(&config, &store, &client, &opencode).expect("orchestrate once");

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
}

#[test]
fn eval_failure_stays_in_opencode_repair_loop_without_linear_churn() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let worktree = dir.path().join("SYM-81-worktree");
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
    let store = SqliteStore::open(&db_path).expect("open sqlite");
    store.migrate().expect("migrate");
    store.reconcile_projects(&config).expect("projects");
    store
        .upsert_issue(test_issue("symphony", "repair", "SYM-81", "In Progress"))
        .expect("running issue");
    store
        .upsert_opencode_session(test_session("symphony", "repair", "oc-81", &worktree))
        .expect("running session");
    let client = RecordingLinearClient::new(vec![linear_issue(
        "repair",
        "SYM-81",
        "In Progress",
        Some(1),
    )]);
    let opencode =
        ScriptedOpenCodeLauncher::new(Some(eval_failed_handoff("oc-81", "fmt-check-7f")));

    daemon::run_once_with_clients(&config, &store, &client, &opencode).expect("orchestrate once");

    assert!(client.transitions().is_empty());
    assert_eq!(
        opencode.repairs(),
        vec![("oc-81".into(), "fmt-check-7f".into())]
    );
    let issue = store
        .issue("symphony", "repair")
        .expect("query repair")
        .expect("repair issue");
    assert_eq!(issue.state, "In Progress");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Running);
    let failure = issue.failure.expect("failure");
    assert_eq!(failure.kind, "eval_failure");
    assert_eq!(failure.fingerprint.as_deref(), Some("fmt-check-7f"));
    assert_eq!(failure.occurrence_count, 1);
}

#[test]
fn repeated_identical_eval_failure_parks_owner_input_with_typed_evidence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let worktree = dir.path().join("SYM-82-worktree");
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
    let store = SqliteStore::open(&db_path).expect("open sqlite");
    store.migrate().expect("migrate");
    store.reconcile_projects(&config).expect("projects");
    let mut issue = test_issue("symphony", "repeat", "SYM-82", "In Progress");
    issue.failure = Some(FailureRecord {
        kind: "eval_failure".into(),
        message: "lint-loop".into(),
        fingerprint: Some("lint-loop".into()),
        occurrence_count: 1,
    });
    store.upsert_issue(issue).expect("running issue");
    store
        .upsert_opencode_session(test_session("symphony", "repeat", "oc-82", &worktree))
        .expect("running session");
    let client = RecordingLinearClient::new(vec![linear_issue(
        "repeat",
        "SYM-82",
        "In Progress",
        Some(1),
    )]);
    let opencode = ScriptedOpenCodeLauncher::new(Some(eval_failed_handoff("oc-82", "lint-loop")));

    daemon::run_once_with_clients(&config, &store, &client, &opencode).expect("orchestrate once");

    assert_eq!(
        client.transitions(),
        vec![("repeat".into(), LinearTransition::NeedOwnerInput)]
    );
    assert!(opencode.repairs().is_empty());
    let issue = store
        .issue("symphony", "repeat")
        .expect("query repeat")
        .expect("repeat issue");
    assert_eq!(issue.state, "Need Owner Input");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Blocked);
    assert_eq!(
        issue.blocker.expect("blocker").kind,
        "repeated_eval_failure"
    );
}

#[test]
fn provider_blocker_owner_question_and_malformed_handoff_park_without_closing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
    let store = SqliteStore::open(&db_path).expect("open sqlite");
    store.migrate().expect("migrate");
    store.reconcile_projects(&config).expect("projects");

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
            .expect("running issue");
        store
            .upsert_opencode_session(test_session(
                "symphony",
                issue_id,
                &handoff.session_id,
                &worktree,
            ))
            .expect("running session");
        let client = RecordingLinearClient::new(vec![linear_issue(
            issue_id,
            identifier,
            "In Progress",
            Some(1),
        )]);
        let opencode = ScriptedOpenCodeLauncher::new(Some(handoff));

        daemon::run_once_with_clients(&config, &store, &client, &opencode)
            .expect("orchestrate once");

        assert_eq!(
            client.transitions(),
            vec![(issue_id.into(), LinearTransition::NeedOwnerInput)]
        );
        let issue = store
            .issue("symphony", issue_id)
            .expect("query parked")
            .expect("parked issue");
        assert_eq!(issue.state, "Need Owner Input");
        assert_eq!(issue.lifecycle_stage, LifecycleStage::Blocked);
        assert_eq!(issue.blocker.expect("blocker").kind, expected_kind);
    }
}

#[test]
fn rust_path_parks_legacy_review_and_rca_states_instead_of_preserving_them() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_yaml_str(valid_config_yaml()).expect("config");
    let store = SqliteStore::open(&db_path).expect("open sqlite");
    store.migrate().expect("migrate");
    store.reconcile_projects(&config).expect("projects");
    let client = RecordingLinearClient::new(vec![
        linear_issue("review", "SYM-86", "In Review", Some(1)),
        linear_issue("rca", "SYM-87", "RCA Required", Some(2)),
    ]);

    daemon::run_once_with_linear_client(&config, &store, &client).expect("orchestrate once");

    assert_eq!(
        client.transitions(),
        vec![
            ("review".into(), LinearTransition::NeedOwnerInput),
            ("rca".into(), LinearTransition::NeedOwnerInput),
        ]
    );
    for issue_id in ["review", "rca"] {
        let issue = store
            .issue("symphony", issue_id)
            .expect("query parked")
            .expect("parked issue");
        assert_eq!(issue.state, "Need Owner Input");
        assert_eq!(issue.lifecycle_stage, LifecycleStage::Blocked);
        assert_eq!(issue.blocker.expect("blocker").kind, "legacy_runtime_state");
    }
}

#[test]
fn orchestration_processes_multiple_projects_in_config_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_yaml_str(&valid_config_yaml().replace(
        "  - id: symphony\n",
        "  - id: alpha\n    name: Alpha\n    enabled: true\n    workflow_path: /home/agent/proj/alpha/WORKFLOW.md\n    repo_path: /home/agent/proj/alpha\n    branch:\n      base: main\n      worktree_root: /home/agent/.symphony/workspaces/opencode/alpha\n    linear:\n      team_key: ALPHA\n      project_id: alpha-project\n      project_milestone_id: alpha-milestone\n    opencode:\n      command: /usr/local/bin/opencode\n      args: [\"acp\"]\n      agent: build\n      model: null\n      permission_policy: reject\n    eval:\n      default_suite: alpha-smoke\n    concurrency:\n      max_sessions: 1\n  - id: symphony\n",
    ))
    .expect("config");
    let store = SqliteStore::open(&db_path).expect("open sqlite");
    store.migrate().expect("migrate");
    store.reconcile_projects(&config).expect("projects");

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

    let report =
        daemon::run_once_with_linear_client(&config, &store, &client).expect("orchestrate once");

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

#[derive(Debug)]
struct RecordingLinearClient {
    issues: Vec<LinearIssue>,
    transitions: std::cell::RefCell<Vec<(String, LinearTransition)>>,
    evidence: std::cell::RefCell<Vec<(String, LinearIssueEvidence)>>,
}

impl RecordingLinearClient {
    fn new(issues: Vec<LinearIssue>) -> Self {
        Self {
            issues,
            transitions: std::cell::RefCell::new(Vec::new()),
            evidence: std::cell::RefCell::new(Vec::new()),
        }
    }

    fn transitions(&self) -> Vec<(String, LinearTransition)> {
        self.transitions.borrow().clone()
    }

    fn evidence(&self) -> Vec<(String, LinearIssueEvidence)> {
        self.evidence.borrow().clone()
    }
}

impl LinearClient for RecordingLinearClient {
    fn fetch_candidate_issues(
        &self,
        _project: &symphony_vnext::config::ProjectConfig,
    ) -> Result<Vec<LinearIssue>, LinearClientError> {
        Ok(self.issues.clone())
    }

    fn transition_issue(
        &self,
        issue_id: &str,
        transition: LinearTransition,
    ) -> Result<(), LinearClientError> {
        self.transitions
            .borrow_mut()
            .push((issue_id.to_string(), transition));
        Ok(())
    }

    fn record_issue_evidence(
        &self,
        issue_id: &str,
        evidence: LinearIssueEvidence,
    ) -> Result<(), LinearClientError> {
        self.evidence
            .borrow_mut()
            .push((issue_id.to_string(), evidence));
        Ok(())
    }
}

#[derive(Debug)]
struct ProjectAwareLinearClient {
    issues_by_project: std::collections::HashMap<String, Vec<LinearIssue>>,
    transitions: std::cell::RefCell<Vec<(String, LinearTransition)>>,
}

impl ProjectAwareLinearClient {
    fn new<const N: usize>(issues: [(&str, Vec<LinearIssue>); N]) -> Self {
        Self {
            issues_by_project: issues
                .into_iter()
                .map(|(project_id, issues)| (project_id.to_string(), issues))
                .collect(),
            transitions: std::cell::RefCell::new(Vec::new()),
        }
    }

    fn transitions(&self) -> Vec<(String, LinearTransition)> {
        self.transitions.borrow().clone()
    }
}

impl LinearClient for ProjectAwareLinearClient {
    fn fetch_candidate_issues(
        &self,
        project: &symphony_vnext::config::ProjectConfig,
    ) -> Result<Vec<LinearIssue>, LinearClientError> {
        Ok(self
            .issues_by_project
            .get(&project.id)
            .cloned()
            .unwrap_or_default())
    }

    fn transition_issue(
        &self,
        issue_id: &str,
        transition: LinearTransition,
    ) -> Result<(), LinearClientError> {
        self.transitions
            .borrow_mut()
            .push((issue_id.to_string(), transition));
        Ok(())
    }
}

#[derive(Debug, Default)]
struct ScriptedOpenCodeLauncher {
    handoff: Option<OpenCodeHandoff>,
    repairs: std::cell::RefCell<Vec<(String, String)>>,
}

impl ScriptedOpenCodeLauncher {
    fn new(handoff: Option<OpenCodeHandoff>) -> Self {
        Self {
            handoff,
            repairs: std::cell::RefCell::new(Vec::new()),
        }
    }

    fn repairs(&self) -> Vec<(String, String)> {
        self.repairs.borrow().clone()
    }
}

impl OpenCodeLauncher for ScriptedOpenCodeLauncher {
    fn launch(
        &self,
        spec: &opencode::OpenCodeLaunchSpec,
    ) -> Result<opencode::OpenCodeStartedSession, opencode::OpenCodeError> {
        Ok(opencode::OpenCodeStartedSession {
            session_id: format!("scripted:{}", spec.cwd.display()),
        })
    }

    fn latest_handoff(
        &self,
        session: &OpenCodeSessionRecord,
    ) -> Result<Option<OpenCodeHandoff>, opencode::OpenCodeError> {
        Ok(self
            .handoff
            .clone()
            .filter(|handoff| handoff.session_id == session.session_id))
    }

    fn continue_repair(
        &self,
        session: &OpenCodeSessionRecord,
        failure_fingerprint: &str,
    ) -> Result<(), opencode::OpenCodeError> {
        self.repairs
            .borrow_mut()
            .push((session.session_id.clone(), failure_fingerprint.to_string()));
        Ok(())
    }
}
