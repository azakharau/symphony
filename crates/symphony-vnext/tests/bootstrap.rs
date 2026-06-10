use std::{fs, path::PathBuf};

use symphony_vnext::{
    api::RuntimeReadModel,
    cli,
    config::RootConfig,
    state::{
        CleanupStatus, FailureRecord, GitRefRecord, IssueStateRecord, LifecycleStage,
        OpenCodeSessionRecord, ProjectStateRecord,
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
                }),
                git_ref: Some(GitRefRecord {
                    branch: "agent-server/opencode-runner-extension".into(),
                    worktree_path: "/home/agent/.symphony/workspaces/codex/symphony/SYM-25".into(),
                    head_sha: None,
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
                lifecycle_stage: LifecycleStage::Running,
                last_event: Some("started".into()),
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

    let read_model = RuntimeReadModel::from_store(&reloaded).expect("read model");
    assert_eq!(read_model.projects[0].project_id, "symphony");
    assert_eq!(
        read_model.projects[0].issues[0].opencode_sessions[0].session_id,
        "oc-session-1"
    );
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
