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
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
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
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
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

    for (session_id, message_id, part_id) in [
        ("ses-root", "msg-root", "part-root"),
        ("ses-child", "msg-child", "part-child"),
    ] {
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, 1000, 2000, ?3)",
            libsql::params![message_id, session_id, serde_json::json!({"role":"assistant"}).to_string()],
        )
        .await
        .expect("insert message");
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, 1000, 2000, ?4)",
            libsql::params![part_id, message_id, session_id, serde_json::json!({"type":"text","text":"local raw transcript"}).to_string()],
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
