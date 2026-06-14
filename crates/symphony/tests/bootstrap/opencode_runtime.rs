use super::*;

#[tokio::test]
async fn opencode_session_archive_preserves_session_tree_before_deleting_sqlite_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("opencode.db");
    seed_opencode_session_tree(&db_path).await;
    let archive_root = dir.path().join("archives");

    let report =
        opencode::archive_and_delete_session_tree(opencode::OpenCodeSessionArchiveRequest {
            opencode_database_path: db_path.clone(),
            archive_root: archive_root.clone(),
            project_id: "mnemesh".into(),
            issue_id: "issue-100".into(),
            issue_identifier: "MNE-100".into(),
            root_session_id: "ses-root".into(),
        })
        .await
        .expect("archive session tree");

    assert_eq!(report.sessions_archived, 2);
    assert_eq!(report.messages_archived, 2);
    assert_eq!(report.parts_archived, 2);
    assert_eq!(report.todos_archived, 1);
    assert_eq!(report.sessions_deleted, 2);
    assert_eq!(
        report.artifact_root,
        archive_root
            .join("mnemesh")
            .join("MNE-100")
            .join("ses-root")
    );

    let manifest =
        fs::read_to_string(report.artifact_root.join("manifest.json")).expect("manifest archived");
    assert!(manifest.contains("symphony.opencode_session_archive.v1"));
    assert!(manifest.contains("OpenCode SQLite session tables across root and child sessions"));
    assert!(manifest.contains("\"raw_transcripts_retained_locally\": true"));
    assert!(report.artifact_root.join("sessions.json").exists());
    assert!(report.artifact_root.join("raw").join("parts.json").exists());

    let remaining_sessions = opencode_row_count(&db_path, "session").await;
    let remaining_messages = opencode_row_count(&db_path, "message").await;
    let remaining_parts = opencode_row_count(&db_path, "part").await;
    let remaining_todos = opencode_row_count(&db_path, "todo").await;
    assert_eq!(remaining_sessions, 0);
    assert_eq!(remaining_messages, 0);
    assert_eq!(remaining_parts, 0);
    assert_eq!(remaining_todos, 0);
}

#[tokio::test]
async fn opencode_session_tree_metrics_count_subagent_activity_and_tokens() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("opencode.db");
    seed_opencode_session_tree(&db_path).await;

    let metrics = opencode::read_session_tree_metrics(&db_path, "ses-root")
        .await
        .expect("read metrics")
        .expect("session tree exists");

    assert_eq!(metrics.root_session_id, "ses-root");
    assert_eq!(metrics.session_count, 2);
    assert_eq!(metrics.subagent_count, 1);
    assert_eq!(metrics.message_count, 2);
    assert_eq!(metrics.part_count, 2);
    assert_eq!(metrics.todo_count, 1);
    assert_eq!(metrics.tokens_input, 150);
    assert_eq!(metrics.tokens_output, 30);
    assert_eq!(metrics.tokens_reasoning, 4);
    assert_eq!(metrics.tokens_cache_read, 600);
    assert_eq!(metrics.tokens_cache_write, 0);
    assert_eq!(metrics.tokens_total, 784);
    assert_eq!(metrics.active_agent.as_deref(), Some("rust-engineer"));
    assert_eq!(metrics.active_model.as_deref(), Some("gpt-5.5"));
    assert_eq!(metrics.last_updated_ms, Some(2000));
}

