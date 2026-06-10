use super::*;

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
