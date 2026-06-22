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
        project
            .recall
            .as_ref()
            .expect("recall config")
            .workspace_root,
        PathBuf::from("/home/agent/proj/symphony")
    );
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
async fn omp_acp_provider_config_loads_capabilities_without_changing_opencode_runtime() {
    let configured = valid_config_toml().replace(
        "[projects.eval]\n",
        r#"[[projects.omp_acp_providers]]
id = "omp-primary"
command = "/usr/local/bin/omp"
args = ["acp"]
cwd = "issue_worktree"
env_allowlist = ["PATH", "HOME"]
agent = "build"
model = "openai/gpt-5.5"
effort = "high"
live_smoke = true

[projects.omp_acp_providers.capabilities]
acp_stdio = true
hook_evidence = true
sdk_session_evidence = true
rpc_secondary_mode = true
inverse_bridge_reference = true

[projects.eval]
"#,
    );

    let config = RootConfig::from_toml_str(&configured).expect("OMP ACP provider config");
    let project = config.project("symphony").expect("project");
    assert_eq!(
        project.opencode.command,
        PathBuf::from("/usr/local/bin/opencode")
    );
    assert_eq!(project.opencode.args, vec!["acp"]);

    let provider = project.omp_acp_providers.first().expect("OMP ACP provider");
    assert_eq!(provider.id, "omp-primary");
    assert_eq!(provider.command, PathBuf::from("/usr/local/bin/omp"));
    assert_eq!(provider.args, vec!["acp"]);
    assert_eq!(provider.env_allowlist, vec!["PATH", "HOME"]);
    assert_eq!(provider.agent.as_deref(), Some("build"));
    assert_eq!(provider.model.as_deref(), Some("openai/gpt-5.5"));
    assert_eq!(provider.effort.as_deref(), Some("high"));
    assert!(provider.live_smoke);
    assert!(provider.capabilities.acp_stdio);
    assert!(provider.capabilities.hook_evidence);
    assert!(provider.capabilities.sdk_session_evidence);
    assert!(provider.capabilities.rpc_secondary_mode);
    assert!(provider.capabilities.inverse_bridge_reference);
}

#[tokio::test]
async fn omp_acp_provider_config_rejects_invalid_definitions() {
    let configured = valid_config_toml().replace(
        "[projects.eval]\n",
        r#"[[projects.omp_acp_providers]]
id = "omp-primary"
command = "/usr/local/bin/omp"
args = ["acp"]
cwd = "issue_worktree"
env_allowlist = ["PATH"]
agent = "build"
model = "openai/gpt-5.5"
effort = "high"

[projects.omp_acp_providers.capabilities]
acp_stdio = true
hook_evidence = true
sdk_session_evidence = true
rpc_secondary_mode = false
inverse_bridge_reference = true

[projects.eval]
"#,
    );

    let unknown_field = configured.replace(
        "agent = \"build\"",
        "agent = \"build\"\nmutation = \"implicit\"",
    );
    let err =
        RootConfig::from_toml_str(&unknown_field).expect_err("unknown provider field rejected");
    assert!(err.to_string().contains("mutation"), "{err}");

    let invalid_capability = configured.replace("acp_stdio = true", "acp_stdio = false");
    let err = RootConfig::from_toml_str(&invalid_capability)
        .expect_err("ACP stdio capability must be explicit");
    assert!(err.to_string().contains("capabilities.acp_stdio"), "{err}");

    let duplicate = configured.replace(
        "[projects.eval]\n",
        r#"[[projects.omp_acp_providers]]
id = "omp-primary"
command = "/usr/local/bin/omp"
cwd = "project_repo"

[projects.omp_acp_providers.capabilities]
acp_stdio = true
hook_evidence = false
sdk_session_evidence = false
rpc_secondary_mode = false
inverse_bridge_reference = true

[projects.eval]
"#,
    );
    let err = RootConfig::from_toml_str(&duplicate).expect_err("duplicate provider id rejected");
    assert!(
        err.to_string().contains("duplicate omp_acp_providers"),
        "{err}"
    );
}

#[tokio::test]
async fn recall_workspace_config_validates_global_project_workspace_root() {
    let invalid = valid_config_toml().replace(
        "workspace_root = \"/home/agent/proj/symphony\"",
        "workspace_root = \"relative/project\"",
    );
    let err = RootConfig::from_toml_str(&invalid).expect_err("relative workspace root rejected");

    assert!(err.to_string().contains("recall.workspace_root"), "{err}");
}