#[tokio::test]
async fn opencode_session_tree_activity_exposes_subagents_todos_and_recent_events() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("opencode.db");
    seed_opencode_session_tree(&db_path).await;

    let activity = opencode::read_session_tree_activity(&db_path, "ses-root", 8)
        .await
        .expect("read activity")
        .expect("activity exists");

    assert_eq!(activity.root_session_id, "ses-root");
    assert_eq!(activity.sessions.len(), 2);
    assert_eq!(activity.subagents.len(), 1);
    assert_eq!(activity.subagents[0].session_id, "ses-child");
    assert_eq!(
        activity.subagents[0].agent.as_deref(),
        Some("rust-engineer")
    );
    assert_eq!(activity.todos.len(), 1);
    assert_eq!(activity.todos[0].content, "Run eval");
    assert_eq!(activity.timeline.len(), 2);
    assert_eq!(activity.timeline[0].session_id, "ses-root");
    assert_eq!(activity.timeline[0].kind, "text");
    assert_eq!(activity.timeline[0].summary, "root transcript");
    assert_eq!(activity.timeline[1].session_id, "ses-child");
    assert_eq!(activity.timeline[1].kind, "tool");
    assert_eq!(activity.timeline[1].tool.as_deref(), Some("bash"));
    assert_eq!(activity.timeline[1].status.as_deref(), Some("running"));
    assert_eq!(activity.running_tool_count, 1);
}

#[tokio::test]
async fn dashboard_issue_detail_embeds_live_opencode_activity_from_sqlite() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime_db_path = dir.path().join("runtime.sqlite3");
    let opencode_db_path = dir.path().join("opencode.db");
    seed_opencode_session_tree(&opencode_db_path).await;
    let config_toml = valid_config_toml().replacen(
        "[[projects]]",
        &format!(
            "[opencode_storage]\ndatabase_path = \"{}\"\narchive_root = \"{}\"\n\n[[projects]]",
            opencode_db_path.display(),
            dir.path().join("archives").display()
        ),
        1,
    );
    let config = RootConfig::from_toml_str(&config_toml).expect("config");
    let store = SqliteStore::open(&runtime_db_path)
        .await
        .expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");

    let issue = test_issue("symphony", "activity", "SYM-120");
    store.upsert_issue(issue).await.expect("issue");
    let mut session = test_session(
        "symphony",
        "activity",
        "ses-root",
        "/home/agent/.symphony/workspaces/opencode/symphony/SYM-120",
    );
    session.process_id = Some(std::process::id());
    store
        .upsert_opencode_session(session)
        .await
        .expect("session");

    let api = RuntimeDashboardApi::from_store(&config, &store)
        .await
        .expect("dashboard api");
    let detail = api
        .issue_detail("symphony", "activity")
        .expect("issue detail")
        .expect("issue exists");
    let session = &detail.opencode_sessions[0];

    assert_eq!(session.process_id, Some(std::process::id()));
    assert_eq!(session.process_alive, Some(true));
    let activity = session.activity.as_ref().expect("opencode activity");
    assert_eq!(activity.subagents[0].session_id, "ses-child");
    assert_eq!(activity.todos[0].content, "Run eval");
    assert_eq!(activity.timeline[0].summary, "root transcript");
    assert!(session.activity_error.is_none());
}

