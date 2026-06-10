use std::{fs, path::PathBuf};

use symphony_vnext::{
    api::RuntimeReadModel,
    cli,
    config::RootConfig,
    daemon,
    linear::{LinearBlocker, LinearClient, LinearClientError, LinearIssue, LinearTransition},
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

#[derive(Debug)]
struct RecordingLinearClient {
    issues: Vec<LinearIssue>,
    transitions: std::cell::RefCell<Vec<(String, LinearTransition)>>,
}

impl RecordingLinearClient {
    fn new(issues: Vec<LinearIssue>) -> Self {
        Self {
            issues,
            transitions: std::cell::RefCell::new(Vec::new()),
        }
    }

    fn transitions(&self) -> Vec<(String, LinearTransition)> {
        self.transitions.borrow().clone()
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
