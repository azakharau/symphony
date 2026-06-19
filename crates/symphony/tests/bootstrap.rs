use std::{
    fs,
    io::{BufRead, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use symphony::{
    api::{RuntimeDashboardApi, RuntimeReadModel},
    cli,
    config::RootConfig,
    daemon,
    linear::{
        LinearBlocker, LinearClient, LinearClientError, LinearGraphqlClient,
        LinearGraphqlTransport, LinearIssue, LinearIssueEvidence, LinearTransition,
        ManagedLinearIssueCreate, ManagedLinearIssueState, ManagedLinearRelation,
    },
    opencode::{
        self, GitClosureEvidence, OpenCodeEvalResult, OpenCodeHandoff, OpenCodeLauncher,
        OpenCodeSessionEvent, OpenCodeStopReason, PermissionPolicy,
    },
    state::{
        BlockerRecord, CleanupStatus, EvalRunRecord, FailureRecord, GitRefRecord, IssueStateRecord,
        LifecycleStage, OpenCodeSessionRecord, OpenCodeStage, OpenCodeStageEventRecord,
        ProjectStateRecord, RuntimeLivenessStatus, SelfDefectOccurrenceRecord,
        SelfDefectRecommendationConfidence, SelfDefectRecommendationRecord, SelfDefectRelationMode,
    },
    storage::SqliteStore,
};

#[path = "bootstrap/config_linear_storage_api.rs"]
mod config_linear_storage_api;
#[path = "bootstrap/dashboard_reason_codes.rs"]
mod dashboard_reason_codes;
#[path = "bootstrap/opencode_runtime.rs"]
mod opencode_runtime;
#[path = "bootstrap/orchestration_basic.rs"]
mod orchestration_basic;
#[path = "bootstrap/orchestration_handoff.rs"]
mod orchestration_handoff;

const fn valid_config_toml() -> &'static str {
    r#"
[server]
host = "127.0.0.1"
port = 4110

[[projects]]
id = "symphony"
name = "Symphony"
enabled = true
workflow_path = "/home/agent/proj/symphony/WORKFLOW.md"
repo_path = "/home/agent/proj/symphony"

[projects.mnemesh]
workspace_root = "/home/agent/proj/symphony"

[projects.branch]
base = "agent-server/opencode-runner-extension"
worktree_root = "/home/agent/.symphony/workspaces/opencode/symphony"

[projects.linear]
team_key = "SYM"
project_id = "07df87ce-4e93-4d2c-a73d-84aee1f27e07"

[projects.opencode]
command = "/usr/local/bin/opencode"
args = ["acp"]
agent = "build"
model = "openai/gpt-5.5"
effort = "high"
permission_policy = "reject"

[projects.eval]
default_suite = "symphony-smoke"

[projects.concurrency]
max_sessions = 2
"#
}

const fn two_project_config_toml() -> &'static str {
    r#"
[server]
host = "127.0.0.1"
port = 4110

[[projects]]
id = "alpha"
name = "Alpha"
enabled = true
workflow_path = "/home/agent/proj/alpha/WORKFLOW.md"
repo_path = "/home/agent/proj/alpha"

[projects.mnemesh]
workspace_root = "/home/agent/proj/alpha"

[projects.branch]
base = "main"
worktree_root = "/home/agent/.symphony/workspaces/opencode/alpha"

[projects.linear]
team_key = "ALPHA"
project_id = "alpha-project"

[projects.opencode]
command = "/usr/local/bin/opencode"
args = ["acp"]
agent = "build"
model = "openai/gpt-5.5"
effort = "high"
permission_policy = "reject"

[projects.eval]
default_suite = "alpha-smoke"

[projects.concurrency]
max_sessions = 1

[[projects]]
id = "symphony"
name = "Symphony"
enabled = true
workflow_path = "/home/agent/proj/symphony/WORKFLOW.md"
repo_path = "/home/agent/proj/symphony"

[projects.mnemesh]
workspace_root = "/home/agent/proj/symphony"

[projects.branch]
base = "agent-server/opencode-runner-extension"
worktree_root = "/home/agent/.symphony/workspaces/opencode/symphony"

[projects.linear]
team_key = "SYM"
project_id = "07df87ce-4e93-4d2c-a73d-84aee1f27e07"

[projects.opencode]
command = "/usr/local/bin/opencode"
args = ["acp"]
agent = "build"
model = "openai/gpt-5.5"
effort = "high"
permission_policy = "reject"

[projects.eval]
default_suite = "symphony-smoke"

[projects.concurrency]
max_sessions = 2
"#
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
        project_milestone: Some(symphony::linear::LinearMilestone {
            id: "test-milestone-id".into(),
            name: "Test Milestone".into(),
        }),
        blocked_by: Vec::new(),
        has_new_owner_answer: false,
        owner_answer_created_at: None,
        created_at: None,
        updated_at: None,
    }
}

