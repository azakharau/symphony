use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    thread,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    config::ProjectConfig,
    linear::LinearIssue,
    state::{LifecycleStage, OpenCodeSessionRecord, OpenCodeStage},
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeRuntimeConfig {
    pub command: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    pub agent: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub permission_policy: PermissionPolicy,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPolicy {
    Reject,
    Cancel,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenCodeLaunchSpec {
    pub command: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub worktree_root: Option<PathBuf>,
    pub issue_identifier: String,
    pub repo_path: Option<PathBuf>,
    pub base_ref: Option<String>,
    pub agent: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub prompt: String,
    pub permission_policy: PermissionPolicy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenCodeStartedSession {
    pub session_id: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OpenCodeSessionEvent {
    pub stage: Option<OpenCodeStage>,
    pub active_agent: Option<String>,
    pub active_model: Option<String>,
    pub message_delta: u64,
    pub todo_delta: u64,
    pub part_delta: u64,
    pub token_delta: u64,
    pub cost_micros_delta: u64,
    pub subagent_delta: u64,
    pub eval_stage: Option<String>,
    pub lifecycle_marker: Option<String>,
    pub last_event: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeHandoff {
    pub session_id: String,
    pub lifecycle_stages: Vec<OpenCodeStage>,
    pub subagents: Vec<String>,
    pub eval_results: Vec<OpenCodeEvalResult>,
    pub changed_files: Vec<String>,
    pub git: Option<GitClosureEvidence>,
    pub risks: Vec<String>,
    pub stop_reason: OpenCodeStopReason,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeEvalResult {
    pub suite: String,
    pub passed: bool,
    pub failure_fingerprint: Option<String>,
    pub details: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GitClosureEvidence {
    pub branch: String,
    pub head_sha: Option<String>,
    pub pr_url: Option<String>,
    pub worktree_path: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum OpenCodeStopReason {
    Success,
    EvalFailed { failure_fingerprint: String },
    ProviderBlocker { message: String },
    OwnerQuestion { question: String },
}

pub trait OpenCodeLauncher {
    fn launch(&self, spec: &OpenCodeLaunchSpec) -> Result<OpenCodeStartedSession, OpenCodeError>;

    fn latest_handoff(
        &self,
        _session: &OpenCodeSessionRecord,
    ) -> Result<Option<OpenCodeHandoff>, OpenCodeError> {
        Ok(None)
    }

    fn continue_repair(
        &self,
        _session: &OpenCodeSessionRecord,
        _failure_fingerprint: &str,
    ) -> Result<(), OpenCodeError> {
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct DeterministicOpenCodeLauncher;

impl OpenCodeLauncher for DeterministicOpenCodeLauncher {
    fn launch(&self, spec: &OpenCodeLaunchSpec) -> Result<OpenCodeStartedSession, OpenCodeError> {
        Ok(OpenCodeStartedSession {
            session_id: deterministic_session_id(&spec.cwd.display().to_string()),
        })
    }
}

#[derive(Debug, Default)]
pub struct StdioOpenCodeLauncher;

impl OpenCodeLauncher for StdioOpenCodeLauncher {
    fn launch(&self, spec: &OpenCodeLaunchSpec) -> Result<OpenCodeStartedSession, OpenCodeError> {
        ensure_worktree(spec)?;
        let mut child = Command::new(&spec.command)
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().ok_or(OpenCodeError::MissingStdin)?;
        let stdout = child.stdout.take().ok_or(OpenCodeError::MissingStdout)?;
        let mut stdout = BufReader::new(stdout);

        let mut next_id = 1_u64;
        acp_request(
            &mut stdin,
            &mut stdout,
            &spec.permission_policy,
            next_id,
            "initialize",
            json!({
                "protocolVersion": 1,
                "agent": spec.agent,
                "model": spec.model,
            }),
        )?;
        next_id += 1;

        let session_result = acp_request(
            &mut stdin,
            &mut stdout,
            &spec.permission_policy,
            next_id,
            "session/new",
            session_new_params(spec),
        )?;
        next_id += 1;
        let session_id = extract_session_id(&session_result)?;
        set_session_config_option(
            &mut stdin,
            &mut stdout,
            &spec.permission_policy,
            next_id,
            &session_id,
            "mode",
            Some(spec.agent.as_str()),
        )?;
        next_id += 1;
        set_session_config_option(
            &mut stdin,
            &mut stdout,
            &spec.permission_policy,
            next_id,
            &session_id,
            "model",
            spec.model.as_deref(),
        )?;
        next_id += 1;
        set_session_config_option(
            &mut stdin,
            &mut stdout,
            &spec.permission_policy,
            next_id,
            &session_id,
            "effort",
            spec.effort.as_deref(),
        )?;
        next_id += 1;
        let background_session_id = session_id.clone();
        let permission_policy = spec.permission_policy.clone();
        let prompt = spec.prompt.clone();

        thread::spawn(move || {
            let _ = acp_request(
                &mut stdin,
                &mut stdout,
                &permission_policy,
                next_id,
                "session/prompt",
                json!({
                    "sessionId": background_session_id,
                    "prompt": [
                        {
                            "type": "text",
                            "text": prompt,
                        }
                    ],
                }),
            );
            let _ = child.wait();
        });

        Ok(OpenCodeStartedSession { session_id })
    }

    fn latest_handoff(
        &self,
        session: &OpenCodeSessionRecord,
    ) -> Result<Option<OpenCodeHandoff>, OpenCodeError> {
        let path = handoff_sidecar_path(&session.worktree_path);
        if !path.exists() {
            return Ok(None);
        }

        let input = fs::read_to_string(&path)?;
        let handoff = serde_json::from_str(&input)
            .map_err(|error| OpenCodeError::MalformedHandoff(format!("{path:?}: {error}")))?;
        Ok(Some(handoff))
    }
}

pub fn build_acp_launch_spec(project: &ProjectConfig, issue: &LinearIssue) -> OpenCodeLaunchSpec {
    OpenCodeLaunchSpec {
        command: project.opencode.command.clone(),
        args: project.opencode.args.clone(),
        cwd: project.branch.worktree_root.join(&issue.identifier),
        worktree_root: Some(project.branch.worktree_root.clone()),
        issue_identifier: issue.identifier.clone(),
        repo_path: Some(project.repo_path.clone()),
        base_ref: Some(project.branch.base.clone()),
        agent: project.opencode.agent.clone(),
        model: project.opencode.model.clone(),
        effort: project.opencode.effort.clone(),
        prompt: build_issue_prompt(project, issue),
        permission_policy: project.opencode.permission_policy.clone(),
    }
}

pub fn new_session_record(
    project: &ProjectConfig,
    issue: &LinearIssue,
    started: OpenCodeStartedSession,
    spec: &OpenCodeLaunchSpec,
) -> OpenCodeSessionRecord {
    OpenCodeSessionRecord {
        project_id: project.id.clone(),
        issue_id: issue.id.clone(),
        session_id: started.session_id,
        agent: project.opencode.agent.clone(),
        model: project.opencode.model.clone(),
        worktree_path: spec.cwd.display().to_string(),
        lifecycle_stage: LifecycleStage::Running,
        stage: OpenCodeStage::Starting,
        active_agent: Some(project.opencode.agent.clone()),
        active_model: project.opencode.model.clone(),
        message_count: 0,
        todo_count: 0,
        part_count: 0,
        token_count: 0,
        cost_micros: 0,
        subagent_count: 0,
        eval_stage: Some(project.eval.default_suite.clone()),
        lifecycle_marker: Some("acp_started".into()),
        last_event: Some("acp_process_started".into()),
        silence_observed: false,
    }
}

pub fn ingest_session_event(session: &mut OpenCodeSessionRecord, event: OpenCodeSessionEvent) {
    if let Some(stage) = event.stage {
        session.stage = stage;
    }
    if let Some(agent) = event.active_agent {
        session.active_agent = Some(agent);
    }
    if let Some(model) = event.active_model {
        session.active_model = Some(model);
    }
    if let Some(eval_stage) = event.eval_stage {
        session.eval_stage = Some(eval_stage);
    }
    if let Some(marker) = event.lifecycle_marker {
        session.lifecycle_marker = Some(marker);
    }
    if let Some(last_event) = event.last_event {
        session.last_event = Some(last_event);
    }

    session.message_count = session.message_count.saturating_add(event.message_delta);
    session.todo_count = session.todo_count.saturating_add(event.todo_delta);
    session.part_count = session.part_count.saturating_add(event.part_delta);
    session.token_count = session.token_count.saturating_add(event.token_delta);
    session.cost_micros = session.cost_micros.saturating_add(event.cost_micros_delta);
    session.subagent_count = session.subagent_count.saturating_add(event.subagent_delta);
}

pub fn mark_session_silence(session: &mut OpenCodeSessionRecord, reason: &str) {
    session.stage = OpenCodeStage::Silent;
    session.silence_observed = true;
    session.last_event = Some(format!("silence:{reason}"));
}

fn build_issue_prompt(project: &ProjectConfig, issue: &LinearIssue) -> String {
    let description = issue
        .description
        .as_deref()
        .unwrap_or("No description provided.");
    format!(
        "Run OpenCode ACP for {identifier}: {title}\n\n\
         Project: {project_id}\n\
         Repository: {repo_path}\n\
         Isolated worktree: {worktree}\n\
         Eval default suite: {eval_suite}\n\
         Linear state: {state}\n\
         URL: {url}\n\n\
         On completion, write the structured Symphony handoff JSON to:\n\
         {handoff_path}\n\n\
         Full issue spec:\n{description}\n",
        identifier = issue.identifier,
        title = issue.title,
        project_id = project.id,
        repo_path = project.repo_path.display(),
        worktree = project
            .branch
            .worktree_root
            .join(&issue.identifier)
            .display(),
        handoff_path =
            handoff_sidecar_path(project.branch.worktree_root.join(&issue.identifier)).display(),
        eval_suite = project.eval.default_suite,
        state = issue.state,
        url = issue.url.as_deref().unwrap_or("none"),
    )
}

fn deterministic_session_id(input: &str) -> String {
    format!("opencode:{input}")
}

fn ensure_worktree(spec: &OpenCodeLaunchSpec) -> Result<(), OpenCodeError> {
    validate_launch_worktree(spec)?;

    let Some(repo_path) = &spec.repo_path else {
        fs::create_dir_all(&spec.cwd)?;
        return Ok(());
    };
    let Some(base_ref) = &spec.base_ref else {
        fs::create_dir_all(&spec.cwd)?;
        return Ok(());
    };

    if spec.cwd.join(".git").exists() {
        return Ok(());
    }

    if spec.cwd.exists() && spec.cwd.read_dir()?.next().is_some() {
        return Err(OpenCodeError::InvalidWorktree(format!(
            "target worktree {} exists but is not a git worktree",
            spec.cwd.display()
        )));
    }

    if let Some(parent) = spec.cwd.parent() {
        fs::create_dir_all(parent)?;
    }

    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["worktree", "add", "--detach"])
        .arg(&spec.cwd)
        .arg(base_ref)
        .output()?;

    if output.status.success() {
        Ok(())
    } else {
        Err(OpenCodeError::GitCommand {
            command: format!(
                "git -C {} worktree add --detach {} {}",
                repo_path.display(),
                spec.cwd.display(),
                base_ref
            ),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

fn validate_launch_worktree(spec: &OpenCodeLaunchSpec) -> Result<(), OpenCodeError> {
    if !safe_worktree_name(&spec.issue_identifier) {
        return Err(OpenCodeError::InvalidWorktree(format!(
            "issue identifier `{}` is not a safe worktree path component",
            spec.issue_identifier
        )));
    }

    if let Some(root) = &spec.worktree_root {
        let expected = root.join(&spec.issue_identifier);
        if spec.cwd != expected {
            return Err(OpenCodeError::InvalidWorktree(format!(
                "worktree path {} does not match configured root plus issue identifier {}",
                spec.cwd.display(),
                expected.display()
            )));
        }
    }

    Ok(())
}

fn safe_worktree_name(identifier: &str) -> bool {
    !identifier.is_empty()
        && identifier
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

fn set_session_config_option<R: BufRead, W: Write>(
    stdin: &mut W,
    stdout: &mut R,
    permission_policy: &PermissionPolicy,
    id: u64,
    session_id: &str,
    config_id: &str,
    value: Option<&str>,
) -> Result<(), OpenCodeError> {
    let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
        return Ok(());
    };
    acp_request(
        stdin,
        stdout,
        permission_policy,
        id,
        "session/set_config_option",
        json!({
            "sessionId": session_id,
            "configId": config_id,
            "value": value,
        }),
    )?;
    Ok(())
}

fn session_new_params(spec: &OpenCodeLaunchSpec) -> Value {
    json!({
        "cwd": spec.cwd,
        "title": spec
            .prompt
            .lines()
            .next()
            .unwrap_or("Symphony OpenCode issue"),
        "agent": spec.agent,
        "mcpServers": [],
    })
}

fn acp_request<R: BufRead, W: Write>(
    stdin: &mut W,
    stdout: &mut R,
    permission_policy: &PermissionPolicy,
    id: u64,
    method: &str,
    params: Value,
) -> Result<Value, OpenCodeError> {
    let request = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    writeln!(stdin, "{request}")?;
    stdin.flush()?;

    loop {
        let mut line = String::new();
        let bytes = stdout.read_line(&mut line)?;
        if bytes == 0 {
            return Err(OpenCodeError::AcpProtocol(format!(
                "ACP stdout closed before `{method}` response"
            )));
        }
        let message: Value = serde_json::from_str(&line).map_err(|error| {
            OpenCodeError::AcpProtocol(format!("invalid ACP JSON `{line}`: {error}"))
        })?;

        if message.get("id").and_then(Value::as_u64) == Some(id) {
            if let Some(error) = message.get("error") {
                return Err(OpenCodeError::AcpProtocol(format!(
                    "ACP `{method}` failed: {error}"
                )));
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }

        if message.get("id").is_some() && message.get("method").is_some() {
            respond_to_acp_request(stdin, permission_policy, &message)?;
        }
    }
}

fn respond_to_acp_request<W: Write>(
    stdin: &mut W,
    permission_policy: &PermissionPolicy,
    request: &Value,
) -> Result<(), OpenCodeError> {
    let Some(id) = request.get("id").cloned() else {
        return Ok(());
    };
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("request");
    let result = match permission_policy {
        PermissionPolicy::Reject => json!({
            "outcome": "rejected",
            "message": format!("Symphony rejects ACP `{method}` requests in unattended mode"),
        }),
        PermissionPolicy::Cancel => json!({
            "outcome": "cancelled",
            "message": format!("Symphony cancels ACP `{method}` requests in unattended mode"),
        }),
    };
    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        })
    )?;
    stdin.flush()?;
    Ok(())
}

fn extract_session_id(result: &Value) -> Result<String, OpenCodeError> {
    result
        .get("sessionId")
        .or_else(|| result.get("id"))
        .and_then(Value::as_str)
        .filter(|session_id| !session_id.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            OpenCodeError::AcpProtocol(format!(
                "ACP session/new response did not include session id: {result}"
            ))
        })
}

fn handoff_sidecar_path(path: impl AsRef<Path>) -> PathBuf {
    path.as_ref()
        .join(".symphony")
        .join("opencode-handoff.json")
}

pub fn worktree_path_allowed(root: &Path, candidate: &Path) -> bool {
    candidate.is_absolute()
        && candidate.starts_with(root)
        && !candidate
            .components()
            .any(|component| matches!(component, Component::ParentDir))
}

#[derive(Debug, Error)]
pub enum OpenCodeError {
    #[error("opencode io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("opencode child stdin was not piped")]
    MissingStdin,
    #[error("opencode child stdout was not piped")]
    MissingStdout,
    #[error("opencode ACP protocol error: {0}")]
    AcpProtocol(String),
    #[error("invalid opencode worktree: {0}")]
    InvalidWorktree(String),
    #[error("git command failed: {command}: {stderr}")]
    GitCommand { command: String, stderr: String },
    #[error("malformed opencode handoff: {0}")]
    MalformedHandoff(String),
}
