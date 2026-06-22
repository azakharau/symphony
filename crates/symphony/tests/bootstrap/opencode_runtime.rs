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
            project_id: "recall".into(),
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
        archive_root.join("recall").join("MNE-100").join("ses-root")
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
async fn opencode_session_tree_activity_uses_global_bounded_timeline_ordering() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("opencode.db");
    seed_opencode_session_tree(&db_path).await;

    let database = libsql::Builder::new_local(db_path.display().to_string())
        .build()
        .await
        .expect("build opencode db");
    let conn = database.connect().expect("connect opencode db");
    for (session_id, title, part_id, time_created, time_updated, summary) in [
        (
            "ses-review",
            "Review subagent",
            "part-review",
            1002_i64,
            2003_i64,
            "review transcript",
        ),
        (
            "ses-scout",
            "Scout subagent",
            "part-scout",
            1001_i64,
            2001_i64,
            "scout transcript",
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
            VALUES (?1, 'project-row', 'ses-root', ?1, '/tmp/work', ?2, '0',
                    ?3, ?4, 'rust-engineer', '{"id":"gpt-5.5","providerID":"openai"}',
                    0.0, 1, 1, 0, 0, 0)
            "#,
            libsql::params![session_id, title, time_created, time_updated],
        )
        .await
        .expect("insert extra session");
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5)",
            libsql::params![format!("msg-{session_id}"), session_id, time_created, time_updated, serde_json::json!({"role":"assistant"}).to_string()],
        )
        .await
        .expect("insert extra message");
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            libsql::params![part_id, format!("msg-{session_id}"), session_id, time_created, time_updated, serde_json::json!({"type":"text","text":summary}).to_string()],
        )
        .await
        .expect("insert extra part");
    }

    let activity = opencode::read_session_tree_activity(&db_path, "ses-root", 3)
        .await
        .expect("read activity")
        .expect("activity exists");

    assert_eq!(activity.subagents.len(), 3);
    assert_eq!(activity.timeline.len(), 3);
    assert_eq!(activity.timeline[0].part_id, "part-review");
    assert_eq!(activity.timeline[1].part_id, "part-scout");
    assert_eq!(activity.timeline[2].part_id, "part-root");
    assert!(
        !activity
            .timeline
            .iter()
            .any(|event| event.part_id == "part-child")
    );
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
    session.process_id = None;
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

    assert_eq!(session.process_id, None);
    assert_eq!(session.process_alive, None);
    let activity = session.activity.as_ref().expect("opencode activity");
    assert_eq!(activity.subagents[0].session_id, "ses-child");
    assert_eq!(activity.todos[0].content, "Run eval");
    assert_eq!(activity.timeline[0].summary, "root transcript");
    assert!(session.activity_error.is_none());

    let ui_issue = symphony::api::runtime_api_json_response(
        &config,
        &store,
        "/api/projects/symphony/issues/activity/ui",
    )
    .await
    .expect("ui issue response");
    assert!(ui_issue.body.contains(r#""activity""#));
    assert!(ui_issue.body.contains(r#""subagents""#));
    assert!(ui_issue.body.contains(r#""todos""#));
    assert!(!ui_issue.body.contains("cost_micros"));
}

#[tokio::test]
async fn opencode_acp_launch_spec_uses_stdio_command_isolated_worktree_and_full_issue_prompt() {
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let project = config.project("symphony").expect("project");
    let mut issue = linear_issue("issue-27", "SYM-27", "Todo", Some(1))
        .with_description("Implement the OpenCode ACP lifecycle runner with stage telemetry.");
    issue.upstream_context.push(LinearUpstreamContext {
        id: "upstream-55".into(),
        identifier: "NER-55".into(),
        title: "Canon source authority map".into(),
        state: "Done".into(),
        url: Some("https://linear.example/NER-55".into()),
        branch_name: Some("feature/ner-55-canon-source-authority-map".into()),
        recall_workspace_ids: vec!["workspace-54f6c799-4258-4b40-80ec-f0606bff3ce9".into()],
        recall_task_ids: vec!["task-ner-55".into()],
        accepted_artifacts: vec!["docs/canon-source-authority-map.md".into()],
        handoff_summary: Some(
            "## OpenCode Handoff Accepted\nCommitted 61f216d docs: add canon source authority map"
                .into(),
        ),
    });

    let spec = opencode::build_acp_launch_spec(project, &issue);

    assert_eq!(spec.command, PathBuf::from("/usr/local/bin/opencode"));
    assert_eq!(spec.args, vec!["acp"]);
    assert_eq!(
        spec.cwd,
        PathBuf::from("/home/agent/.symphony/workspaces/opencode/symphony/SYM-27")
    );
    assert_eq!(
        spec.recall_workspace_root,
        Some(PathBuf::from("/home/agent/proj/symphony"))
    );
    assert!(spec.prompt.contains("SYM-27"), "{}", spec.prompt);
    assert!(!spec.prompt.contains("Run OpenCode ACP"), "{}", spec.prompt);
    assert!(
        !spec.prompt.contains("OpenCode ACP session id"),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt
            .contains("Recall workspace root: /home/agent/proj/symphony"),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt.contains(
            "Do not create or register a separate Recall workspace for the isolated worktree"
        ),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt.contains(
            "Required OpenCode MCP tool is `recall_create_task`; do not use Codex-style tool names such as `mcp__recall__create_task`"
        ),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt.contains(
            "Required `recall_create_task` payload shape is exactly `objective`, `playbook`, `requested_by`, and `worktree` at top level"
        ),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt.contains("Do not send top-level `session_id`, `project`, `workspace`, `repo_root`, `worktree_path`, `actor_id`, `actor_type`, `label`, or `role`"),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt.contains(
            "`recall_create_task.requested_by` payload: include `actor_id`, `actor_type`, `label`, and `role`"
        ),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt.contains(
            "\"requested_by\":{\"actor_id\":\"symphony-opencode\",\"actor_type\":\"agent\",\"label\":\"Symphony OpenCode\",\"role\":\"implementation-runner\"}"
        ),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt.contains(
            "\"worktree\":{\"repo_root\":\"/home/agent/proj/symphony\",\"worktree_path\":\"/home/agent/proj/symphony\""
        ),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt.contains(
            "Never set `recall_create_task.worktree.worktree_path` to `/home/agent/.symphony/workspaces/opencode/symphony/SYM-27`"
        ),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt.contains("MCP tool-schema loop guard"),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt.contains("Upstream accepted context"),
        "{}",
        spec.prompt
    );
    for fragment in [
        "NER-55 (`Done`): Canon source authority map",
        "workspace-54f6c799-4258-4b40-80ec-f0606bff3ce9",
        "task-ner-55",
        "docs/canon-source-authority-map.md",
        "Committed 61f216d docs: add canon source authority map",
        "treat this as accepted upstream input; inspect the Recall refs/artifacts before rediscovering or replanning this surface",
    ] {
        assert!(spec.prompt.contains(fragment), "{}", spec.prompt);
    }
    assert!(
        spec.prompt.contains(
            "After two failed calls to the same MCP method for schema/validation reasons, stop retrying that method in this session"
        ),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt
            .contains("Delegated review/evaluator subagent contract"),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt.contains(
            "Delegated reviewer/evaluator subagents are read-only unless the issue spec explicitly says otherwise"
        ),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt.contains(
            "Do not ask delegated reviewer/evaluator subagents to call Recall mutation tools"
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
        spec.prompt.contains("Triage and owner-input boundary"),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt
            .contains("Use owner_question only for real owner, product, or permission questions"),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt.contains(
            "Use provider_blocker for provider, infrastructure, workspace, credential, or tool availability blockers; these are not owner input"
        ),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt.contains(
            "Treat missing or malformed handoff sidecars, stale process/session evidence, git closure mismatches, cleanup failures, prompt regressions, and evaluator contract failures as runtime/tooling defects"
        ),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt
            .contains("Do not requeue runtime/tooling defects to runnable Todo as product work"),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt.contains(
            "Classifier, model, and evaluator output is advisory only; only deterministic runtime policy and the Linear writer may create or mutate Linear issues"
        ),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt.contains(
            "Do not classify runtime/tooling defects as owner input unless a real owner, product, or permission decision is required"
        ),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt.contains(
            "Auto-created self-reference bugs use P0 Todo only for unsafe runtime advance or closure blockers; P1 degraded project paths and P2 non-blocking hardening default to Backlog unless hard policy explicitly escalates"
        ),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt.contains(
            "do not wait on or requeue the same active issue; park it with typed runtime-defect/provider evidence and create or link a separate self-reference bug"
        ),
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
            .contains("If the task changes code, docs, config, tests, or any other git-tracked state, commit and push those changes before writing a success handoff"),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt
            .contains("Do not report success with changed_files unless git.head_sha is the pushed commit that contains those changes and is reachable from origin/git.branch"),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt.contains(
            "If commit or push fails, do not write a success handoff; stop with a provider_blocker or eval_failed handoff"
        ),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt.contains(
            "After validation, commit, and push are complete, write the structured Symphony handoff JSON"
        ),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt
            .contains("valid JSON with durable execution evidence"),
        "{}",
        spec.prompt
    );
    for fragment in [
        "Symphony accepts OpenCode orchestrator field names",
        "subagents_used",
        "\"stop_reason\": \"accepted\"",
    ] {
        assert!(spec.prompt.contains(fragment), "{}", spec.prompt);
    }
    assert!(
        spec.prompt.contains("Do not write only prose fields such as result, summary, tests_run, or next_action without the structured git/eval/stop_reason fields above"),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt.contains(
            "Write or rewrite the handoff sidecar only after validation, commit, and push are complete"
        ),
        "{}",
        spec.prompt
    );
    assert!(
        spec.prompt.contains(
            "If there are truly no git changes, leave changed_files empty, keep git.branch and git.worktree_path populated, set git.head_sha to null"
        ),
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
async fn omp_acp_launch_spec_uses_provider_command_cwd_env_and_mode() {
    let config_toml = valid_config_toml().replace(
        "[projects.eval]\n",
        r#"[[projects.omp_acp_providers]]
id = "omp-primary"
command = "/tmp/mock-omp"
args = ["acp"]
cwd = "project_repo"
env_allowlist = ["PATH", "OMP_TOKEN"]
agent = "implementer"
model = "openai/gpt-5.5"
effort = "medium"

[projects.omp_acp_providers.capabilities]
acp_stdio = true
hook_evidence = true
sdk_session_evidence = true
rpc_secondary_mode = false
inverse_bridge_reference = true

[projects.eval]
"#,
    );
    let config = RootConfig::from_toml_str(&config_toml).expect("config");
    let project = config.project("symphony").expect("project");
    let issue = linear_issue("issue-27", "SYM-27", "Todo", Some(1));

    let spec = opencode::build_acp_launch_spec(project, &issue);

    assert_eq!(spec.provider_mode, RuntimeProviderMode::OmpAcp);
    assert_eq!(spec.provider_id.as_deref(), Some("omp-primary"));
    assert_eq!(spec.command, PathBuf::from("/tmp/mock-omp"));
    assert_eq!(spec.args, ["acp"]);
    assert_eq!(spec.cwd, PathBuf::from("/home/agent/proj/symphony"));
    assert_eq!(spec.env_allowlist, ["PATH", "OMP_TOKEN"]);
    assert_eq!(spec.agent, "implementer");
    assert_eq!(spec.model.as_deref(), Some("openai/gpt-5.5"));
}

#[tokio::test]
async fn mocked_omp_acp_launch_returns_session_telemetry_and_evidence_refs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let transcript_path = dir.path().join("omp-acp-transcript.jsonl");
    let transcript_literal =
        serde_json::to_string(&transcript_path.display().to_string()).expect("json path");
    let command = dir.path().join("mock-omp-acp.py");
    fs::write(
        &command,
        format!(
            r#"#!/usr/bin/env python3
import json, os, pathlib, sys
transcript_path = pathlib.Path({transcript_literal})
with transcript_path.open("a", encoding="utf-8") as transcript:
    transcript.write(json.dumps({{"argv": sys.argv, "cwd": os.getcwd(), "env": {{"SYMPHONY_ISSUE_WORKTREE": os.environ.get("SYMPHONY_ISSUE_WORKTREE"), "SYMPHONY_OMP_CLEANUP_MARKER": os.environ.get("SYMPHONY_OMP_CLEANUP_MARKER")}}}}, sort_keys=True) + "\n")
for line in sys.stdin:
    msg = json.loads(line)
    method = msg.get("method")
    with transcript_path.open("a", encoding="utf-8") as transcript:
        transcript.write(json.dumps(msg, sort_keys=True) + "\n")
    if method == "initialize":
        print(json.dumps({{"jsonrpc":"2.0","id":msg["id"],"result":{{"protocolVersion":1}}}}), flush=True)
    elif method == "session/new":
        print(json.dumps({{"jsonrpc":"2.0","id":msg["id"],"result":{{"sessionId":"omp-session-1","sdkSessionEvidenceRefs":["sdk:one","sdk:two","sdk:three","sdk:four","sdk:five","sdk:six","sdk:seven","sdk:eight","sdk:nine"]}}}}), flush=True)
    elif method == "session/prompt":
        print(json.dumps({{"jsonrpc":"2.0","id":msg["id"],"result":{{"ok":True}}}}), flush=True)
        break
"#,
        ),
    )
    .expect("write mock");
    fs::set_permissions(&command, fs::Permissions::from_mode(0o755)).expect("chmod");
    let worktree = dir.path().join("worktree");
    fs::create_dir_all(&worktree).expect("worktree");
    let spec = opencode::OpenCodeLaunchSpec {
        provider_mode: RuntimeProviderMode::OmpAcp,
        provider_id: Some("omp-primary".into()),
        command: command.clone(),
        args: vec!["acp".into()],
        cwd: worktree.clone(),
        env_allowlist: vec!["PATH".into()],
        worktree_root: None,
        issue_identifier: "SYM-102".into(),
        branch_name: "feature/sym-102".into(),
        repo_path: None,
        recall_workspace_root: None,
        base_ref: None,
        agent: "implementer".into(),
        model: Some("openai/gpt-5.5".into()),
        effort: None,
        prompt: "Implement SYM-102".into(),
        permission_policy: PermissionPolicy::Reject,
    };

    let started = opencode::StdioOpenCodeLauncher
        .launch(&spec)
        .await
        .expect("mock OMP launch");

    assert_eq!(started.session_id, "omp-session-1");
    assert!(started.process_id.is_some());
    assert_eq!(started.acp_frame_count, 5);
    assert_eq!(
        started.session_evidence_refs,
        [
            "sdk:one",
            "sdk:two",
            "sdk:three",
            "sdk:four",
            "sdk:five",
            "sdk:six",
            "sdk:seven",
            "sdk:eight"
        ]
    );
    for _ in 0..50 {
        if let Ok(transcript) = fs::read_to_string(&transcript_path)
            && transcript.contains(r#""method": "session/prompt""#)
        {
            assert!(transcript.contains(r#""argv": ["#), "{transcript}");
            assert!(
                transcript.contains(&command.display().to_string()),
                "{transcript}"
            );
            assert!(transcript.contains(r#""acp""#), "{transcript}");
            assert!(
                transcript.contains(&worktree.display().to_string()),
                "{transcript}"
            );
            assert!(
                transcript.contains(r#""method": "initialize""#),
                "{transcript}"
            );
            assert!(
                transcript.contains(r#""protocolVersion": 1"#),
                "{transcript}"
            );
            assert!(
                transcript.contains(r#""method": "session/new""#),
                "{transcript}"
            );
            assert!(transcript.contains(r#""title": "SYM-102""#), "{transcript}");
            assert!(
                transcript.contains(r#""agent": "implementer""#),
                "{transcript}"
            );
            assert!(
                transcript.contains(r#""model": "openai/gpt-5.5""#),
                "{transcript}"
            );
            assert!(
                transcript.contains(r#""SYMPHONY_ISSUE_WORKTREE""#),
                "{transcript}"
            );
            assert!(
                transcript.contains(r#""SYMPHONY_OMP_CLEANUP_MARKER""#),
                "{transcript}"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "OMP ACP transcript was not observed; transcript={:?}",
        fs::read_to_string(transcript_path)
    );
}

#[tokio::test]
async fn mocked_omp_acp_launch_with_env_allowlist_retains_issue_worktree_marker() {
    let dir = tempfile::tempdir().expect("tempdir");
    let command = dir.path().join("mock-omp-acp-env.py");
    fs::write(
        &command,
        r#"#!/usr/bin/env python3
import json, os, sys
if os.environ.get("SYMPHONY_ISSUE_WORKTREE") != os.getcwd():
    print("missing SYMPHONY_ISSUE_WORKTREE marker", file=sys.stderr, flush=True)
    sys.exit(2)
expected_cleanup_marker = "provider=omp-primary;issue=SYM-102;cwd=" + os.getcwd()
if os.environ.get("SYMPHONY_OMP_CLEANUP_MARKER") != expected_cleanup_marker:
    print("missing SYMPHONY_OMP_CLEANUP_MARKER marker", file=sys.stderr, flush=True)
    sys.exit(3)
for line in sys.stdin:
    msg = json.loads(line)
    method = msg.get("method")
    if method == "initialize":
        print(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{"protocolVersion":1}}), flush=True)
    elif method == "session/new":
        print(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{"sessionId":"omp-session-env"}}), flush=True)
    elif method == "session/prompt":
        print(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{"ok":True}}), flush=True)
        break
"#,
    )
    .expect("write mock");
    fs::set_permissions(&command, fs::Permissions::from_mode(0o755)).expect("chmod");
    let worktree = dir.path().join("worktree");
    fs::create_dir_all(&worktree).expect("worktree");
    let spec = opencode::OpenCodeLaunchSpec {
        provider_mode: RuntimeProviderMode::OmpAcp,
        provider_id: Some("omp-primary".into()),
        command,
        args: Vec::new(),
        cwd: worktree,
        env_allowlist: vec!["PATH".into()],
        worktree_root: None,
        issue_identifier: "SYM-102".into(),
        branch_name: "feature/sym-102".into(),
        repo_path: None,
        recall_workspace_root: None,
        base_ref: None,
        agent: "implementer".into(),
        model: Some("openai/gpt-5.5".into()),
        effort: None,
        prompt: "Implement SYM-102".into(),
        permission_policy: PermissionPolicy::Reject,
    };

    let started = opencode::StdioOpenCodeLauncher
        .launch(&spec)
        .await
        .expect("mock OMP launch");

    assert_eq!(started.session_id, "omp-session-env");
    assert!(started.process_id.is_some());
}

#[tokio::test]
async fn mocked_project_repo_omp_acp_launch_uses_issue_specific_cleanup_marker() {
    let dir = tempfile::tempdir().expect("tempdir");
    let command = dir.path().join("mock-project-repo-omp-acp-env.py");
    fs::write(
        &command,
        r#"#!/usr/bin/env python3
import json, os, sys
repo = os.getcwd()
if os.environ.get("SYMPHONY_ISSUE_WORKTREE") != repo:
    print("provider cwd marker changed", file=sys.stderr, flush=True)
    sys.exit(2)
expected_cleanup_marker = "provider=omp-primary;issue=SYM-102;cwd=" + repo
if os.environ.get("SYMPHONY_OMP_CLEANUP_MARKER") != expected_cleanup_marker:
    print("cleanup marker is not issue-specific", file=sys.stderr, flush=True)
    sys.exit(3)
for line in sys.stdin:
    msg = json.loads(line)
    method = msg.get("method")
    if method == "initialize":
        print(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{"protocolVersion":1}}), flush=True)
    elif method == "session/new":
        print(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{"sessionId":"omp-session-project-repo"}}), flush=True)
    elif method == "session/prompt":
        print(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{"ok":True}}), flush=True)
        break
"#,
    )
    .expect("write mock");
    fs::set_permissions(&command, fs::Permissions::from_mode(0o755)).expect("chmod");
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo");
    let spec = opencode::OpenCodeLaunchSpec {
        provider_mode: RuntimeProviderMode::OmpAcp,
        provider_id: Some("omp-primary".into()),
        command,
        args: Vec::new(),
        cwd: repo,
        env_allowlist: vec!["PATH".into()],
        worktree_root: None,
        issue_identifier: "SYM-102".into(),
        branch_name: "feature/sym-102".into(),
        repo_path: None,
        recall_workspace_root: None,
        base_ref: None,
        agent: "implementer".into(),
        model: Some("openai/gpt-5.5".into()),
        effort: None,
        prompt: "Implement SYM-102".into(),
        permission_policy: PermissionPolicy::Reject,
    };

    let started = opencode::StdioOpenCodeLauncher
        .launch(&spec)
        .await
        .expect("mock OMP project repo launch");

    assert_eq!(started.session_id, "omp-session-project-repo");
    assert!(started.process_id.is_some());
}

#[tokio::test]
async fn mocked_omp_acp_launch_prepares_issue_worktree_before_spawn() {
    let dir = tempfile::tempdir().expect("tempdir");
    let command = dir.path().join("mock-omp-acp.py");
    fs::write(
        &command,
        r#"#!/usr/bin/env python3
import json, sys
for line in sys.stdin:
    msg = json.loads(line)
    method = msg.get("method")
    if method == "initialize":
        print(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{"protocolVersion":1}}), flush=True)
    elif method == "session/new":
        print(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{"sessionId":"omp-session-worktree"}}), flush=True)
    elif method == "session/prompt":
        print(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{"ok":True}}), flush=True)
        break
"#,
    )
    .expect("write mock");
    fs::set_permissions(&command, fs::Permissions::from_mode(0o755)).expect("chmod");
    let worktree_root = dir.path().join("worktrees");
    let worktree = worktree_root.join("SYM-102");
    let spec = opencode::OpenCodeLaunchSpec {
        provider_mode: RuntimeProviderMode::OmpAcp,
        provider_id: Some("omp-primary".into()),
        command,
        args: Vec::new(),
        cwd: worktree.clone(),
        env_allowlist: vec!["PATH".into()],
        worktree_root: Some(worktree_root),
        issue_identifier: "SYM-102".into(),
        branch_name: "feature/sym-102".into(),
        repo_path: None,
        recall_workspace_root: None,
        base_ref: None,
        agent: "implementer".into(),
        model: None,
        effort: None,
        prompt: "Implement SYM-102".into(),
        permission_policy: PermissionPolicy::Reject,
    };

    let started = opencode::StdioOpenCodeLauncher
        .launch(&spec)
        .await
        .expect("mock OMP launch");

    assert_eq!(started.session_id, "omp-session-worktree");
    assert!(worktree.is_dir());
}

#[tokio::test]
async fn malformed_omp_acp_frame_is_typed_and_cleans_process_tree() {
    let dir = tempfile::tempdir().expect("tempdir");
    let command = dir.path().join("bad-omp-acp.sh");
    fs::write(
        &command,
        "#!/bin/sh\nsleep 30 &\necho '{not-json'\nsleep 30\n",
    )
    .expect("write bad mock");
    fs::set_permissions(&command, fs::Permissions::from_mode(0o755)).expect("chmod");
    let worktree = dir.path().join("worktree");
    fs::create_dir_all(&worktree).expect("worktree");
    let spec = opencode::OpenCodeLaunchSpec {
        provider_mode: RuntimeProviderMode::OmpAcp,
        provider_id: Some("omp-primary".into()),
        command,
        args: Vec::new(),
        cwd: worktree,
        env_allowlist: vec!["PATH".into()],
        worktree_root: None,
        issue_identifier: "SYM-102".into(),
        branch_name: "feature/sym-102".into(),
        repo_path: None,
        recall_workspace_root: None,
        base_ref: None,
        agent: "implementer".into(),
        model: None,
        effort: None,
        prompt: "Implement SYM-102".into(),
        permission_policy: PermissionPolicy::Reject,
    };

    let err = opencode::StdioOpenCodeLauncher
        .launch(&spec)
        .await
        .expect_err("malformed ACP frame fails");

    let opencode::OpenCodeError::AcpSetupFailed {
        reason,
        termination,
        ..
    } = err
    else {
        panic!("expected setup failure");
    };
    assert!(
        reason.contains("malformed_acp_frame") || reason.contains("invalid ACP JSON"),
        "{reason}"
    );
    assert!(termination.term_signal_sent || termination.kill_signal_sent);
    assert!(!termination.still_alive);
}

#[tokio::test]
async fn missing_omp_acp_binary_is_typed_runtime_failure() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = opencode::OpenCodeLaunchSpec {
        provider_mode: RuntimeProviderMode::OmpAcp,
        provider_id: Some("omp-primary".into()),
        command: dir.path().join("missing-omp"),
        args: Vec::new(),
        cwd: dir.path().to_path_buf(),
        env_allowlist: vec!["PATH".into()],
        worktree_root: None,
        issue_identifier: "SYM-102".into(),
        branch_name: "feature/sym-102".into(),
        repo_path: None,
        recall_workspace_root: None,
        base_ref: None,
        agent: "implementer".into(),
        model: None,
        effort: None,
        prompt: "Implement SYM-102".into(),
        permission_policy: PermissionPolicy::Reject,
    };

    let err = opencode::StdioOpenCodeLauncher
        .launch(&spec)
        .await
        .expect_err("missing OMP binary fails");

    let opencode::OpenCodeError::RuntimeFailure { kind, .. } = err else {
        panic!("expected typed runtime failure");
    };
    assert_eq!(kind, RuntimeFailureKind::MissingBinary);
}

#[tokio::test]
async fn unsupported_omp_acp_version_is_typed_and_cleans_process_tree() {
    let dir = tempfile::tempdir().expect("tempdir");
    let command = dir.path().join("unsupported-omp-acp.py");
    fs::write(
        &command,
        r#"#!/usr/bin/env python3
import json, sys, time
line = sys.stdin.readline()
msg = json.loads(line)
print(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{"protocolVersion":2}}), flush=True)
time.sleep(30)
"#,
    )
    .expect("write mock");
    fs::set_permissions(&command, fs::Permissions::from_mode(0o755)).expect("chmod");
    let spec = opencode::OpenCodeLaunchSpec {
        provider_mode: RuntimeProviderMode::OmpAcp,
        provider_id: Some("omp-primary".into()),
        command,
        args: Vec::new(),
        cwd: dir.path().to_path_buf(),
        env_allowlist: vec!["PATH".into()],
        worktree_root: None,
        issue_identifier: "SYM-102".into(),
        branch_name: "feature/sym-102".into(),
        repo_path: None,
        recall_workspace_root: None,
        base_ref: None,
        agent: "implementer".into(),
        model: None,
        effort: None,
        prompt: "Implement SYM-102".into(),
        permission_policy: PermissionPolicy::Reject,
    };

    let err = opencode::StdioOpenCodeLauncher
        .launch(&spec)
        .await
        .expect_err("unsupported OMP ACP version fails");

    let opencode::OpenCodeError::AcpSetupFailed {
        reason,
        termination,
        ..
    } = err
    else {
        panic!("expected setup failure");
    };
    assert!(reason.contains("unsupported_omp_version"), "{reason}");
    assert!(!termination.still_alive);
}

#[tokio::test]
async fn runtime_api_surfaces_omp_acp_provider_mode_and_session_telemetry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "issue-102", "SYM-102"))
        .await
        .expect("issue");
    let mut session = test_session("symphony", "issue-102", "omp-session-1", "/tmp/sym-102");
    session.provider_mode = RuntimeProviderMode::OmpAcp;
    session.provider_id = Some("omp-primary".into());
    session.process_id = Some(std::process::id());
    session.runtime_failure_kind = Some(RuntimeFailureKind::ProviderAuthUnavailable);
    session.acp_frame_count = 5;
    session.session_evidence_refs = vec!["sdk:one".into()];
    session.silence_observed = true;
    store
        .upsert_opencode_session(&session)
        .await
        .expect("session");

    let api = RuntimeDashboardApi::from_store(&config, &store)
        .await
        .expect("dashboard api");
    let detail = api
        .issue_detail("symphony", "issue-102")
        .expect("issue detail")
        .expect("issue exists");
    let session = &detail.opencode_sessions[0];

    assert_eq!(session.provider_mode, RuntimeProviderMode::OmpAcp);
    assert_eq!(session.provider_id.as_deref(), Some("omp-primary"));
    assert_eq!(
        session.runtime_failure_kind,
        Some(RuntimeFailureKind::ProviderAuthUnavailable)
    );
    assert_eq!(session.acp_frame_count, 5);
    assert_eq!(session.session_evidence_refs, ["sdk:one"]);
    assert!(session.silence_observed);

    let running = &api.aggregate().projects[0].running_issues[0];
    assert_eq!(running.provider_mode, Some(RuntimeProviderMode::OmpAcp));
    assert_eq!(running.provider_id.as_deref(), Some("omp-primary"));
    assert_eq!(running.process_id, Some(std::process::id()));
    assert_eq!(running.process_alive, Some(true));
    assert_eq!(running.lifecycle_stage, Some(LifecycleStage::Running));
    assert_eq!(
        running.runtime_failure_kind,
        Some(RuntimeFailureKind::ProviderAuthUnavailable)
    );
    assert_eq!(running.acp_frame_count, 5);
    assert_eq!(running.session_evidence_refs, ["sdk:one"]);
    assert!(running.silence_observed);
}