fn managed_self_bug(
    id: impl Into<String>,
    identifier: impl Into<String>,
    priority: Option<i64>,
) -> LinearIssue {
    let mut issue = linear_issue(id, identifier, "Todo", priority);
    issue.title = "Symphony self-defect: test".into();
    issue.description = Some("<!-- symphony:managed-self-bug fingerprint=test -->".into());
    issue.project_milestone = None;
    issue
}

fn test_issue(
    project_id: impl Into<String>,
    issue_id: impl Into<String>,
    identifier: impl Into<String>,
) -> IssueStateRecord {
    IssueStateRecord {
        project_id: project_id.into(),
        issue_id: issue_id.into(),
        identifier: identifier.into(),
        title: "Test issue".into(),
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
        process_id: None,
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
        eval_stage: Some("symphony-smoke".into()),
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
            evidence_ref: None,
        }],
        changed_files: vec!["crates/symphony/src/opencode.rs".into()],
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
            evidence_ref: None,
        }],
        changed_files: vec!["crates/symphony/src/daemon.rs".into()],
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
import os
import pathlib
import sys

transcript_path = pathlib.Path({transcript_literal})
cwd = None
config = {{"mode": "build", "model": "opencode/big-pickle", "effort": "none"}}
with transcript_path.open("a", encoding="utf-8") as transcript:
    transcript.write(json.dumps({{"env": {{"SYMPHONY_MNEMESH_WORKSPACE_ROOT": os.environ.get("SYMPHONY_MNEMESH_WORKSPACE_ROOT"), "SYMPHONY_ISSUE_WORKTREE": os.environ.get("SYMPHONY_ISSUE_WORKTREE")}}}}, sort_keys=True) + "\n")

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