#[tokio::test]
async fn opencode_storage_config_loads_and_validates_archive_paths() {
    let configured = valid_config_toml().replace(
        "[[projects]]\n",
        "[opencode_storage]\ndatabase_path = \"/home/agent/.local/share/opencode/opencode.db\"\narchive_root = \"/home/agent/recall-benchmark-runs/symphony-opencode-session-archives\"\n\n[[projects]]\n",
    );
    let config = RootConfig::from_toml_str(&configured).expect("opencode storage config");
    let storage = config.opencode_storage.expect("storage config");

    assert_eq!(
        storage.database_path,
        PathBuf::from("/home/agent/.local/share/opencode/opencode.db")
    );
    assert_eq!(
        storage.archive_root,
        PathBuf::from("/home/agent/recall-benchmark-runs/symphony-opencode-session-archives")
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
                            "labels": { "nodes": [{ "name": "symphony" }] },
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
                                            "title": "Accepted upstream implementation",
                                            "description": "Recall workspace_id: `workspace-upstream`\nAccepted artifact: `docs/upstream.md`",
                                            "state": { "name": "Done" },
                                            "branchName": "feature/sym-99-upstream",
                                            "url": "https://linear.example/SYM-99",
                                            "comments": {
                                                "nodes": [
                                                    {
                                                        "body": "## OpenCode Handoff Accepted\nRecall task_id: `task-upstream`\n### Changed Files\n- `src/upstream.rs:1-10`",
                                                        "createdAt": "2026-06-10T00:02:00Z"
                                                    }
                                                ]
                                            }
                                        }
                                    }
                                ]
                            },
                            "inverseRelations": {
                                "nodes": [
                                    {
                                        "type": "blocks",
                                        "issue": {
                                            "id": "inverse-blocker-1",
                                            "identifier": "SYM-98",
                                            "title": "Open upstream implementation",
                                            "description": null,
                                            "state": { "name": "In Progress" },
                                            "branchName": null,
                                            "url": "https://linear.example/SYM-98",
                                            "comments": { "nodes": [] }
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
                            "inverseRelations": { "nodes": [] },
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
    assert_eq!(issues[0].labels, vec!["symphony"]);
    assert_eq!(
        issues[0].blocked_by[0].identifier.as_deref(),
        Some("SYM-99")
    );
    assert_eq!(
        issues[0].blocked_by[1].identifier.as_deref(),
        Some("SYM-98")
    );
    assert_eq!(issues[0].upstream_context.len(), 1);
    let upstream = &issues[0].upstream_context[0];
    assert_eq!(upstream.identifier, "SYM-99");
    assert_eq!(upstream.title, "Accepted upstream implementation");
    assert_eq!(upstream.state, "Done");
    assert_eq!(
        upstream.recall_workspace_ids,
        vec!["workspace-upstream".to_string()]
    );
    assert_eq!(upstream.recall_task_ids, vec!["task-upstream".to_string()]);
    assert_eq!(
        upstream.accepted_artifacts,
        vec![
            "docs/upstream.md".to_string(),
            "src/upstream.rs:1-10".to_string(),
        ]
    );
    assert!(
        upstream
            .handoff_summary
            .as_deref()
            .is_some_and(|summary| summary.contains("OpenCode Handoff Accepted"))
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
    assert!(
        requests[0]["query"]
            .as_str()
            .expect("candidate query")
            .contains("comments(last: 20, orderBy: createdAt)")
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
async fn linear_client_finds_open_managed_issue_by_fingerprint() {
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let project = config.project("symphony").expect("project");
    let client = RecordingLinearClient::new(vec![
        linear_issue("done-managed", "SYM-10", "Done", Some(1))
            .with_description("<!-- symphony:managed-self-bug fingerprint=sym-self-1 -->"),
        linear_issue("open-managed", "SYM-11", "Todo", Some(1))
            .with_description("<!-- symphony:managed-self-bug fingerprint=sym-self-1 -->"),
    ]);

    let issue = client
        .find_managed_issue(project, "sym-self-1")
        .await
        .expect("find managed issue")
        .expect("managed issue");

    assert_eq!(issue.id, "open-managed");
}

#[tokio::test]
async fn linear_graphql_client_creates_managed_issue_in_configured_project() {
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let project = config.project("symphony").expect("project");
    let mut created = linear_issue_node_json("managed-1", "SYM-200", "Todo", 2);
    created["description"] = serde_json::json!(
        "panic evidence\n\n<!-- symphony:managed-self-bug fingerprint=sym-self-2 -->"
    );
    let transport = RecordingGraphqlTransport::new(vec![
        serde_json::json!({
            "data": {
                "teams": {
                    "nodes": [{
                        "id": "team-symphony",
                        "states": { "nodes": [{ "id": "state-todo", "name": "Todo" }] }
                    }]
                }
            }
        }),
        serde_json::json!({
            "data": {
                "issueCreate": {
                    "success": true,
                    "issue": created
                }
            }
        }),
    ]);
    let client = LinearGraphqlClient::new(
        "https://linear.example/graphql",
        "linear-token",
        transport.clone(),
    );

    let issue = client
        .create_managed_issue(
            project,
            ManagedLinearIssueCreate {
                source_issue_id: "source-issue".into(),
                fingerprint: "sym-self-2".into(),
                title: "Managed self bug".into(),
                description: "panic evidence".into(),
                priority: 2,
                state: ManagedLinearIssueState::Todo,
                project_milestone_id: Some("milestone-1".into()),
                label_ids: vec!["label-symphony".into()],
            },
        )
        .await
        .expect("create managed issue");

    assert_eq!(issue.identifier, "SYM-200");
    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["variables"]["teamKey"], "SYM");
    let input = &requests[1]["variables"]["input"];
    assert_eq!(input["teamId"], "team-symphony");
    assert_eq!(input["projectId"], "07df87ce-4e93-4d2c-a73d-84aee1f27e07");
    assert_eq!(input["stateId"], "state-todo");
    assert_eq!(input["projectMilestoneId"], "milestone-1");
    assert_eq!(input["labelIds"], serde_json::json!(["label-symphony"]));
    assert!(
        input["description"]
            .as_str()
            .expect("description")
            .contains("fingerprint=sym-self-2")
    );
}

#[tokio::test]
async fn linear_managed_issue_creation_accepts_sdk_extracted_response_shapes() {
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let project = config.project("symphony").expect("project");
    let mut created = linear_issue_node_json("managed-sdk", "SYM-202", "Backlog", 1);
    created["description"] =
        serde_json::json!("sdk evidence\n\n<!-- symphony:managed-self-bug fingerprint=sym-sdk -->");
    let transport = RecordingGraphqlTransport::new(vec![
        serde_json::json!({
            "nodes": [{
                "id": "team-symphony",
                "states": { "nodes": [{ "id": "state-backlog", "name": "Backlog" }] }
            }]
        }),
        serde_json::json!({
            "success": true,
            "issue": created
        }),
    ]);
    let client = LinearGraphqlClient::new(
        "https://linear.example/graphql",
        "linear-token",
        transport.clone(),
    );

    let issue = client
        .create_managed_issue(
            project,
            ManagedLinearIssueCreate {
                source_issue_id: "source-issue".into(),
                fingerprint: "sym-sdk".into(),
                title: "Managed SDK self bug".into(),
                description: "sdk evidence".into(),
                priority: 1,
                state: ManagedLinearIssueState::Backlog,
                project_milestone_id: None,
                label_ids: Vec::new(),
            },
        )
        .await
        .expect("create managed issue from sdk-shaped responses");

    assert_eq!(issue.identifier, "SYM-202");
    let requests = transport.requests();
    assert_eq!(
        requests[1]["variables"]["input"]["stateId"],
        "state-backlog"
    );
}

#[tokio::test]
async fn linear_graphql_client_creates_relation_and_uses_related_for_self_deadlock() {
    let transport = RecordingGraphqlTransport::new(vec![
        serde_json::json!({ "data": { "issueRelationCreate": { "success": true } } }),
        serde_json::json!({ "data": { "issueRelationCreate": { "success": true } } }),
    ]);
    let client = LinearGraphqlClient::new(
        "https://linear.example/graphql",
        "linear-token",
        transport.clone(),
    );

    client
        .create_issue_relation(
            "source-issue",
            "managed-issue",
            ManagedLinearRelation::Blocks,
        )
        .await
        .expect("relation");
    client
        .create_issue_relation(
            "managed-issue",
            "managed-issue",
            ManagedLinearRelation::Blocks,
        )
        .await
        .expect("self relation");

    let requests = transport.requests();
    assert_eq!(requests[0]["variables"]["type"], "blocks");
    assert_eq!(requests[1]["variables"]["type"], "related");
}

#[tokio::test]
async fn duplicate_managed_issue_reuse_records_occurrence_comment() {
    let client = RecordingLinearClient::new(vec![
        linear_issue("managed-duplicate", "SYM-201", "Todo", Some(1))
            .with_description("<!-- symphony:managed-self-bug fingerprint=sym-self-3 -->"),
    ]);
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let project = config.project("symphony").expect("project");

    let existing = client
        .find_managed_issue(project, "sym-self-3")
        .await
        .expect("find")
        .expect("existing managed issue");
    client
        .record_issue_evidence(
            &existing.id,
            LinearIssueEvidence {
                kind: "duplicate_occurrence".into(),
                body: "second occurrence evidence".into(),
            },
        )
        .await
        .expect("record duplicate occurrence");

    assert_eq!(client.evidence().len(), 1);
    assert_eq!(client.evidence()[0].0, "managed-duplicate");
    assert_eq!(client.evidence()[0].1.kind, "duplicate_occurrence");
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
async fn issue_runtime_store_does_not_persist_linear_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");

    let database = libsql::Builder::new_local(db_path.display().to_string())
        .build()
        .await
        .expect("open database");
    let conn = database.connect().expect("connect");
    let mut rows = conn
        .query("PRAGMA table_info(issues)", ())
        .await
        .expect("pragma");
    let mut columns = Vec::new();
    while let Some(row) = rows.next().await.expect("row") {
        columns.push(row.get::<String>(1).expect("column name"));
    }

    assert!(
        !columns.iter().any(|column| column == "state"),
        "Linear issue state must remain in Linear, not SQLite: {columns:?}"
    );
}

#[tokio::test]
async fn migration_removes_legacy_issue_state_without_losing_sessions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let database = libsql::Builder::new_local(db_path.display().to_string())
        .build()
        .await
        .expect("open database");
    let conn = database.connect().expect("connect");
    conn.execute_batch(
        r#"
        CREATE TABLE schema_migrations (
            id TEXT PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        INSERT INTO schema_migrations (id) VALUES ('001_runtime_state');

        CREATE TABLE projects (
            project_id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            enabled INTEGER NOT NULL,
            lifecycle_stage TEXT NOT NULL,
            cleanup_status TEXT NOT NULL,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        INSERT INTO projects (project_id, name, enabled, lifecycle_stage, cleanup_status)
        VALUES ('symphony', 'Symphony', 1, 'running', 'clean');

        CREATE TABLE issues (
            project_id TEXT NOT NULL,
            issue_id TEXT NOT NULL,
            identifier TEXT NOT NULL,
            title TEXT NOT NULL,
            state TEXT NOT NULL,
            lifecycle_stage TEXT NOT NULL,
            blocker_json TEXT,
            failure_json TEXT,
            git_ref_json TEXT,
            cleanup_status TEXT NOT NULL,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (project_id, issue_id),
            FOREIGN KEY (project_id) REFERENCES projects(project_id) ON DELETE CASCADE
        );
        INSERT INTO issues (
            project_id, issue_id, identifier, title, state, lifecycle_stage, cleanup_status
        )
        VALUES ('symphony', 'issue-1', 'SYM-1', 'Legacy issue', 'In Progress', 'running', 'clean');

        CREATE TABLE opencode_sessions (
            project_id TEXT NOT NULL,
            issue_id TEXT NOT NULL,
            session_id TEXT NOT NULL,
            agent TEXT NOT NULL,
            model TEXT,
            worktree_path TEXT NOT NULL,
            process_id INTEGER,
            lifecycle_stage TEXT NOT NULL,
            stage TEXT NOT NULL,
            active_agent TEXT,
            active_model TEXT,
            message_count INTEGER NOT NULL,
            todo_count INTEGER NOT NULL,
            part_count INTEGER NOT NULL,
            token_count INTEGER NOT NULL,
            cost_micros INTEGER NOT NULL,
            subagent_count INTEGER NOT NULL,
            eval_stage TEXT,
            lifecycle_marker TEXT,
            last_event TEXT,
            silence_observed INTEGER NOT NULL,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (project_id, issue_id, session_id),
            FOREIGN KEY (project_id, issue_id) REFERENCES issues(project_id, issue_id) ON DELETE CASCADE
        );
        INSERT INTO opencode_sessions (
            project_id, issue_id, session_id, agent, worktree_path, lifecycle_stage, stage,
            message_count, todo_count, part_count, token_count, cost_micros, subagent_count,
            silence_observed
        )
        VALUES (
            'symphony', 'issue-1', 'session-1', 'build', '/tmp/SYM-1', 'running', 'running',
            1, 2, 3, 4, 5, 6, 0
        );
        "#,
    )
    .await
    .expect("seed legacy schema");

    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");

    let mut columns = conn
        .query("PRAGMA table_info(issues)", ())
        .await
        .expect("pragma");
    while let Some(row) = columns.next().await.expect("row") {
        assert_ne!(row.get::<String>(1).expect("column name"), "state");
    }
    let issue = store
        .issue("symphony", "issue-1")
        .await
        .expect("query issue")
        .expect("issue preserved");
    assert_eq!(issue.lifecycle_stage, LifecycleStage::Running);
    let session = store
        .opencode_session("symphony", "issue-1", "session-1")
        .await
        .expect("query session")
        .expect("session preserved");
    assert_eq!(session.session_id, "session-1");
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
                provider_mode: RuntimeProviderMode::OpenCodeAcp,
                provider_id: None,
                agent: "build".into(),
                model: None,
                worktree_path: "/home/agent/.symphony/workspaces/opencode/symphony/SYM-25".into(),
                process_id: None,
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
                runtime_failure_kind: None,
                acp_frame_count: 0,
                session_evidence_refs: Vec::new(),
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
async fn opencode_sessions_for_issue_orders_by_runtime_update_not_session_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "runtime-order", "SYM-161"))
        .await
        .expect("issue");

    let stale_worktree = dir.path().join("SYM-161-stale");
    let fresh_worktree = dir.path().join("SYM-161-fresh");
    let mut stale = test_session("symphony", "runtime-order", "zz-stale", &stale_worktree);
    stale.lifecycle_stage = LifecycleStage::Failed;
    stale.stage = OpenCodeStage::Failed;
    store
        .upsert_opencode_session(stale)
        .await
        .expect("stale session");
    store
        .upsert_opencode_session(test_session(
            "symphony",
            "runtime-order",
            "aa-fresh",
            &fresh_worktree,
        ))
        .await
        .expect("fresh session");

    let sessions = store
        .opencode_sessions_for_issue("symphony", "runtime-order")
        .await
        .expect("sessions");

    assert_eq!(
        sessions.last().expect("latest runtime session").session_id,
        "aa-fresh"
    );
}

#[tokio::test]
async fn dashboard_api_orders_active_opencode_session_before_stale_failures() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");

    let mut issue = test_issue("symphony", "mixed-sessions", "SYM-233");
    issue.lifecycle_stage = LifecycleStage::Running;
    store.upsert_issue(issue).await.expect("issue");

    let failed_worktree = dir.path().join("SYM-233-failed");
    let running_worktree = dir.path().join("SYM-233-running");
    let mut failed = test_session("symphony", "mixed-sessions", "zz-failed", &failed_worktree);
    failed.lifecycle_stage = LifecycleStage::Failed;
    failed.stage = OpenCodeStage::Failed;
    failed.last_event = Some("failed:missing_handoff_sidecar".into());
    store
        .upsert_opencode_session(failed)
        .await
        .expect("failed session");

    let mut running = test_session(
        "symphony",
        "mixed-sessions",
        "aa-running",
        &running_worktree,
    );
    running.lifecycle_stage = LifecycleStage::Running;
    running.stage = OpenCodeStage::Running;
    running.last_event = Some("opencode_db_updated:123".into());
    store
        .upsert_opencode_session(running)
        .await
        .expect("running session");

    let api = RuntimeDashboardApi::from_store(&config, &store)
        .await
        .expect("dashboard api");
    let _project = api
        .project_drilldown("symphony")
        .expect("project endpoint")
        .expect("project exists");
    let issue = api
        .issue_detail("symphony", "mixed-sessions")
        .expect("issue endpoint")
        .expect("issue exists");
    let card = api
        .aggregate()
        .projects
        .iter()
        .find(|project| project.project_id == "symphony")
        .expect("project card");

    assert_eq!(issue.opencode_sessions[0].opencode_session_id, "aa-running");
    assert_eq!(
        issue.opencode_sessions[0].last_event.as_deref(),
        Some("opencode_db_updated:123")
    );
    assert_eq!(issue.opencode_sessions[1].opencode_session_id, "zz-failed");
    assert_eq!(card.runner_health, "active");
    assert_eq!(
        card.running_issues[0].session_id.as_deref(),
        Some("aa-running")
    );
    assert_eq!(
        card.running_issues[0].last_event.as_deref(),
        Some("opencode_db_updated:123")
    );
}

#[tokio::test]
async fn active_opencode_sessions_excludes_terminal_sessions_for_metrics_polling() {
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
    for issue_id in ["done", "running", "starting"] {
        store
            .upsert_issue(test_issue("symphony", issue_id, format!("SYM-{issue_id}")))
            .await
            .expect("issue");
    }

    let mut completed = test_session(
        "symphony",
        "done",
        "completed-session",
        dir.path().join("done"),
    );
    completed.lifecycle_stage = LifecycleStage::Completed;
    completed.stage = OpenCodeStage::Completed;
    store
        .upsert_opencode_session(completed)
        .await
        .expect("completed session");

    let running = test_session(
        "symphony",
        "running",
        "running-session",
        dir.path().join("running"),
    );
    store
        .upsert_opencode_session(running)
        .await
        .expect("running session");

    let mut starting = test_session(
        "symphony",
        "starting",
        "starting-session",
        dir.path().join("starting"),
    );
    starting.lifecycle_stage = LifecycleStage::Blocked;
    starting.stage = OpenCodeStage::Starting;
    store
        .upsert_opencode_session(starting)
        .await
        .expect("starting session");

    let sessions = store
        .active_opencode_sessions()
        .await
        .expect("active sessions");
    let session_ids = sessions
        .iter()
        .map(|session| session.session_id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(session_ids, vec!["running-session", "starting-session"]);
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

    let mut completed = test_issue("symphony", "done", "SYM-1");
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

    let running = test_issue("symphony", "running", "SYM-2");
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

    let mut blocked = test_issue("symphony", "blocked", "SYM-3");
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
async fn sqlite_store_reads_canceled_terminal_issue_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store
        .upsert_project(ProjectStateRecord {
            project_id: "nervure".into(),
            name: "Nervure".into(),
            enabled: true,
            lifecycle_stage: LifecycleStage::Running,
            cleanup_status: CleanupStatus::Clean,
        })
        .await
        .expect("project");

    let mut canceled = test_issue("nervure", "canceled", "NRV-8");
    canceled.lifecycle_stage = LifecycleStage::Canceled;
    canceled.cleanup_status = CleanupStatus::Complete;
    store.upsert_issue(canceled).await.expect("canceled issue");

    let issues = store
        .issues_for_project("nervure")
        .await
        .expect("issue rows");

    assert_eq!(issues[0].lifecycle_stage, LifecycleStage::Canceled);
}

#[tokio::test]
async fn dashboard_treats_canceled_issues_as_terminal_history_not_parked_work() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");

    let mut canceled = test_issue("symphony", "canceled", "SYM-8");
    canceled.lifecycle_stage = LifecycleStage::Canceled;
    canceled.cleanup_status = CleanupStatus::Complete;
    store.upsert_issue(canceled).await.expect("canceled issue");
    store
        .mark_project_liveness_poll(
            "symphony",
            RuntimeLivenessStatus::NoEligibleIssues,
            "candidate scan found no eligible issues",
            2,
            0,
            true,
        )
        .await
        .expect("liveness");

    let api = RuntimeDashboardApi::from_store(&config, &store)
        .await
        .expect("dashboard api");
    let project = api
        .project_drilldown("symphony")
        .expect("project endpoint")
        .expect("project");

    assert!(project.active_issues.is_empty());
    assert_eq!(project.history_issues.len(), 1);
    let card = api
        .aggregate()
        .projects
        .iter()
        .find(|card| card.project_id == "symphony")
        .expect("project card");
    assert_eq!(card.active_count, 0);
    assert_eq!(card.parked_count, 0);
    assert_eq!(card.runner_health, "idle");
}

#[tokio::test]
async fn dashboard_api_snapshots_aggregate_project_drilldown_and_issue_detail() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");

    let mut repair = test_issue("symphony", "repair", "SYM-91");
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

    let mut provider_blocked = test_issue("symphony", "provider", "SYM-92");
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

    let mut completed = test_issue("symphony", "done", "SYM-93");
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
  "totals": {
    "project_count": 1,
    "enabled_project_count": 1,
    "running_issue_count": 1,
    "available_sessions": 1,
    "max_sessions": 2,
    "running_tokens": 4096,
    "running_cached_tokens": 0,
    "recorded_tokens": 4096,
    "running_cost_micros": 123456,
    "recorded_cost_micros": 123456
  },
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
      "liveness": {
        "status": "inactive_runtime",
        "reason": "runtime has not reported a poll for this enabled project",
        "primary_reason_code": "active_opencode_session",
        "primary_reason_detail": "an OpenCode session is actively executing",
        "last_poll_at": null,
        "last_successful_candidate_scan_at": null,
        "capacity": {
          "max_sessions": 2,
          "running_sessions": 1,
          "available_sessions": 1
        }
      },
      "cleanup_status": "clean",
      "running_tokens": 4096,
      "running_cached_tokens": 0,
      "recorded_tokens": 4096,
      "running_cost_micros": 123456,
      "recorded_cost_micros": 123456,
      "running_issues": [
        {
          "project_id": "symphony",
          "project_name": "Symphony",
          "issue_id": "repair",
          "identifier": "SYM-91",
          "title": "Test issue",
          "display_status": "repair loop",
          "session_id": "oc-repair",
          "provider_mode": "open_code_acp",
          "provider_id": null,
          "process_id": null,
          "process_alive": null,
          "lifecycle_stage": "running",
          "stage": "eval",
          "agent": "build",
          "model": null,
          "active_agent": "evaluator",
          "active_model": "gpt-5",
          "token_count": 4096,
          "cached_token_count": 0,
          "cost_micros": 123456,
          "subagents_used": 2,
          "running_tool_count": 0,
          "pending_tool_count": 0,
          "todo_count": 1,
          "started_at_ms": null,
          "duration_ms": null,
          "last_event": "eval_failed:clippy-needless-collect",
          "runtime_failure_kind": null,
          "acp_frame_count": 0,
          "session_evidence_refs": [],
          "silence_observed": false,
          "worktree_path": "/home/agent/.symphony/workspaces/opencode/symphony/SYM-91"
        }
      ],
      "self_defect_routes": []
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
    assert!(aggregate_json.contains(r#""running_cost_micros": 123456"#));
    assert!(issue_json.contains(r#""cost_micros": 123456"#));

    let ui_aggregate =
        symphony::api::runtime_api_json_response(&config, &store, "/api/dashboard/ui")
            .await
            .expect("ui aggregate response");
    let ui_project =
        symphony::api::runtime_api_json_response(&config, &store, "/api/projects/symphony/ui")
            .await
            .expect("ui project response");
    let ui_issue = symphony::api::runtime_api_json_response(
        &config,
        &store,
        "/api/projects/symphony/issues/repair/ui",
    )
    .await
    .expect("ui issue response");
    let events = symphony::api::runtime_api_json_response(&config, &store, "/api/dashboard/events")
        .await
        .expect("dashboard event stream");

    assert_eq!(ui_aggregate.status, 200);
    assert_eq!(ui_project.status, 200);
    assert_eq!(ui_issue.status, 200);
    assert_eq!(events.status, 200);
    assert_eq!(events.content_type, "text/event-stream; charset=utf-8");
    assert!(
        ui_aggregate
            .body
            .contains(r#""polling_fallback_endpoint":"/api/dashboard/ui""#)
    );
    assert!(
        ui_aggregate
            .body
            .contains(r#""live_events_endpoint":"/api/dashboard/events""#)
    );
    assert!(ui_project.body.contains(r#""active_issues""#));
    assert!(ui_aggregate.body.contains(r#""running_cached_tokens":0"#));
    assert!(ui_aggregate.body.contains(r#""duration_ms":null"#));
    assert!(
        ui_issue
            .body
            .contains(r#""opencode_session_id":"oc-repair""#)
    );
    assert!(ui_issue.body.contains(r#""cached_token_count":0"#));
    assert!(ui_issue.body.contains(r#""duration_ms":null"#));
    assert!(events.body.starts_with("event: dashboard.snapshot\ndata: "));
    assert!(events.body.contains(r#""running_cached_tokens":0"#));
    assert!(!ui_aggregate.body.contains("cost_micros"));
    assert!(!ui_project.body.contains("cost_micros"));
    assert!(!ui_issue.body.contains("cost_micros"));
    assert!(!events.body.contains("cost_micros"));
}

#[tokio::test]
async fn dashboard_api_json_routes_aggregate_project_and_issue_paths() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    let issue = test_issue("symphony", "api-issue", "SYM-94");
    store.upsert_issue(issue).await.expect("issue");

    let aggregate = symphony::api::runtime_api_json_response(&config, &store, "/api/dashboard")
        .await
        .expect("aggregate response");
    let project =
        symphony::api::runtime_api_json_response(&config, &store, "/api/projects/symphony")
            .await
            .expect("project response");
    let issue = symphony::api::runtime_api_json_response(
        &config,
        &store,
        "/api/projects/symphony/issues/api-issue",
    )
    .await
    .expect("issue response");
    let missing =
        symphony::api::runtime_api_json_response(&config, &store, "/api/projects/missing")
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
async fn dashboard_api_only_rejects_root_and_legacy_ui_paths_without_html() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "api-only", "SYM-101"))
        .await
        .expect("issue");

    let root = symphony::api::runtime_api_json_response(&config, &store, "/")
        .await
        .expect("root response");
    let project_ui_path =
        symphony::api::runtime_api_json_response(&config, &store, "/projects/symphony")
            .await
            .expect("legacy project path response");
    let issue_ui_path = symphony::api::runtime_api_json_response(
        &config,
        &store,
        "/projects/symphony/issues/api-only",
    )
    .await
    .expect("legacy issue path response");
    let api = symphony::api::runtime_api_json_response(&config, &store, "/api/dashboard")
        .await
        .expect("json api");

    assert_eq!(root.status, 404);
    assert_eq!(project_ui_path.status, 404);
    assert_eq!(issue_ui_path.status, 404);
    assert_eq!(root.content_type, "application/json");
    assert_eq!(project_ui_path.content_type, "application/json");
    assert_eq!(issue_ui_path.content_type, "application/json");
    assert_eq!(api.status, 200);
    assert_eq!(api.content_type, "application/json");
    assert!(api.body.contains(r#""project_id":"symphony""#));
    assert!(!root.body.contains("<html"));
    assert!(!project_ui_path.body.contains("<html"));
    assert!(!issue_ui_path.body.contains("<html"));
}

#[tokio::test]
async fn dashboard_surfaces_managed_self_defect_routing_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");

    let mut issue = test_issue("symphony", "runtime-held", "SYM-208");
    issue.lifecycle_stage = LifecycleStage::Failed;
    issue.failure = Some(FailureRecord {
        kind: "runtime_defect".into(),
        message: "OpenCode launch failed after Linear transition".into(),
        fingerprint: Some("launch_failed".into()),
        occurrence_count: 2,
    });
    store.upsert_issue(issue).await.expect("issue");
    store
        .record_self_defect_occurrence(&SelfDefectOccurrenceRecord {
            fingerprint: "launch_failed".into(),
            defect_kind: "runtime_defect".into(),
            category: "runtime".into(),
            severity: "p0".into(),
            initial_routing_decision: "managed_self_defect".into(),
            source_project_id: "symphony".into(),
            source_issue_id: "runtime-held".into(),
            source_issue_identifier: "SYM-208".into(),
            source_session_id: Some("ses-failed".into()),
            source_process_id: Some(4242),
            managed_issue_id: "managed-launch-failed".into(),
            managed_issue_identifier: "SYM-62".into(),
            latest_evidence_summary: "launch failure still open\nrelation_mode: blocking".into(),
            relation_mode: SelfDefectRelationMode::Blocking,
        })
        .await
        .expect("self defect");

    let api = RuntimeDashboardApi::from_store(&config, &store)
        .await
        .expect("dashboard api");
    let aggregate_json = serde_json::to_string(&api.aggregate()).expect("aggregate json");
    let project_json = serde_json::to_string(
        &api.project_drilldown("symphony")
            .expect("project endpoint")
            .expect("project exists"),
    )
    .expect("project json");
    let issue = api
        .issue_detail("symphony", "runtime-held")
        .expect("issue endpoint")
        .expect("issue exists");
    let routing = issue.self_defect_routing.as_ref().expect("self routing");

    assert_eq!(routing.managed_bug.issue_id, "managed-launch-failed");
    assert_eq!(routing.managed_bug.identifier, "SYM-62");
    assert_eq!(
        routing.managed_bug.url.as_deref(),
        Some("https://linear.app/issue/SYM-62")
    );
    assert_eq!(routing.source_context.issue_identifier, "SYM-208");
    assert_eq!(
        routing.source_context.session_id.as_deref(),
        Some("ses-failed")
    );
    assert_eq!(routing.source_context.process_id, Some(4242));
    assert_eq!(routing.fingerprint, "launch_failed");
    assert_eq!(routing.severity, "p0");
    assert_eq!(routing.defect_kind, "runtime_defect");
    assert_eq!(routing.occurrence_count, 1);
    assert_eq!(routing.relation_mode, SelfDefectRelationMode::Blocking);
    assert_eq!(routing.next_action, "repair_managed_self_defect");
    assert_eq!(
        routing.suppression_reason.as_deref(),
        Some("source_issue_blocked_by_managed_self_defect")
    );
    assert!(!routing.deadlock_skipped_blocker);
    assert!(aggregate_json.contains(r#""managed_issue_identifier":"SYM-62""#));
    assert!(project_json.contains(r#""self_defect_routing""#));
    assert!(!project_json.contains("latest_evidence_summary"));

    let ui_aggregate =
        symphony::api::runtime_api_json_response(&config, &store, "/api/dashboard/ui")
            .await
            .expect("ui aggregate response");
    let ui_issue = symphony::api::runtime_api_json_response(
        &config,
        &store,
        "/api/projects/symphony/issues/runtime-held/ui",
    )
    .await
    .expect("ui issue response");
    assert!(ui_aggregate.body.contains(r#""self_defect_routes""#));
    assert!(
        ui_aggregate
            .body
            .contains(r#""managed_issue_identifier":"SYM-62""#)
    );
    assert!(ui_issue.body.contains(r#""self_defect_routing""#));
    assert!(
        ui_issue
            .body
            .contains(r#""next_action":"repair_managed_self_defect""#)
    );
}

#[tokio::test]
async fn dashboard_surfaces_related_only_self_defect_and_deadlock_skip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "active-self", "SYM-209"))
        .await
        .expect("issue");
    store
        .record_self_defect_occurrence(&SelfDefectOccurrenceRecord {
            fingerprint: "session_id_mismatch".into(),
            defect_kind: "runtime_defect".into(),
            category: "runtime".into(),
            severity: "p0".into(),
            initial_routing_decision: "managed_self_defect".into(),
            source_project_id: "symphony".into(),
            source_issue_id: "active-self".into(),
            source_issue_identifier: "SYM-209".into(),
            source_session_id: Some("ses-active".into()),
            source_process_id: None,
            managed_issue_id: "managed-session-mismatch".into(),
            managed_issue_identifier: "SYM-209".into(),
            latest_evidence_summary: "runtime mismatch\nrelation_mode: related_only\nskipped_blocker_reason: active_symphony_self_deadlock_prevention".into(),
            relation_mode: SelfDefectRelationMode::RelatedOnly,
        })
        .await
        .expect("self defect");

    let api = RuntimeDashboardApi::from_store(&config, &store)
        .await
        .expect("dashboard api");
    let issue = api
        .issue_detail("symphony", "active-self")
        .expect("issue endpoint")
        .expect("issue exists");
    let routing = issue.self_defect_routing.as_ref().expect("self routing");

    assert_eq!(routing.relation_mode, SelfDefectRelationMode::RelatedOnly);
    assert_eq!(routing.next_action, "monitor_related_self_defect");
    assert_eq!(routing.suppression_reason, None);
    assert_eq!(
        routing.skipped_blocker_reason.as_deref(),
        Some("active_symphony_self_deadlock_prevention")
    );
    assert!(routing.deadlock_skipped_blocker);

    let ui_issue = symphony::api::runtime_api_json_response(
        &config,
        &store,
        "/api/projects/symphony/issues/active-self/ui",
    )
    .await
    .expect("ui issue response");
    assert!(ui_issue.body.contains(r#""relation_mode":"related_only""#));
    assert!(
        ui_issue
            .body
            .contains(r#""skipped_blocker_reason":"active_symphony_self_deadlock_prevention""#)
    );
    assert!(ui_issue.body.contains(r#""deadlock_skipped_blocker":true"#));
}

#[tokio::test]
async fn dashboard_does_not_surface_stale_stop_reason_for_running_issue() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");

    let mut issue = test_issue("symphony", "running-recovered", "SYM-211");
    issue.lifecycle_stage = LifecycleStage::Running;
    issue.failure = Some(FailureRecord {
        kind: "provider_blocker".into(),
        message: "stale OpenCode provider auth failure".into(),
        fingerprint: Some("opencode_providerautherror_api_key_missing".into()),
        occurrence_count: 1,
    });
    store.upsert_issue(issue).await.expect("issue");

    let api = RuntimeDashboardApi::from_store(&config, &store)
        .await
        .expect("dashboard api");
    let issue = api
        .issue_detail("symphony", "running-recovered")
        .expect("issue endpoint")
        .expect("issue exists");

    assert_eq!(issue.lifecycle_stage, LifecycleStage::Running);
    assert_eq!(issue.stop_reason, None);
    assert_eq!(issue.display_status, "running");
}

#[tokio::test]
async fn dashboard_surfaces_classifier_recommendation_without_managed_bug() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "ambiguous", "SYM-210"))
        .await
        .expect("issue");
    store
        .record_self_defect_recommendation(&SelfDefectRecommendationRecord {
            recommendation_id: "recommendation:ambiguous-fp".into(),
            evidence_fingerprint: "ambiguous-fp".into(),
            defect_kind: "runtime_defect".into(),
            defect_category: "handoff".into(),
            confidence: SelfDefectRecommendationConfidence::Medium,
            evidence_refs: vec!["issue:SYM-210".into()],
            recommended_action: "backlog_recommendation".into(),
            rationale: "ambiguous handoff evidence should be reviewed".into(),
            source_project_id: "symphony".into(),
            source_issue_id: "ambiguous".into(),
            source_issue_identifier: "SYM-210".into(),
            source_session_id: Some("ses-ambiguous".into()),
            source_process_id: None,
            occurrence_count: 0,
            first_seen_at: String::new(),
            last_seen_at: String::new(),
        })
        .await
        .expect("recommendation");

    let api = RuntimeDashboardApi::from_store(&config, &store)
        .await
        .expect("dashboard api");
    let issue = api
        .issue_detail("symphony", "ambiguous")
        .expect("issue endpoint")
        .expect("issue exists");
    let routing = issue.self_defect_routing.as_ref().expect("self routing");
    let recommendation = routing
        .classifier_recommendation
        .as_ref()
        .expect("recommendation");

    assert_eq!(routing.managed_bug.identifier, "recommendation-only");
    assert_eq!(routing.fingerprint, "ambiguous-fp");
    assert_eq!(routing.severity, "medium");
    assert_eq!(routing.next_action, "review_classifier_recommendation");
    assert_eq!(recommendation.recommended_action, "backlog_recommendation");
    assert_eq!(
        recommendation.confidence,
        SelfDefectRecommendationConfidence::Medium
    );
    let issue_json = serde_json::to_string(issue).expect("issue json");
    assert!(!issue_json.contains("evidence_refs"));
    assert!(!issue_json.contains("rationale"));
}

#[tokio::test]
async fn dashboard_reuses_self_defect_fingerprint_after_second_occurrence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("runtime.sqlite3");
    let config = RootConfig::from_toml_str(valid_config_toml()).expect("config");
    let store = SqliteStore::open(&db_path).await.expect("open sqlite");
    store.migrate().await.expect("migrate");
    store.reconcile_projects(&config).await.expect("projects");
    store
        .upsert_issue(test_issue("symphony", "first", "SYM-211"))
        .await
        .expect("first issue");
    store
        .upsert_issue(test_issue("symphony", "second", "SYM-212"))
        .await
        .expect("second issue");

    for (issue_id, identifier) in [("first", "SYM-211"), ("second", "SYM-212")] {
        store
            .record_self_defect_occurrence(&SelfDefectOccurrenceRecord {
                fingerprint: "reused-fingerprint".into(),
                defect_kind: "runtime_defect".into(),
                category: "runtime".into(),
                severity: "p1".into(),
                initial_routing_decision: "managed_self_defect".into(),
                source_project_id: "symphony".into(),
                source_issue_id: issue_id.into(),
                source_issue_identifier: identifier.into(),
                source_session_id: None,
                source_process_id: None,
                managed_issue_id: "managed-reused".into(),
                managed_issue_identifier: "SYM-63".into(),
                latest_evidence_summary: format!("{identifier} reused fingerprint"),
                relation_mode: SelfDefectRelationMode::Blocking,
            })
            .await
            .expect("self defect");
    }

    let api = RuntimeDashboardApi::from_store(&config, &store)
        .await
        .expect("dashboard api");
    let first = api
        .issue_detail("symphony", "first")
        .expect("first endpoint")
        .expect("first issue");
    let second = api
        .issue_detail("symphony", "second")
        .expect("second endpoint")
        .expect("second issue");

    assert!(first.self_defect_routing.is_none());
    let routing = second
        .self_defect_routing
        .as_ref()
        .expect("latest source routing");
    assert_eq!(routing.fingerprint, "reused-fingerprint");
    assert_eq!(routing.occurrence_count, 2);
    assert_eq!(routing.source_context.issue_identifier, "SYM-212");
}