#[tokio::test]
async fn opencode_acp_launch_spec_uses_stdio_command_isolated_worktree_and_full_issue_prompt() {
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
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
    assert_eq!(
        spec.mnemesh_workspace_root,
        Some(PathBuf::from("/home/agent/proj/symphony"))
    );
    assert!(spec.prompt.contains("SYM-27"), "{}", spec.prompt);
    assert!(
        spec.prompt
            .contains("Mnemesh workspace root: /home/agent/proj/symphony"),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt.contains(
            "Do not create or register a separate Mnemesh workspace for the isolated worktree"
        ),
        "{}",
        spec.prompt
    );
    assert!(spec.prompt.contains("symphony-smoke"), "{}", spec.prompt);
    assert!(
        spec.prompt
            .contains("fallback metadata, not a blanket workspace gate"),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt
            .contains("Treat the issue's Validation section as the authority"),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt
            .contains("For docs-only/no-code changes, run documentation/file-level validation"),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt.contains("do not run cargo nextest --workspace"),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt.contains("Commit policy for successful handoff"),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt
            .contains("If the task changes code, docs, config, tests, or any other git-tracked state, commit those changes before writing a success handoff"),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt
            .contains("Do not report success with changed_files unless git.head_sha is the commit that contains those changes"),
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
        branch_name: "feature/sym-200".into(),
        repo_path: None,
        mnemesh_workspace_root: Some(worktree.clone()),
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
async fn handoff_sidecar_accepts_eval_result_evidence_ref() {
    let dir = tempfile::tempdir().expect("tempdir");
    let worktree = dir.path().join("worktree");
    let sidecar_dir = worktree.join(".symphony");
    fs::create_dir_all(&sidecar_dir).expect("sidecar dir");
    fs::write(
        sidecar_dir.join("opencode-handoff.json"),
        r#"{
  "session_id": "ses-evidence-ref",
  "lifecycle_stages": ["running", "eval", "handoff", "completed"],
  "subagents": ["rust-engineer:ses-child"],
  "eval_results": [{"suite": "cargo test", "passed": true, "failure_fingerprint": null, "details": "ok", "evidence_ref": "mnemesh:evidence:abc123"}],
  "changed_files": ["crates/symphony/src/opencode/types.rs:80-87"],
  "git": {"branch": "feature/sym-37", "head_sha": "abc123", "pr_url": null, "worktree_path": "/tmp/worktree"},
  "risks": [],
  "stop_reason": {"type": "success"}
}"#,
    )
    .expect("handoff fixture");
    let launcher = opencode::StdioOpenCodeLauncher;
    let session = test_session("symphony", "issue-evidence", "ses-evidence-ref", &worktree);

    let handoff = launcher
        .latest_handoff(&session)
        .await
        .expect("handoff parse")
        .expect("handoff present");

    assert_eq!(
        handoff.eval_results[0].evidence_ref.as_deref(),
        Some("mnemesh:evidence:abc123")
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
        branch_name: "feature/sym-201".into(),
        repo_path: None,
        mnemesh_workspace_root: Some(dir.path().to_path_buf()),
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
async fn stdio_launcher_resumes_existing_session_without_replaying_prompt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let transcript_path = dir.path().join("acp-transcript.jsonl");
    let script_path = write_fake_acp_resume_script(dir.path(), &transcript_path);
    let worktree = dir.path().join("worktree");
    fs::create_dir_all(&worktree).expect("worktree");
    let spec = opencode::OpenCodeLaunchSpec {
        command: script_path,
        args: Vec::new(),
        cwd: worktree.clone(),
        worktree_root: None,
        issue_identifier: "SYM-202".into(),
        branch_name: "feature/sym-202".into(),
        repo_path: None,
        mnemesh_workspace_root: Some(dir.path().to_path_buf()),
        base_ref: None,
        agent: "build".into(),
        model: Some("openai/gpt-5.5".into()),
        effort: Some("high".into()),
        prompt: "Original prompt must not be replayed on resume".into(),
        permission_policy: PermissionPolicy::Reject,
    };
    let mut session = test_session("symphony", "issue-202", "ses-existing", &worktree);
    session.process_id = None;
    let launcher = opencode::StdioOpenCodeLauncher;

    let resumed = launcher
        .resume(&spec, &session)
        .await
        .expect("resume existing session");

    assert_eq!(resumed.session_id, "ses-existing");
    assert!(resumed.process_id.is_some());
    let transcript = fs::read_to_string(&transcript_path).expect("transcript");
    assert!(
        transcript.contains(r#""method": "initialize""#),
        "{transcript}"
    );
    assert!(
        transcript.contains(r#""method": "session/resume""#),
        "{transcript}"
    );
    assert!(
        transcript.contains(r#""sessionId": "ses-existing""#),
        "{transcript}"
    );
    assert!(
        !transcript.contains(r#""method": "session/new""#),
        "{transcript}"
    );
    assert!(
        !transcript.contains(r#""method": "session/prompt""#),
        "{transcript}"
    );
    assert!(
        !transcript.contains("Original prompt must not be replayed"),
        "{transcript}"
    );
}

#[tokio::test]
async fn installed_opencode_acp_supports_ndjson_config_options_without_prompting() {
    if std::env::var("SYMPHONY_LIVE_OPENCODE_ACP").ok().as_deref() != Some("1") {
        eprintln!("set SYMPHONY_LIVE_OPENCODE_ACP=1 to run installed OpenCode ACP smoke");
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
            "clientInfo": {"name": "symphony-test", "version": "0"},
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
            "title": "Symphony ACP contract smoke"
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
        branch_name: "feature/sym-200".into(),
        repo_path: Some(repo.clone()),
        mnemesh_workspace_root: Some(repo.clone()),
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
            assert_eq!(
                git_output(&worktree, ["branch", "--show-current"]).trim(),
                "feature/sym-200"
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
        branch_name: "feature/sym-200".into(),
        repo_path: Some(dir.path().join("repo")),
        mnemesh_workspace_root: Some(dir.path().join("repo")),
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
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let project = config.project("symphony").expect("project");
    let issue = linear_issue("issue-27", "SYM-27", "In Progress", Some(1));
    let spec = opencode::build_acp_launch_spec(project, &issue);
    let mut session = opencode::new_session_record(
        project,
        &issue,
        opencode::OpenCodeStartedSession {
            session_id: "oc-session-27".into(),
            process_id: None,
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
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let project = config.project("symphony").expect("project");
    let issue = linear_issue("issue-27", "SYM-27", "In Progress", Some(1));
    let spec = opencode::build_acp_launch_spec(project, &issue);
    let mut session = opencode::new_session_record(
        project,
        &issue,
        opencode::OpenCodeStartedSession {
            session_id: "oc-session-27".into(),
            process_id: None,
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
fn unchanged_opencode_db_snapshot_preserves_control_marker() {
    let mut session = test_session(
        "symphony",
        "work",
        "ses-control-marker",
        "/home/agent/.symphony/workspaces/opencode/symphony/SYM-67",
    );
    session.lifecycle_marker = Some("repair_prompted".into());
    session.last_event = Some("opencode_db_updated:2000".into());
    let previous_last_event = session.last_event.clone();
    let previous_marker = session.lifecycle_marker.clone();
    let metrics = opencode::OpenCodeSessionTreeMetrics {
        root_session_id: session.session_id.clone(),
        session_count: 1,
        subagent_count: 0,
        message_count: 3,
        part_count: 9,
        todo_count: 4,
        tokens_input: 10,
        tokens_output: 5,
        tokens_reasoning: 0,
        tokens_cache_read: 0,
        tokens_cache_write: 0,
        tokens_total: 15,
        cost_micros: 0,
        active_agent: Some("build".into()),
        active_model: Some("gpt-5.5".into()),
        last_updated_ms: Some(2000),
    };

    opencode::apply_session_tree_metrics_preserving_marker(
        &mut session,
        &metrics,
        previous_last_event.as_deref(),
        previous_marker.as_deref(),
    );

    assert_eq!(session.lifecycle_marker.as_deref(), Some("repair_prompted"));
    assert_eq!(
        session.last_event.as_deref(),
        Some("opencode_db_updated:2000")
    );
}

pub(crate) async fn seed_opencode_session_tree(path: &std::path::Path) {
    let database = libsql::Builder::new_local(path.display().to_string())
        .build()
        .await
        .expect("build opencode db");
    let conn = database.connect().expect("connect opencode db");
    conn.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;
        CREATE TABLE session (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            parent_id TEXT,
            slug TEXT NOT NULL,
            directory TEXT NOT NULL,
            title TEXT NOT NULL,
            version TEXT NOT NULL,
            share_url TEXT,
            summary_additions INTEGER,
            summary_deletions INTEGER,
            summary_files INTEGER,
            summary_diffs TEXT,
            revert TEXT,
            permission TEXT,
            time_created INTEGER NOT NULL,
            time_updated INTEGER NOT NULL,
            time_compacting INTEGER,
            time_archived INTEGER,
            workspace_id TEXT,
            path TEXT,
            agent TEXT,
            model TEXT,
            cost REAL DEFAULT 0 NOT NULL,
            tokens_input INTEGER DEFAULT 0 NOT NULL,
            tokens_output INTEGER DEFAULT 0 NOT NULL,
            tokens_reasoning INTEGER DEFAULT 0 NOT NULL,
            tokens_cache_read INTEGER DEFAULT 0 NOT NULL,
            tokens_cache_write INTEGER DEFAULT 0 NOT NULL
        );
        CREATE TABLE message (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            time_created INTEGER NOT NULL,
            time_updated INTEGER NOT NULL,
            data TEXT NOT NULL,
            FOREIGN KEY (session_id) REFERENCES session(id) ON DELETE CASCADE
        );
        CREATE TABLE part (
            id TEXT PRIMARY KEY,
            message_id TEXT NOT NULL,
            session_id TEXT NOT NULL,
            time_created INTEGER NOT NULL,
            time_updated INTEGER NOT NULL,
            data TEXT NOT NULL,
            FOREIGN KEY (message_id) REFERENCES message(id) ON DELETE CASCADE,
            FOREIGN KEY (session_id) REFERENCES session(id) ON DELETE CASCADE
        );
        CREATE TABLE session_message (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            type TEXT NOT NULL,
            time_created INTEGER NOT NULL,
            time_updated INTEGER NOT NULL,
            data TEXT NOT NULL,
            FOREIGN KEY (session_id) REFERENCES session(id) ON DELETE CASCADE
        );
        CREATE TABLE todo (
            session_id TEXT NOT NULL,
            content TEXT NOT NULL,
            status TEXT NOT NULL,
            priority TEXT NOT NULL,
            position INTEGER NOT NULL,
            time_created INTEGER NOT NULL,
            time_updated INTEGER NOT NULL,
            PRIMARY KEY(session_id, position),
            FOREIGN KEY (session_id) REFERENCES session(id) ON DELETE CASCADE
        );
        CREATE TABLE event (
            id TEXT PRIMARY KEY,
            aggregate_id TEXT NOT NULL,
            seq INTEGER NOT NULL,
            type TEXT NOT NULL,
            data TEXT NOT NULL
        );
        CREATE TABLE event_sequence (
            aggregate_id TEXT PRIMARY KEY,
            seq INTEGER NOT NULL
        );
        "#,
    )
    .await
    .expect("schema");

    for (id, parent_id, title, agent, input, output) in [
        ("ses-root", None, "Root build", "build", 100_i64, 20_i64),
        (
            "ses-child",
            Some("ses-root"),
            "Child engineer",
            "rust-engineer",
            50_i64,
            10_i64,
        ),
    ] {
        conn.execute(
            r#"
            INSERT INTO session (
                id, project_id, parent_id, slug, directory, title, version,
                time_created, time_updated, agent, model, cost, tokens_input,
                tokens_output, tokens_reasoning, tokens_cache_read,
                tokens_cache_write
            )
            VALUES (?1, 'project-row', ?2, ?1, '/tmp/work', ?3, '0',
                    1000, 2000, ?4, '{"id":"gpt-5.5","providerID":"openai"}',
                    0.0, ?5, ?6, 2, 300, 0)
            "#,
            libsql::params![id, parent_id, title, agent, input, output],
        )
        .await
        .expect("insert session");
    }

    for (session_id, message_id, part_id, time_updated, data) in [
        (
            "ses-root",
            "msg-root",
            "part-root",
            2001_i64,
            serde_json::json!({"type":"text","text":"root transcript"}),
        ),
        (
            "ses-child",
            "msg-child",
            "part-child",
            2000_i64,
            serde_json::json!({"type":"tool","tool":"bash","state":{"status":"running"},"title":"cargo check"}),
        ),
    ] {
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, 1000, 2000, ?3)",
            libsql::params![message_id, session_id, serde_json::json!({"role":"assistant"}).to_string()],
        )
        .await
        .expect("insert message");
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, 1000, ?4, ?5)",
            libsql::params![part_id, message_id, session_id, time_updated, data.to_string()],
        )
        .await
        .expect("insert part");
    }
    conn.execute(
        "INSERT INTO session_message (id, session_id, type, time_created, time_updated, data) VALUES ('switch-root', 'ses-root', 'agent-switched', 1000, 2000, '{}')",
        (),
    )
    .await
    .expect("insert session message");
    conn.execute(
        "INSERT INTO todo (session_id, content, status, priority, position, time_created, time_updated) VALUES ('ses-root', 'Run eval', 'completed', 'high', 0, 1000, 2000)",
        (),
    )
    .await
    .expect("insert todo");
}

async fn opencode_row_count(path: &std::path::Path, table: &str) -> u64 {
    assert!(matches!(table, "session" | "message" | "part" | "todo"));
    let database = libsql::Builder::new_local(path.display().to_string())
        .build()
        .await
        .expect("build opencode db");
    let conn = database.connect().expect("connect opencode db");
    let mut rows = conn
        .query(format!("SELECT count(*) FROM {table}").as_str(), ())
        .await
        .expect("count rows");
    let row = rows.next().await.expect("row result").expect("row");
    let count: i64 = row.get(0).expect("count value");
    count as u64
}

#[tokio::test]
async fn stdio_launcher_repairs_clean_existing_worktree_on_wrong_branch() {
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
    run_git(&repo, ["branch", "stale-branch"]);

    let root = dir.path().join("worktrees");
    let worktree = root.join("SYM-204");
    run_git(
        &repo,
        [
            "worktree",
            "add",
            "-B",
            "stale-branch",
            worktree.to_str().expect("worktree utf8"),
            "agent-server/opencode-runner-extension",
        ],
    );
    let transcript_path = dir.path().join("acp-transcript.jsonl");
    let script_path = write_fake_acp_script(dir.path(), &transcript_path);
    let spec = opencode::OpenCodeLaunchSpec {
        command: script_path,
        args: Vec::new(),
        cwd: worktree.clone(),
        worktree_root: Some(root),
        issue_identifier: "SYM-204".into(),
        branch_name: "feature/sym-204".into(),
        repo_path: Some(repo),
        mnemesh_workspace_root: Some(dir.path().join("repo")),
        base_ref: Some("agent-server/opencode-runner-extension".into()),
        agent: "build".into(),
        model: Some("openai/gpt-5.5".into()),
        effort: Some("high".into()),
        prompt: "Full Linear issue spec with eval defaults".into(),
        permission_policy: PermissionPolicy::Reject,
    };

    opencode::StdioOpenCodeLauncher
        .launch(&spec)
        .await
        .expect("clean stale worktree can be repaired");

    assert_eq!(
        git_output(&worktree, ["branch", "--show-current"]).trim(),
        "feature/sym-204"
    );
}

#[tokio::test]
async fn stdio_launcher_rejects_dirty_existing_worktree_on_wrong_branch() {
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
    run_git(&repo, ["branch", "stale-branch"]);

    let root = dir.path().join("worktrees");
    let worktree = root.join("SYM-205");
    run_git(
        &repo,
        [
            "worktree",
            "add",
            "-B",
            "stale-branch",
            worktree.to_str().expect("worktree utf8"),
            "agent-server/opencode-runner-extension",
        ],
    );
    fs::write(worktree.join("dirty.txt"), "local work").expect("dirty file");
    let spec = opencode::OpenCodeLaunchSpec {
        command: PathBuf::from("/bin/false"),
        args: Vec::new(),
        cwd: worktree.clone(),
        worktree_root: Some(root),
        issue_identifier: "SYM-205".into(),
        branch_name: "feature/sym-205".into(),
        repo_path: Some(repo),
        mnemesh_workspace_root: Some(dir.path().join("repo")),
        base_ref: Some("agent-server/opencode-runner-extension".into()),
        agent: "build".into(),
        model: Some("openai/gpt-5.5".into()),
        effort: Some("high".into()),
        prompt: "Full Linear issue spec with eval defaults".into(),
        permission_policy: PermissionPolicy::Reject,
    };

    let error = opencode::StdioOpenCodeLauncher
        .launch(&spec)
        .await
        .expect_err("dirty stale worktree must be rejected");

    assert!(
        matches!(error, opencode::OpenCodeError::InvalidWorktree(ref message) if message.contains("dirty or untracked files prevent safe repair") && message.contains("stale-branch")),
        "{error:?}"
    );
    assert_eq!(
        git_output(&worktree, ["branch", "--show-current"]).trim(),
        "stale-branch"
    );
}