fn write_hanging_before_session_new_acp_script(dir: &Path, transcript_path: &Path) -> PathBuf {
    let script_path = dir.join("fake-opencode-acp-session-new-hang.py");
    let transcript_literal =
        serde_json::to_string(&transcript_path.display().to_string()).expect("json path");
    fs::write(
        &script_path,
        format!(
            r#"#!/usr/bin/env python3
import json
import pathlib
import sys
import time

transcript_path = pathlib.Path({transcript_literal})

for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    with transcript_path.open("a", encoding="utf-8") as transcript:
        transcript.write(json.dumps(message, sort_keys=True) + "\n")

    if method == "initialize":
        print(json.dumps({{"jsonrpc": "2.0", "id": message["id"], "result": {{"protocolVersion": 1}}}}), flush=True)
    elif method == "session/new":
        time.sleep(60)
    else:
        print(json.dumps({{"jsonrpc": "2.0", "id": message.get("id"), "error": {{"code": -32601, "message": "unexpected method before session/new"}}}}), flush=True)
"#
        ),
    )
    .expect("hanging fake acp script");
    let mut permissions = fs::metadata(&script_path)
        .expect("hanging fake acp metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("hanging fake acp executable");
    script_path
}

fn write_failing_acp_setup_script(dir: &Path, child_pid_path: &Path) -> PathBuf {
    let script_path = dir.join("fake-opencode-acp-setup-fail.py");
    let child_pid_literal =
        serde_json::to_string(&child_pid_path.display().to_string()).expect("json path");
    fs::write(
        &script_path,
        format!(
            r#"#!/usr/bin/env python3
import json
import pathlib
import subprocess
import sys
import time

child = subprocess.Popen(["sleep", "60"])
pathlib.Path({child_pid_literal}).write_text(str(child.pid))
sys.stderr.write("setup stderr must be drained\n" * 2000)
sys.stderr.flush()

line = sys.stdin.readline()
message = json.loads(line)
print(json.dumps({{"jsonrpc": "2.0", "id": message["id"], "error": {{"code": -32000, "message": "setup failed before session attachment"}}}}), flush=True)
time.sleep(60)
"#
        ),
    )
    .expect("fake setup failure script");
    let mut permissions = fs::metadata(&script_path)
        .expect("script metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("script executable");
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

fn write_fake_acp_resume_script(dir: &Path, transcript_path: &Path) -> PathBuf {
    let script_path = dir.join("fake-opencode-acp-resume.py");
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
config = {{"mode": "build", "model": "opencode/big-pickle", "effort": "none"}}

def ok(mid, result):
    print(json.dumps({{"jsonrpc": "2.0", "id": mid, "result": result}}), flush=True)

for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    with transcript_path.open("a", encoding="utf-8") as transcript:
        transcript.write(json.dumps(message, sort_keys=True) + "\n")

    if method == "initialize":
        ok(message["id"], {{"protocolVersion": 1, "agentCapabilities": {{"sessionCapabilities": {{"resume": {{}}}}}}}})
    elif method == "session/resume":
        ok(message["id"], {{"sessionId": message["params"]["sessionId"], "resumed": True}})
    elif method == "session/set_config_option":
        config[message["params"]["configId"]] = message["params"]["value"]
        ok(message["id"], {{"configOptions": []}})
    elif method == "session/new":
        print(json.dumps({{"jsonrpc": "2.0", "id": message["id"], "error": {{"code": -32000, "message": "session/new must not be called on resume"}}}}), flush=True)
        break
    elif method == "session/prompt":
        print(json.dumps({{"jsonrpc": "2.0", "id": message["id"], "error": {{"code": -32000, "message": "prompt must not be replayed on resume"}}}}), flush=True)
        break
    else:
        ok(message.get("id"), {{}})
"#
        ),
    )
    .expect("fake resume acp script");
    let mut permissions = fs::metadata(&script_path)
        .expect("fake resume acp metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("fake resume acp executable");
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
        "projectMilestone": { "id": "test-milestone-id", "name": "Test Milestone" },
        "labels": { "nodes": [] },
        "comments": { "nodes": [] },
        "relations": { "nodes": [] },
        "inverseRelations": { "nodes": [] },
        "createdAt": "2026-06-10T00:00:00Z",
        "updatedAt": "2026-06-10T00:00:00Z"
    })
}

#[derive(Debug)]
struct RecordingLinearClient {
    issues: Vec<LinearIssue>,
    transitions: std::sync::Mutex<Vec<(String, LinearTransition)>>,
    evidence: std::sync::Mutex<Vec<(String, LinearIssueEvidence)>>,
    managed_issues: std::sync::Mutex<Vec<ManagedLinearIssueCreate>>,
    relations: std::sync::Mutex<Vec<(String, String, ManagedLinearRelation)>>,
}