#[tokio::test]
async fn stdio_launcher_uses_acp_json_rpc_session_lifecycle() {
    let dir = tempfile::tempdir().expect("tempdir");
    let transcript_path = dir.path().join("acp-transcript.jsonl");
    let script_path = write_fake_acp_script(dir.path(), &transcript_path);
    let worktree = dir.path().join("worktree");
    let recall_workspace_root = dir.path().join("recall-root");
    let spec = opencode::OpenCodeLaunchSpec {
        provider_mode: RuntimeProviderMode::OpenCodeAcp,
        provider_id: None,
        command: script_path,
        args: Vec::new(),
        cwd: worktree.clone(),
        env_allowlist: Vec::new(),
        worktree_root: None,
        issue_identifier: "SYM-200".into(),
        branch_name: "feature/sym-200".into(),
        repo_path: None,
        recall_workspace_root: Some(recall_workspace_root.clone()),
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
                !transcript.contains("OpenCode ACP session id"),
                "{transcript}"
            );
            assert!(!transcript.contains("Run OpenCode ACP"), "{transcript}");
            assert!(
                transcript.contains(&format!(
                    r#""SYMPHONY_RECALL_WORKSPACE_ROOT": "{}""#,
                    recall_workspace_root.display()
                )),
                "{transcript}"
            );
            assert!(
                transcript.contains(&format!(
                    r#""SYMPHONY_ISSUE_WORKTREE": "{}""#,
                    worktree.display()
                )),
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
async fn stdio_launcher_kills_process_tree_when_setup_fails_before_session_attachment() {
    let dir = tempfile::tempdir().expect("tempdir");
    let child_pid_path = dir.path().join("child.pid");
    let script_path = write_failing_acp_setup_script(dir.path(), &child_pid_path);
    let worktree = dir.path().join("worktree");
    fs::create_dir_all(&worktree).expect("worktree");
    let spec = opencode::OpenCodeLaunchSpec {
        provider_mode: RuntimeProviderMode::OpenCodeAcp,
        provider_id: None,
        command: script_path,
        args: Vec::new(),
        cwd: worktree.clone(),
        env_allowlist: Vec::new(),
        worktree_root: None,
        issue_identifier: "SYM-209".into(),
        branch_name: "feature/sym-209".into(),
        repo_path: None,
        recall_workspace_root: Some(worktree),
        base_ref: None,
        agent: "build".into(),
        model: Some("openai/gpt-5.5".into()),
        effort: Some("high".into()),
        prompt: "Full Linear issue spec with eval defaults".into(),
        permission_policy: PermissionPolicy::Reject,
    };

    let error = opencode::StdioOpenCodeLauncher
        .launch(&spec)
        .await
        .expect_err("setup failure must be returned");

    let child_pid = fs::read_to_string(&child_pid_path)
        .expect("child pid")
        .trim()
        .parse::<u32>()
        .expect("child pid number");
    match error {
        opencode::OpenCodeError::AcpSetupFailed {
            issue_identifier,
            process_id,
            session_id,
            reason,
            termination,
        } => {
            assert_eq!(issue_identifier, "SYM-209");
            assert!(process_id.is_some());
            assert_eq!(session_id, None);
            assert!(reason.contains("setup failed before session attachment"));
            assert_eq!(termination.root_process_id, process_id.expect("root pid"));
            assert!(termination.descendant_process_ids.contains(&child_pid));
            assert!(termination.term_signal_sent);
            assert!(!termination.still_alive);
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert!(!Path::new(&format!("/proc/{child_pid}")).exists());
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
  "eval_results": [{"suite": "cargo test", "passed": true, "failure_fingerprint": null, "details": "ok", "evidence_ref": "recall:evidence:abc123"}],
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
        Some("recall:evidence:abc123")
    );
}

#[tokio::test]
async fn handoff_sidecar_normalizes_null_string_fields_from_opencode() {
    let dir = tempfile::tempdir().expect("tempdir");
    let worktree = dir.path().join("worktree");
    let sidecar_dir = worktree.join(".symphony");
    fs::create_dir_all(&sidecar_dir).expect("sidecar dir");
    fs::write(
        sidecar_dir.join("opencode-handoff.json"),
        r#"{
  "session_id": "ses-null-fields",
  "lifecycle_stages": ["running", "eval", "handoff", "completed"],
  "subagents": ["build"],
  "eval_results": [{"suite": null, "passed": true, "failure_fingerprint": null, "details": null, "evidence_ref": null}],
  "changed_files": ["crates/recall-storage/src/migration.rs:1-20"],
  "git": {"branch": "feature/mne-226", "head_sha": "2da52e40f3a0e75e4d4fdf28dc79d06c6ad49979", "pr_url": null, "worktree_path": null},
  "risks": [],
  "stop_reason": "accepted"
}"#,
    )
    .expect("handoff fixture");

    let handoff = opencode::StdioOpenCodeLauncher
        .latest_handoff(&test_session(
            "recall",
            "issue-mne-226",
            "ses-null-fields",
            &worktree,
        ))
        .await
        .expect("OpenCode null string fields should be normalized")
        .expect("handoff present");

    assert_eq!(handoff.eval_results[0].suite, "opencode-evaluation");
    assert!(handoff.eval_results[0].failure_fingerprint.is_none());
    assert!(handoff.eval_results[0].details.is_none());
    assert!(handoff.eval_results[0].evidence_ref.is_none());
    let git = handoff.git.expect("git evidence");
    assert_eq!(git.worktree_path, worktree.display().to_string());
    assert!(git.pr_url.is_none());
}

#[tokio::test]
async fn handoff_sidecar_ignores_harmless_status_marker() {
    let dir = tempfile::tempdir().expect("tempdir");
    let worktree = dir.path().join("worktree");
    let sidecar_dir = worktree.join(".symphony");
    fs::create_dir_all(&sidecar_dir).expect("sidecar dir");
    fs::write(
        sidecar_dir.join("opencode-handoff.json"),
        r#"{"status":"implemented","session_id":"ses-markdown-fields","lifecycle_stages":["running","handoff","completed"],"subagents":[],"eval_results":[],"changed_files":[],"git":null,"risks":[],"stop_reason":{"type":"success"}}"#,
    )
    .expect("handoff fixture");

    let handoff = opencode::StdioOpenCodeLauncher
        .latest_handoff(&test_session(
            "symphony",
            "issue-status-marker",
            "ses-markdown-fields",
            &worktree,
        ))
        .await
        .expect("status marker should not make a valid handoff malformed")
        .expect("handoff present");

    assert_eq!(handoff.session_id, "ses-markdown-fields");
    assert!(matches!(
        handoff.stop_reason,
        opencode::OpenCodeStopReason::Success
    ));
}

#[tokio::test]
async fn handoff_sidecar_normalizes_opencode_acp_shape() {
    let dir = tempfile::tempdir().expect("tempdir");
    let worktree = dir.path().join("worktree");
    let sidecar_dir = worktree.join(".symphony");
    fs::create_dir_all(&sidecar_dir).expect("sidecar dir");
    fs::write(
        sidecar_dir.join("opencode-handoff.json"),
        r#"{
  "status": "completed",
  "session_id": "ses_12805fdb5ffevWVN8GOADoEkPu",
  "task_id": "324e3b33-b003-443a-8ca7-ce90253acad7",
  "subtask_id": "e71bb704-790b-40a6-80cc-03613d7158ba",
  "repair_fingerprint": "git_closure_unverified",
  "lifecycle_stages": [
    "repair_intake",
    "base_fetch",
    "merge_origin_master",
    "conflict_resolution",
    "verification",
    "review",
    "evaluation",
    "failure_analysis",
    "git_closure_repair",
    "commit",
    "push",
    "handoff"
  ],
  "subagents_used": ["integrator", "rust-engineer", "evaluator"],
  "eval_results": {
    "outcome": "accept",
    "details": [
      "Final evaluator accepted after verification, review, clean pushed branch, and commit/push closure.",
      "The branch merges cleanly into origin/master."
    ],
    "verification_ref": "2b8782b1-9705-4fab-adc9-873fd96cede3",
    "evaluation_ref": "mne188-final-evaluation-accept-pushed"
  },
  "changed_files": [
    "crates/recall-storage/src/graph/revision.rs:160-257",
    "crates/recall-runtime/src/service/code_graph_reads/readiness.rs:16-104"
  ],
  "validation": [
    {"command":"cargo fmt --all -- --check","status":"passed"},
    {"command":"cargo nextest run -p recall-runtime graph","status":"passed"}
  ],
  "git": {
    "branch": "feature/mne-188-p1-graph-summary-and-bounded-graph-query-projections",
    "worktree_path": "/home/agent/.symphony/workspaces/opencode/recall/MNE-188",
    "base_branch": "master",
    "base_sha": "1723b3cad4d65607cb5b645350d0541897effb4e",
    "remote": "origin",
    "head_sha": "661f44082359591b3a820c55464ae32a3e62c1ce",
    "previous_head_sha": "481fe2611342a90a7d7efeac159ad10f0ad28804",
    "pushed": true,
    "remote_ref": "origin/feature/mne-188-p1-graph-summary-and-bounded-graph-query-projections",
    "remote_head_sha": "661f44082359591b3a820c55464ae32a3e62c1ce",
    "status": "clean_tracking_origin",
    "evidence_ref": "506015a7-71cb-4a14-9f40-e792b1e312e4"
  },
  "risks": [],
  "stop_reason": "accepted"
}"#,
    )
    .expect("handoff fixture");

    let handoff = opencode::StdioOpenCodeLauncher
        .latest_handoff(&test_session(
            "recall",
            "issue-mne-188",
            "ses_12805fdb5ffevWVN8GOADoEkPu",
            &worktree,
        ))
        .await
        .expect("OpenCode ACP-shaped handoff should parse")
        .expect("handoff present");

    assert_eq!(handoff.session_id, "ses_12805fdb5ffevWVN8GOADoEkPu");
    assert_eq!(
        handoff.subagents,
        vec!["integrator", "rust-engineer", "evaluator"]
    );
    assert_eq!(
        handoff.lifecycle_stages,
        vec![
            OpenCodeStage::Running,
            OpenCodeStage::Running,
            OpenCodeStage::Running,
            OpenCodeStage::Running,
            OpenCodeStage::Eval,
            OpenCodeStage::Review,
            OpenCodeStage::Eval,
            OpenCodeStage::Running,
            OpenCodeStage::Running,
            OpenCodeStage::Running,
            OpenCodeStage::Running,
            OpenCodeStage::Handoff,
        ]
    );
    assert_eq!(handoff.eval_results.len(), 1);
    assert_eq!(handoff.eval_results[0].suite, "opencode-evaluation");
    assert!(handoff.eval_results[0].passed);
    assert_eq!(
        handoff.eval_results[0].evidence_ref.as_deref(),
        Some("mne188-final-evaluation-accept-pushed")
    );
    assert!(
        handoff.eval_results[0].details.as_ref().is_some_and(
            |details| details.contains("The branch merges cleanly into origin/master.")
        )
    );
    assert_eq!(
        handoff.git.expect("git evidence").head_sha.as_deref(),
        Some("661f44082359591b3a820c55464ae32a3e62c1ce")
    );
    assert!(matches!(
        handoff.stop_reason,
        opencode::OpenCodeStopReason::Success
    ));
}

#[tokio::test]
async fn handoff_sidecar_normalizes_provider_neutral_blocker_reasons() {
    let dir = tempfile::tempdir().expect("tempdir");
    let worktree = dir.path().join("worktree");
    let sidecar_dir = worktree.join(".symphony");
    fs::create_dir_all(&sidecar_dir).expect("sidecar dir");
    fs::write(
        sidecar_dir.join("opencode-handoff.json"),
        r#"{
  "session_id":"ses-provider-blocker",
  "lifecycle_stages":["running","failed"],
  "subagents_used":["build"],
  "eval_results":[],
  "changed_files":[],
  "git":null,
  "risks":["provider unavailable"],
  "stop_reason":{"reason":"provider_blocker","message":"provider quota exhausted"}
}"#,
    )
    .expect("handoff fixture");

    let handoff = opencode::StdioOpenCodeLauncher
        .latest_handoff(&test_session(
            "symphony",
            "issue-provider-blocker",
            "ses-provider-blocker",
            &worktree,
        ))
        .await
        .expect("provider-neutral blocker handoff should parse")
        .expect("handoff present");

    assert!(matches!(
        handoff.stop_reason,
        opencode::OpenCodeStopReason::ProviderBlocker { ref message }
            if message == "provider quota exhausted"
    ));
}