impl RecordingLinearClient {
    const fn new(issues: Vec<LinearIssue>) -> Self {
        Self {
            issues,
            transitions: std::sync::Mutex::new(Vec::new()),
            evidence: std::sync::Mutex::new(Vec::new()),
            managed_issues: std::sync::Mutex::new(Vec::new()),
            relations: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn transitions(&self) -> Vec<(String, LinearTransition)> {
        self.transitions.lock().expect("transitions lock").clone()
    }

    fn evidence(&self) -> Vec<(String, LinearIssueEvidence)> {
        self.evidence.lock().expect("evidence lock").clone()
    }

    fn managed_issues(&self) -> Vec<ManagedLinearIssueCreate> {
        self.managed_issues
            .lock()
            .expect("managed issues lock")
            .clone()
    }

    fn relations(&self) -> Vec<(String, String, ManagedLinearRelation)> {
        self.relations.lock().expect("relations lock").clone()
    }
}

fn assert_todo_transition(transitions: &[(String, LinearTransition)], issue_id: &str) {
    assert!(
        transitions
            .iter()
            .any(|(id, transition)| id == issue_id && *transition == LinearTransition::Todo),
        "expected {issue_id} to leave Linear In Progress via Todo transition, got {transitions:?}"
    );
}

#[async_trait::async_trait]
impl LinearClient for RecordingLinearClient {
    async fn fetch_candidate_issues(
        &self,
        _project: &symphony::config::ProjectConfig,
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

    async fn create_managed_issue(
        &self,
        _project: &symphony::config::ProjectConfig,
        request: ManagedLinearIssueCreate,
    ) -> Result<LinearIssue, LinearClientError> {
        let identifier = format!("SYM-DEFECT-{}", self.managed_issues().len() + 1);
        let issue = LinearIssue {
            id: format!("managed-{}", self.managed_issues().len() + 1),
            identifier,
            title: request.title.clone(),
            description: Some(request.description_with_fingerprint()),
            state: request.state.state_name().into(),
            priority: Some(request.priority),
            branch_name: None,
            url: None,
            labels: Vec::new(),
            project_milestone: None,
            blocked_by: Vec::new(),
            has_new_owner_answer: false,
            owner_answer_created_at: None,
            created_at: None,
            updated_at: None,
        };
        self.managed_issues
            .lock()
            .expect("managed issues lock")
            .push(request);
        Ok(issue)
    }

    async fn create_issue_relation(
        &self,
        source_issue_id: &str,
        managed_issue_id: &str,
        relation: ManagedLinearRelation,
    ) -> Result<(), LinearClientError> {
        self.relations.lock().expect("relations lock").push((
            source_issue_id.to_string(),
            managed_issue_id.to_string(),
            relation,
        ));
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
        project: &symphony::config::ProjectConfig,
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

#[derive(Debug)]
struct PartiallyFailingProjectLinearClient {
    failing_project_id: String,
    issues_by_project: std::collections::HashMap<String, Vec<LinearIssue>>,
    transitions: std::sync::Mutex<Vec<(String, LinearTransition)>>,
}

impl PartiallyFailingProjectLinearClient {
    fn new<const N: usize>(
        failing_project_id: impl Into<String>,
        issues: [(&str, Vec<LinearIssue>); N],
    ) -> Self {
        Self {
            failing_project_id: failing_project_id.into(),
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
impl LinearClient for PartiallyFailingProjectLinearClient {
    async fn fetch_candidate_issues(
        &self,
        project: &symphony::config::ProjectConfig,
    ) -> Result<Vec<LinearIssue>, LinearClientError> {
        if project.id == self.failing_project_id {
            return Err(LinearClientError::Message(format!(
                "synthetic fetch failure for {}",
                project.id
            )));
        }
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
    const fn new(handoff: Option<OpenCodeHandoff>) -> Self {
        Self {
            handoff,
            repairs: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn repairs(&self) -> Vec<(String, String)> {
        self.repairs.lock().expect("repairs lock").clone()
    }
}

#[derive(Debug)]
struct FailingLaunchOpenCodeLauncher {
    message: String,
}

impl FailingLaunchOpenCodeLauncher {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[async_trait::async_trait]
impl OpenCodeLauncher for FailingLaunchOpenCodeLauncher {
    async fn launch(
        &self,
        _spec: &opencode::OpenCodeLaunchSpec,
    ) -> Result<opencode::OpenCodeStartedSession, opencode::OpenCodeError> {
        Err(opencode::OpenCodeError::InvalidWorktree(
            self.message.clone(),
        ))
    }
}

#[derive(Debug)]
struct SetupFailingOpenCodeLauncher;

#[async_trait::async_trait]
impl OpenCodeLauncher for SetupFailingOpenCodeLauncher {
    async fn launch(
        &self,
        spec: &opencode::OpenCodeLaunchSpec,
    ) -> Result<opencode::OpenCodeStartedSession, opencode::OpenCodeError> {
        Err(opencode::OpenCodeError::AcpSetupFailed {
            issue_identifier: spec.issue_identifier.clone(),
            process_id: Some(4242),
            session_id: None,
            reason: "setup failed before session attachment".into(),
            termination: Box::new(opencode::ProcessTreeTerminationEvidence {
                root_process_id: 4242,
                descendant_process_ids: vec![4243],
                term_signal_sent: true,
                kill_signal_sent: false,
                still_alive: false,
                reason: "setup failed before session attachment".into(),
            }),
        })
    }
}

#[derive(Debug)]
struct FailingContinueOpenCodeLauncher;

#[async_trait::async_trait]
impl OpenCodeLauncher for FailingContinueOpenCodeLauncher {
    async fn launch(
        &self,
        spec: &opencode::OpenCodeLaunchSpec,
    ) -> Result<opencode::OpenCodeStartedSession, opencode::OpenCodeError> {
        Ok(opencode::OpenCodeStartedSession {
            session_id: format!("new:{}", spec.issue_identifier),
            process_id: Some(6210),
        })
    }

    async fn continue_session(
        &self,
        _spec: &opencode::OpenCodeLaunchSpec,
        _session: &OpenCodeSessionRecord,
        _continuation_message: &str,
    ) -> Result<opencode::OpenCodeStartedSession, opencode::OpenCodeError> {
        Err(opencode::OpenCodeError::InvalidWorktree(
            "continue failed".into(),
        ))
    }
}

#[derive(Debug)]
struct ResumeRecordingOpenCodeLauncher {
    resumed_process_id: u32,
    launches: std::sync::Mutex<Vec<String>>,
    resumes: std::sync::Mutex<Vec<(String, String)>>,
    continuations: std::sync::Mutex<Vec<(String, String)>>,
    repairs: std::sync::Mutex<Vec<(String, String)>>,
}

impl ResumeRecordingOpenCodeLauncher {
    const fn new(resumed_process_id: u32) -> Self {
        Self {
            resumed_process_id,
            launches: std::sync::Mutex::new(Vec::new()),
            resumes: std::sync::Mutex::new(Vec::new()),
            continuations: std::sync::Mutex::new(Vec::new()),
            repairs: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn launches(&self) -> Vec<String> {
        self.launches.lock().expect("launches lock").clone()
    }

    fn resumes(&self) -> Vec<(String, String)> {
        self.resumes.lock().expect("resumes lock").clone()
    }

    fn continuations(&self) -> Vec<(String, String)> {
        self.continuations
            .lock()
            .expect("continuations lock")
            .clone()
    }

    fn repairs(&self) -> Vec<(String, String)> {
        self.repairs.lock().expect("repairs lock").clone()
    }
}

#[async_trait::async_trait]
impl OpenCodeLauncher for ResumeRecordingOpenCodeLauncher {
    async fn launch(
        &self,
        spec: &opencode::OpenCodeLaunchSpec,
    ) -> Result<opencode::OpenCodeStartedSession, opencode::OpenCodeError> {
        self.launches
            .lock()
            .expect("launches lock")
            .push(spec.issue_identifier.clone());
        Ok(opencode::OpenCodeStartedSession {
            session_id: format!("new:{}", spec.issue_identifier),
            process_id: Some(self.resumed_process_id + 1),
        })
    }

    async fn resume(
        &self,
        spec: &opencode::OpenCodeLaunchSpec,
        session: &OpenCodeSessionRecord,
    ) -> Result<opencode::OpenCodeStartedSession, opencode::OpenCodeError> {
        self.resumes
            .lock()
            .expect("resumes lock")
            .push((spec.issue_identifier.clone(), session.session_id.clone()));
        Ok(opencode::OpenCodeStartedSession {
            session_id: session.session_id.clone(),
            process_id: Some(self.resumed_process_id),
        })
    }

    async fn continue_session(
        &self,
        spec: &opencode::OpenCodeLaunchSpec,
        session: &OpenCodeSessionRecord,
        _continuation_message: &str,
    ) -> Result<opencode::OpenCodeStartedSession, opencode::OpenCodeError> {
        self.continuations
            .lock()
            .expect("continuations lock")
            .push((spec.issue_identifier.clone(), session.session_id.clone()));
        Ok(opencode::OpenCodeStartedSession {
            session_id: session.session_id.clone(),
            process_id: Some(self.resumed_process_id),
        })
    }

    async fn continue_repair(
        &self,
        spec: &opencode::OpenCodeLaunchSpec,
        session: &OpenCodeSessionRecord,
        failure_fingerprint: &str,
        _repair_message: &str,
    ) -> Result<opencode::OpenCodeStartedSession, opencode::OpenCodeError> {
        self.repairs.lock().expect("repairs lock").push((
            spec.issue_identifier.clone(),
            failure_fingerprint.to_owned(),
        ));
        Ok(opencode::OpenCodeStartedSession {
            session_id: session.session_id.clone(),
            process_id: Some(self.resumed_process_id),
        })
    }
}

#[derive(Debug)]
struct MalformedHandoffOpenCodeLauncher {
    message: String,
    repairs: std::sync::Mutex<Vec<(String, String)>>,
}

impl MalformedHandoffOpenCodeLauncher {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            repairs: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn repairs(&self) -> Vec<(String, String)> {
        self.repairs.lock().expect("repairs lock").clone()
    }
}

#[async_trait::async_trait]
impl OpenCodeLauncher for MalformedHandoffOpenCodeLauncher {
    async fn launch(
        &self,
        _spec: &opencode::OpenCodeLaunchSpec,
    ) -> Result<opencode::OpenCodeStartedSession, opencode::OpenCodeError> {
        unreachable!("malformed handoff test should not launch OpenCode")
    }

    async fn latest_handoff(
        &self,
        _session: &OpenCodeSessionRecord,
    ) -> Result<Option<OpenCodeHandoff>, opencode::OpenCodeError> {
        Err(opencode::OpenCodeError::MalformedHandoff(
            self.message.clone(),
        ))
    }

    async fn continue_repair(
        &self,
        _spec: &opencode::OpenCodeLaunchSpec,
        session: &OpenCodeSessionRecord,
        failure_fingerprint: &str,
        _repair_message: &str,
    ) -> Result<opencode::OpenCodeStartedSession, opencode::OpenCodeError> {
        self.repairs
            .lock()
            .expect("repairs lock")
            .push((session.session_id.clone(), failure_fingerprint.to_string()));
        Ok(opencode::OpenCodeStartedSession {
            session_id: session.session_id.clone(),
            process_id: session.process_id,
        })
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
            process_id: None,
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
        _spec: &opencode::OpenCodeLaunchSpec,
        session: &OpenCodeSessionRecord,
        failure_fingerprint: &str,
        _repair_message: &str,
    ) -> Result<opencode::OpenCodeStartedSession, opencode::OpenCodeError> {
        self.repairs
            .lock()
            .expect("repairs lock")
            .push((session.session_id.clone(), failure_fingerprint.to_string()));
        Ok(opencode::OpenCodeStartedSession {
            session_id: session.session_id.clone(),
            process_id: session.process_id,
        })
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