#[tokio::test]
async fn handoff_sidecar_normalizes_unsupported_omp_surface_as_non_success() {
    let dir = tempfile::tempdir().expect("tempdir");
    let worktree = dir.path().join("worktree");
    let sidecar_dir = worktree.join(".symphony");
    fs::create_dir_all(&sidecar_dir).expect("sidecar dir");
    fs::write(
        sidecar_dir.join("opencode-handoff.json"),
        r#"{
  "session_id":"ses-unsupported",
  "lifecycle_stages":["running","failed"],
  "subagents":[],
  "eval_results":[],
  "changed_files":[],
  "git":null,
  "risks":["unsupported OMP tool surface"],
  "stop_reason":"unsupported_omp_surface",
  "message":"OMP provider requested an unsupported tool surface"
}"#,
    )
    .expect("handoff fixture");

    let handoff = opencode::StdioOpenCodeLauncher
        .latest_handoff(&test_session(
            "symphony",
            "issue-unsupported",
            "ses-unsupported",
            &worktree,
        ))
        .await
        .expect("unsupported OMP surface handoff should parse")
        .expect("handoff present");

    assert!(matches!(
        handoff.stop_reason,
        opencode::OpenCodeStopReason::UnsupportedOmpSurface { ref message }
            if message.contains("unsupported tool surface")
    ));
}

#[tokio::test]
async fn handoff_sidecar_fills_missing_git_worktree_path_from_session() {
    let dir = tempfile::tempdir().expect("tempdir");
    let worktree = dir.path().join("worktree");
    let sidecar_dir = worktree.join(".symphony");
    fs::create_dir_all(&sidecar_dir).expect("sidecar dir");
    fs::write(
        sidecar_dir.join("opencode-handoff.json"),
        r#"{
  "session_id":"ses-missing-worktree",
  "lifecycle_stages":["running","handoff"],
  "subagents_used":["rust-engineer"],
  "eval_results":[],
  "changed_files":[],
  "git":{"branch":"feature/sym-77","head_sha":"abc123","pr_url":"https://example.test/pr/77"},
  "risks":[],
  "stop_reason":"completed"
}"#,
    )
    .expect("handoff fixture");

    let handoff = opencode::StdioOpenCodeLauncher
        .latest_handoff(&test_session(
            "symphony",
            "issue-missing-worktree",
            "ses-missing-worktree",
            &worktree,
        ))
        .await
        .expect("missing git worktree_path should be normalized")
        .expect("handoff present");

    assert_eq!(
        handoff.git.expect("git evidence").worktree_path,
        worktree.display().to_string()
    );
}

#[tokio::test]
async fn handoff_sidecar_normalizes_final_review_lifecycle_alias() {
    let dir = tempfile::tempdir().expect("tempdir");
    let worktree = dir.path().join("worktree");
    let sidecar_dir = worktree.join(".symphony");
    fs::create_dir_all(&sidecar_dir).expect("sidecar dir");
    fs::write(
        sidecar_dir.join("opencode-handoff.json"),
        r#"{
  "session_id":"ses-final-review",
  "lifecycle_stages":["running","final_review","final_evaluation","final_handoff","completed"],
  "subagents_used":["code-reviewer","evaluator"],
  "eval_results":{"outcome":"accept","details":"review and evaluation passed"},
  "changed_files":["crates/nervure-types/src/runtime_connector.rs:1-20"],
  "git":{"branch":"feature/nrv-12","head_sha":"9cb9bb9d79e3e7cb4d4c6dd9a7d5a6edd7bc5168","worktree_path":"/tmp/worktree","pushed":true},
  "risks":[],
  "stop_reason":"accepted"
}"#,
    )
    .expect("handoff fixture");

    let handoff = opencode::StdioOpenCodeLauncher
        .latest_handoff(&test_session(
            "nervure",
            "issue-nrv-12",
            "ses-final-review",
            &worktree,
        ))
        .await
        .expect("final review alias should be normalized")
        .expect("handoff present");

    assert_eq!(
        handoff.lifecycle_stages,
        vec![
            OpenCodeStage::Running,
            OpenCodeStage::Review,
            OpenCodeStage::Eval,
            OpenCodeStage::Handoff,
            OpenCodeStage::Completed,
        ]
    );
    assert!(handoff.eval_results[0].passed);
    assert!(matches!(
        handoff.stop_reason,
        opencode::OpenCodeStopReason::Success
    ));
}

#[tokio::test]
async fn malformed_handoff_sidecar_rejects_markdown_acp_fields() {
    let dir = tempfile::tempdir().expect("tempdir");
    let worktree = dir.path().join("worktree");
    let sidecar_dir = worktree.join(".symphony");
    fs::create_dir_all(&sidecar_dir).expect("sidecar dir");
    fs::write(
        sidecar_dir.join("opencode-handoff.json"),
        r#"{"session_id":"ses-markdown-fields","next_action":"continue","lifecycle_stages":["running","handoff","completed"],"subagents":[],"eval_results":[],"changed_files":[],"git":null,"risks":[],"stop_reason":{"type":"success"}}"#,
    )
    .expect("handoff fixture");

    let error = opencode::StdioOpenCodeLauncher
        .latest_handoff(&test_session(
            "symphony",
            "issue-markdown-fields",
            "ses-markdown-fields",
            &worktree,
        ))
        .await
        .expect_err("Markdown ACP fields must not be accepted in the sidecar JSON");

    assert!(
        matches!(error, opencode::OpenCodeError::MalformedHandoff(_)),
        "unexpected error: {error:?}"
    );
    assert!(
        error.to_string().contains("unknown field `next_action`"),
        "{error}"
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
        provider_mode: RuntimeProviderMode::OpenCodeAcp,
        provider_id: None,
        command: script_path,
        args: Vec::new(),
        cwd: worktree,
        env_allowlist: Vec::new(),
        worktree_root: Some(worktree_root),
        issue_identifier: "SYM-201".into(),
        branch_name: "feature/sym-201".into(),
        repo_path: None,
        recall_workspace_root: Some(dir.path().to_path_buf()),
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
        provider_mode: RuntimeProviderMode::OpenCodeAcp,
        provider_id: None,
        command: script_path,
        args: Vec::new(),
        cwd: worktree.clone(),
        env_allowlist: Vec::new(),
        worktree_root: None,
        issue_identifier: "SYM-202".into(),
        branch_name: "feature/sym-202".into(),
        repo_path: None,
        recall_workspace_root: Some(dir.path().to_path_buf()),
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
async fn stdio_launcher_continues_existing_session_from_dirty_resumable_worktree() {
    let dir = tempfile::tempdir().expect("tempdir");
    let transcript_path = dir.path().join("acp-transcript.jsonl");
    let script_path = write_fake_acp_resume_script(dir.path(), &transcript_path);
    let repo = dir.path().join("repo");
    let worktree_root = dir.path().join("worktrees");
    let worktree = worktree_root.join("SYM-203");
    fs::create_dir_all(&repo).expect("repo dir");
    run_git(&repo, ["init", "-b", "main"]);
    fs::write(repo.join("README.md"), "base\n").expect("readme");
    run_git(&repo, ["add", "README.md"]);
    run_git(&repo, ["commit", "-m", "initial"]);
    run_git(
        &repo,
        [
            "worktree",
            "add",
            "-b",
            "feature/sym-203",
            worktree.to_str().expect("worktree path utf8"),
            "main",
        ],
    );
    fs::write(worktree.join("in-flight.txt"), "uncommitted work\n").expect("dirty file");
    let spec = opencode::OpenCodeLaunchSpec {
        provider_mode: RuntimeProviderMode::OpenCodeAcp,
        provider_id: None,
        command: script_path,
        args: Vec::new(),
        cwd: worktree.clone(),
        env_allowlist: Vec::new(),
        worktree_root: Some(worktree_root),
        issue_identifier: "SYM-203".into(),
        branch_name: "feature/sym-203".into(),
        repo_path: Some(repo),
        recall_workspace_root: Some(dir.path().to_path_buf()),
        base_ref: Some("main".into()),
        agent: "build".into(),
        model: Some("openai/gpt-5.5".into()),
        effort: Some("high".into()),
        prompt: "Original prompt must not be replayed on continue".into(),
        permission_policy: PermissionPolicy::Reject,
    };
    let mut session = test_session("symphony", "issue-203", "ses-existing", &worktree);
    session.process_id = None;
    let launcher = opencode::StdioOpenCodeLauncher;

    let continued = launcher
        .continue_session(&spec, &session, "continue after process restart")
        .await
        .expect("continue existing session from dirty worktree");

    assert_eq!(continued.session_id, "ses-existing");
    assert!(continued.process_id.is_some());
    for _ in 0..50 {
        if let Ok(transcript) = fs::read_to_string(&transcript_path)
            && transcript.contains(r#""method": "session/prompt""#)
        {
            assert!(
                transcript.contains(r#""method": "session/resume""#),
                "{transcript}"
            );
            assert!(
                transcript.contains("MCP tool-schema loop guard"),
                "{transcript}"
            );
            assert!(
                transcript.contains(
                    "After two failed calls to the same MCP method for schema/validation reasons"
                ),
                "{transcript}"
            );
            assert!(
                transcript.contains("Delegated review/evaluator subagent contract"),
                "{transcript}"
            );
            assert!(
                transcript.contains(
                    "Delegated reviewer/evaluator subagents are read-only unless the issue spec explicitly says otherwise"
                ),
                "{transcript}"
            );
            assert!(
                !transcript.contains(r#""method": "session/new""#),
                "{transcript}"
            );
            assert!(
                !transcript.contains("Original prompt must not be replayed"),
                "{transcript}"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    panic!(
        "ACP continuation prompt was not observed; transcript={:?}",
        fs::read_to_string(transcript_path)
    );
}

#[tokio::test]
async fn stdio_launcher_continues_dirty_same_issue_worktree_after_branch_title_drift() {
    let dir = tempfile::tempdir().expect("tempdir");
    let transcript_path = dir.path().join("acp-transcript.jsonl");
    let script_path = write_fake_acp_resume_script(dir.path(), &transcript_path);
    let repo = dir.path().join("repo");
    let worktree_root = dir.path().join("worktrees");
    let worktree = worktree_root.join("NRV-48");
    fs::create_dir_all(&repo).expect("repo dir");
    run_git(&repo, ["init", "-b", "main"]);
    fs::write(repo.join("README.md"), "base\n").expect("readme");
    run_git(&repo, ["add", "README.md"]);
    run_git(&repo, ["commit", "-m", "initial"]);
    run_git(
        &repo,
        [
            "worktree",
            "add",
            "-b",
            "feature/nrv-48-define-runtime-context-quarantine-policy-profile",
            worktree.to_str().expect("worktree path utf8"),
            "main",
        ],
    );
    fs::write(worktree.join("in-flight.txt"), "uncommitted work\n").expect("dirty file");
    let spec = opencode::OpenCodeLaunchSpec {
        provider_mode: RuntimeProviderMode::OpenCodeAcp,
        provider_id: None,
        command: script_path,
        args: Vec::new(),
        cwd: worktree.clone(),
        env_allowlist: Vec::new(),
        worktree_root: Some(worktree_root),
        issue_identifier: "NRV-48".into(),
        branch_name: "feature/nrv-48-implement-runtime-context-efficiency-attestation-and".into(),
        repo_path: Some(repo),
        recall_workspace_root: Some(dir.path().to_path_buf()),
        base_ref: Some("main".into()),
        agent: "build".into(),
        model: Some("openai/gpt-5.5".into()),
        effort: Some("high".into()),
        prompt: "Original prompt must not be replayed on continue".into(),
        permission_policy: PermissionPolicy::Reject,
    };
    let mut session = test_session("nervure", "issue-48", "ses-existing", &worktree);
    session.process_id = None;
    let launcher = opencode::StdioOpenCodeLauncher;

    let continued = launcher
        .continue_session(&spec, &session, "continue after issue title changed")
        .await
        .expect("continue existing session from dirty same-issue worktree");

    assert_eq!(continued.session_id, "ses-existing");
    assert!(continued.process_id.is_some());
    for _ in 0..50 {
        if let Ok(transcript) = fs::read_to_string(&transcript_path)
            && transcript.contains(r#""method": "session/prompt""#)
        {
            assert!(
                transcript.contains(r#""method": "session/resume""#),
                "{transcript}"
            );
            assert!(
                transcript.contains(r#""method": "session/prompt""#),
                "{transcript}"
            );
            assert_eq!(
                git_output(&worktree, ["branch", "--show-current"]).trim(),
                "feature/nrv-48-define-runtime-context-quarantine-policy-profile"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    panic!(
        "ACP continuation prompt was not observed; transcript={:?}",
        fs::read_to_string(transcript_path)
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
async fn live_omp_acp_smoke_starts_session_when_explicitly_enabled() {
    if std::env::var("SYMPHONY_LIVE_OMP_ACP").ok().as_deref() != Some("1") {
        eprintln!(
            "skipped live OMP ACP smoke: SYMPHONY_LIVE_OMP_ACP is not set to 1 (requires SYMPHONY_LIVE_OMP_COMMAND=/absolute/path/to/omp)"
        );
        return;
    }
    let command = std::env::var("SYMPHONY_LIVE_OMP_COMMAND")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .expect("SYMPHONY_LIVE_OMP_COMMAND must be set to the OMP executable path");
    assert!(
        command.is_absolute(),
        "SYMPHONY_LIVE_OMP_COMMAND must be an absolute path: {}",
        command.display()
    );
    let metadata = fs::metadata(&command).expect("OMP command path must exist");
    assert!(metadata.is_file(), "OMP command path must be a file");
    assert!(
        metadata.permissions().mode() & 0o111 != 0,
        "OMP command path must be executable"
    );
    let args = std::env::var("SYMPHONY_LIVE_OMP_ACP_ARGS")
        .ok()
        .map(|value| {
            value
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|args| !args.is_empty())
        .unwrap_or_else(|| vec!["acp".into()]);
    live_omp_version_if_available(&command).await;

    let dir = tempfile::tempdir().expect("tempdir");
    let mut child = tokio::process::Command::new(&command)
        .args(&args)
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn live OMP ACP");
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut stdout = tokio::io::BufReader::new(stdout);

    let initialized = live_omp_acp_request(
        &mut stdin,
        &mut stdout,
        1,
        "initialize",
        serde_json::json!({
            "protocolVersion": 1,
            "client": {"name": "symphony-live-smoke", "version": "0"},
            "agent": "implementer",
            "model": null,
            "providerId": "live-omp"
        }),
    )
    .await;
    let protocol_version = initialized
        .get("protocolVersion")
        .or_else(|| initialized.get("protocol_version"))
        .and_then(serde_json::Value::as_u64);
    assert_eq!(protocol_version, Some(1));

    let created = live_omp_acp_request(
        &mut stdin,
        &mut stdout,
        2,
        "session/new",
        serde_json::json!({
            "cwd": dir.path(),
            "title": "SYM-105 live OMP ACP smoke",
            "agent": "implementer",
            "model": null,
            "mcpServers": []
        }),
    )
    .await;
    let session_id = created["sessionId"].as_str().expect("session id");
    assert!(
        !session_id.trim().is_empty(),
        "session id must not be empty"
    );
    let refs = live_omp_session_evidence_refs(&created);
    assert!(
        refs.total <= 8,
        "session evidence refs must be bounded: {:?}",
        refs.bounded
    );
    assert!(
        refs.bounded
            .iter()
            .all(|reference| !reference.trim().is_empty()),
        "session evidence refs must not be empty: {:?}",
        refs.bounded
    );
    eprintln!(
        "live OMP ACP smoke created session {session_id}; session_evidence_refs={}",
        refs.total
    );

    let _ = tokio::io::AsyncWriteExt::shutdown(&mut stdin).await;
    drop(stdin);
    drop(stdout);
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
}

async fn live_omp_version_if_available(command: &Path) {
    let mut version = tokio::process::Command::new(command);
    version
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    match tokio::time::timeout(Duration::from_secs(5), version.output()).await {
        Ok(Ok(output)) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let version_line = stdout
                .lines()
                .chain(stderr.lines())
                .find(|line| !line.trim().is_empty())
                .unwrap_or("<empty version output>");
            eprintln!("live OMP ACP smoke version: {version_line}");
        }
        Ok(Ok(output)) => {
            eprintln!(
                "live OMP ACP smoke version unavailable: --version exited with {}",
                output.status
            );
        }
        Ok(Err(error)) => {
            eprintln!("live OMP ACP smoke version unavailable: {error}");
        }
        Err(_) => {
            eprintln!("live OMP ACP smoke version unavailable: --version timed out after 5s");
        }
    }
}

async fn live_omp_acp_request(
    stdin: &mut tokio::process::ChildStdin,
    stdout: &mut tokio::io::BufReader<tokio::process::ChildStdout>,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        })
        .to_string()
            + "\n";
        tokio::io::AsyncWriteExt::write_all(stdin, request.as_bytes())
            .await
            .expect("write live OMP ACP request");
        tokio::io::AsyncWriteExt::flush(stdin)
            .await
            .expect("flush live OMP ACP request");

        for _ in 0..200 {
            let mut line = String::new();
            let read = tokio::io::AsyncBufReadExt::read_line(stdout, &mut line)
                .await
                .expect("read live OMP ACP response");
            assert!(read != 0, "OMP ACP stdout closed before {method} response");
            let message: serde_json::Value =
                serde_json::from_str(&line).expect("live OMP ACP json response");
            if message.get("id").and_then(serde_json::Value::as_u64) == Some(id) {
                if let Some(error) = message.get("error") {
                    panic!("live OMP ACP {method} failed: {error}");
                }
                return message
                    .get("result")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
            }
        }

        panic!("live OMP ACP response for {method} was not observed");
    })
    .await
    .unwrap_or_else(|_| panic!("live OMP ACP {method} timed out after 10s"))
}

struct LiveOmpSessionEvidenceRefs {
    total: usize,
    bounded: Vec<String>,
}

fn live_omp_session_evidence_refs(value: &serde_json::Value) -> LiveOmpSessionEvidenceRefs {
    let mut refs = LiveOmpSessionEvidenceRefs {
        total: 0,
        bounded: Vec::new(),
    };

    for reference in [
        "sessionEvidenceRefs",
        "sdkSessionEvidenceRefs",
        "evidenceRefs",
    ]
    .into_iter()
    .flat_map(|field| {
        value
            .get(field)
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
    })
    .filter_map(serde_json::Value::as_str)
    {
        refs.total += 1;
        if refs.bounded.len() < 8 {
            refs.bounded.push(reference.to_owned());
        }
    }

    refs
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
        provider_mode: RuntimeProviderMode::OpenCodeAcp,
        provider_id: None,
        command: script_path,
        args: Vec::new(),
        cwd: worktree.clone(),
        env_allowlist: Vec::new(),
        worktree_root: Some(dir.path().join("worktrees")),
        issue_identifier: "SYM-200".into(),
        branch_name: "feature/sym-200".into(),
        repo_path: Some(repo.clone()),
        recall_workspace_root: Some(repo.clone()),
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
        provider_mode: RuntimeProviderMode::OpenCodeAcp,
        provider_id: None,
        command: PathBuf::from("/bin/false"),
        args: Vec::new(),
        cwd: nested.clone(),
        env_allowlist: Vec::new(),
        worktree_root: Some(root.clone()),
        issue_identifier: "SYM/200".into(),
        branch_name: "feature/sym-200".into(),
        repo_path: Some(dir.path().join("repo")),
        recall_workspace_root: Some(dir.path().join("repo")),
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
            acp_frame_count: 0,
            session_evidence_refs: Vec::new(),
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
            acp_frame_count: 0,
            session_evidence_refs: Vec::new(),
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

pub(super) async fn seed_opencode_session_tree(path: &std::path::Path) {
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

pub(super) async fn seed_stale_active_opencode_session_tree(path: &std::path::Path) {
    seed_opencode_session_tree(path).await;
    let stale_ms = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_millis() as i64)
        - (11 * 60 * 1000);
    let database = libsql::Builder::new_local(path.display().to_string())
        .build()
        .await
        .expect("build opencode db");
    let conn = database.connect().expect("connect opencode db");
    conn.execute(
        "UPDATE session SET time_created = ?1, time_updated = ?1",
        libsql::params![stale_ms],
    )
    .await
    .expect("stale sessions");
    conn.execute(
        "UPDATE message SET time_created = ?1, time_updated = ?1",
        libsql::params![stale_ms],
    )
    .await
    .expect("stale messages");
    conn.execute(
        "UPDATE part SET time_created = ?1, time_updated = ?1",
        libsql::params![stale_ms],
    )
    .await
    .expect("stale parts");
    conn.execute(
        "UPDATE session_message SET time_created = ?1, time_updated = ?1",
        libsql::params![stale_ms],
    )
    .await
    .expect("stale session messages");
    conn.execute(
        "UPDATE todo SET status = 'pending', time_created = ?1, time_updated = ?1",
        libsql::params![stale_ms],
    )
    .await
    .expect("stale active todo");
}

pub(super) async fn seed_opencode_provider_auth_error_session(path: &std::path::Path) {
    seed_opencode_session_tree(path).await;
    let database = libsql::Builder::new_local(path.display().to_string())
        .build()
        .await
        .expect("build opencode db");
    let conn = database.connect().expect("connect opencode db");
    conn.execute("DELETE FROM message WHERE session_id = 'ses-root'", ())
        .await
        .expect("clear messages");
    conn.execute("DELETE FROM part WHERE session_id = 'ses-root'", ())
        .await
        .expect("clear parts");
    conn.execute(
        r#"
        INSERT INTO message (id, session_id, time_created, time_updated, data)
        VALUES ('msg-provider-auth', 'ses-root', 3000, 3001, ?1)
        "#,
        libsql::params![serde_json::json!({
            "role": "assistant",
            "time": {"created": 3000, "completed": 3001},
            "error": {
                "name": "ProviderAuthError",
                "data": {
                    "providerID": "openai",
                    "message": "OpenAI API key is missing. Pass it using the 'apiKey' parameter or the OPENAI_API_KEY environment variable."
                }
            }
        })
        .to_string()],
    )
    .await
    .expect("insert provider auth error");
}

pub(super) async fn seed_opencode_recovered_provider_auth_session(path: &std::path::Path) {
    seed_opencode_provider_auth_error_session(path).await;
    let database = libsql::Builder::new_local(path.display().to_string())
        .build()
        .await
        .expect("build opencode db");
    let conn = database.connect().expect("connect opencode db");
    conn.execute(
        r#"
        UPDATE session
        SET tokens_input = 972,
            tokens_output = 249,
            tokens_reasoning = 0,
            tokens_cache_read = 31232,
            time_updated = 4001
        WHERE id = 'ses-root'
        "#,
        (),
    )
    .await
    .expect("update recovered session");
    conn.execute(
        r#"
        INSERT INTO message (id, session_id, time_created, time_updated, data)
        VALUES ('msg-recovered', 'ses-root', 4000, 4001, ?1)
        "#,
        libsql::params![
            serde_json::json!({
                "role": "assistant",
                "time": {"created": 4000, "completed": 4001},
                "tokens": {
                    "total": 32453,
                    "input": 972,
                    "output": 249,
                    "reasoning": 0,
                    "cache": {"read": 31232, "write": 0}
                },
                "finish": "tool-calls"
            })
            .to_string()
        ],
    )
    .await
    .expect("insert recovered message");
    conn.execute(
        r#"
        INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
        VALUES ('part-recovered', 'msg-recovered', 'ses-root', 4000, 4001, ?1)
        "#,
        libsql::params![
            serde_json::json!({
                "type": "tool",
                "tool": "oc_bash",
                "state": {"status": "completed"},
                "title": "git status"
            })
            .to_string()
        ],
    )
    .await
    .expect("insert recovered part");
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
async fn stdio_launcher_rejects_clean_existing_worktree_on_wrong_branch() {
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
        provider_mode: RuntimeProviderMode::OpenCodeAcp,
        provider_id: None,
        command: script_path,
        args: Vec::new(),
        cwd: worktree.clone(),
        env_allowlist: Vec::new(),
        worktree_root: Some(root),
        issue_identifier: "SYM-204".into(),
        branch_name: "feature/sym-204".into(),
        repo_path: Some(repo),
        recall_workspace_root: Some(dir.path().join("repo")),
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
        .expect_err("clean wrong-branch worktree must be rejected");

    assert!(
        matches!(error, opencode::OpenCodeError::InvalidWorktree(ref message) if message.contains("is on branch stale-branch but expected feature/sym-204")),
        "{error:?}"
    );
    assert_eq!(
        git_output(&worktree, ["branch", "--show-current"]).trim(),
        "stale-branch"
    );
}

#[tokio::test]
async fn stdio_launcher_rejects_existing_worktree_on_wrong_branch() {
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
        provider_mode: RuntimeProviderMode::OpenCodeAcp,
        provider_id: None,
        command: PathBuf::from("/bin/false"),
        args: Vec::new(),
        cwd: worktree.clone(),
        env_allowlist: Vec::new(),
        worktree_root: Some(root),
        issue_identifier: "SYM-205".into(),
        branch_name: "feature/sym-205".into(),
        repo_path: Some(repo),
        recall_workspace_root: Some(dir.path().join("repo")),
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
        matches!(error, opencode::OpenCodeError::InvalidWorktree(ref message) if message.contains("is on branch stale-branch but expected feature/sym-205")),
        "{error:?}"
    );
    assert!(
        matches!(error, opencode::OpenCodeError::InvalidWorktree(ref message) if message.contains("dirty or untracked files") && message.contains("dirty.txt")),
        "{error:?}"
    );
    assert_eq!(
        git_output(&worktree, ["branch", "--show-current"]).trim(),
        "stale-branch"
    );
}
