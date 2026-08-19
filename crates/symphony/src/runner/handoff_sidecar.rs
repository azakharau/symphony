use serde_json::{Map, Value, json};

pub(super) fn normalize_handoff_sidecar_value(value: &mut Value, worktree_path: &str) {
    let Some(object) = value.as_object_mut() else {
        return;
    };

    if !object.contains_key("stop_reason")
        && let Some(status) = object.get("status").and_then(Value::as_str)
    {
        object.insert("stop_reason".to_owned(), Value::String(status.to_owned()));
    }
    object.remove("schema_version");
    object.remove("status");
    object.remove("repair_fingerprint");
    object.remove("task_id");
    object.remove("subtask_id");
    object.remove("recall");

    if !object.contains_key("subagents") {
        if let Some(subagents_used) = object.remove("subagents_used") {
            object.insert("subagents".to_owned(), subagents_used);
        }
    } else {
        object.remove("subagents_used");
    }
    normalize_string_array_field(object, "subagents", structured_agent_label);
    normalize_string_array_field(object, "changed_files", structured_changed_file_label);
    normalize_string_array_field(object, "risks", structured_summary_label);

    if let Some(stages) = object
        .get_mut("lifecycle_stages")
        .and_then(Value::as_array_mut)
    {
        for stage in stages {
            if !stage.is_string() {
                *stage = Value::String(structured_stage_label(stage));
            }
            if let Some(stage_name) = stage.as_str().and_then(canonical_handoff_stage) {
                *stage = Value::String(stage_name.to_owned());
            }
        }
    }

    if object.get("eval_results").is_some_and(Value::is_object) {
        let eval = object.remove("eval_results").unwrap_or(Value::Null);
        let passed = compact_eval_object_passed(&eval);
        let evidence_ref = eval
            .get("evaluation_ref")
            .or_else(|| eval.get("evidence_ref"))
            .or_else(|| eval.get("verification_ref"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let failure_fingerprint = eval
            .get("failure_fingerprint")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let details = eval
            .get("details")
            .or_else(|| eval.get("summary"))
            .or_else(|| eval.get("verification"))
            .map(|value| match value {
                Value::String(details) => details.clone(),
                Value::Array(items) => items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n"),
                other => other.to_string(),
            });
        object.insert(
            "eval_results".to_owned(),
            json!([{
                "suite": "runner-evaluation",
                "passed": passed,
                "failure_fingerprint": failure_fingerprint,
                "details": details,
                "evidence_ref": evidence_ref,
            }]),
        );
    }
    normalize_eval_results(object);

    object.remove("validation");

    if let Some(git) = object.get_mut("git").and_then(Value::as_object_mut) {
        if !git.contains_key("head_sha") {
            if let Some(commit) = git.remove("commit") {
                git.insert("head_sha".to_owned(), commit);
            }
        } else {
            git.remove("commit");
        }
        if git
            .get("worktree_path")
            .is_none_or(|value| value.is_null() || value.as_str().is_some_and(str::is_empty))
        {
            git.insert("worktree_path".to_owned(), json!(worktree_path));
        }
        git.remove("remote");
        git.remove("pushed");
        git.remove("status");
        git.remove("evidence_ref");
        git.remove("base_branch");
        git.remove("base_sha");
        git.remove("previous_head_sha");
        git.remove("remote_ref");
        git.remove("remote_head_sha");
        git.remove("commit_message");
        git.remove("commit_summary");
        normalize_object_string_field(git, "branch", structured_summary_label);
        normalize_object_string_field(git, "head_sha", structured_summary_label);
        normalize_object_string_field(git, "pr_url", structured_summary_label);
        normalize_object_string_field(git, "worktree_path", structured_summary_label);
        remove_null_object_fields(git, &["head_sha", "pr_url"]);
    }

    normalize_stop_reason(object);
}

fn normalize_stop_reason(object: &mut Map<String, Value>) {
    let Some(raw) = object.get("stop_reason").cloned() else {
        return;
    };
    let fallback_message = object
        .get("message")
        .or_else(|| object.get("question"))
        .or_else(|| object.get("failure_fingerprint"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let Some(normalized) = normalized_stop_reason_value(&raw, fallback_message.as_deref()) else {
        return;
    };
    if fallback_message.is_some()
        && normalized
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|reason| reason != "success")
    {
        object.remove("message");
        object.remove("question");
        object.remove("failure_fingerprint");
    }
    object.insert("stop_reason".to_owned(), normalized);
}

fn normalized_stop_reason_value(raw: &Value, fallback_message: Option<&str>) -> Option<Value> {
    if let Some(reason) = raw.as_str() {
        return normalized_stop_reason_from_parts(reason, fallback_message, None);
    }

    let object = raw.as_object()?;
    let reason = object
        .get("type")
        .or_else(|| object.get("stop_reason"))
        .or_else(|| object.get("reason"))
        .or_else(|| object.get("kind"))
        .or_else(|| object.get("category"))
        .or_else(|| object.get("status"))
        .and_then(Value::as_str)?;
    let message = object
        .get("message")
        .or_else(|| object.get("question"))
        .or_else(|| object.get("failure_fingerprint"))
        .or_else(|| object.get("error"))
        .or_else(|| object.get("summary"))
        .or_else(|| object.get("details"))
        .and_then(Value::as_str)
        .or(fallback_message);
    normalized_stop_reason_from_parts(reason, message, Some(object))
}

fn normalized_stop_reason_from_parts(
    reason: &str,
    message: Option<&str>,
    object: Option<&Map<String, Value>>,
) -> Option<Value> {
    let normalized = reason.trim().to_ascii_lowercase().replace('-', "_");
    match normalized.as_str() {
        "accepted" | "completed" | "success" => Some(json!({"type": "success"})),
        "eval_failed" | "evaluation_failed" => Some(json!({
            "type": "eval_failed",
            "failure_fingerprint": message.unwrap_or("eval_failed"),
        })),
        "provider_blocker" | "provider_error" | "provider_failure" | "runtime_blocker" => {
            Some(json!({
                "type": "provider_blocker",
                "message": message.unwrap_or("provider blocker"),
            }))
        }
        "auth_blocker" | "provider_auth" | "provider_auth_error" | "credential_blocker" => {
            Some(json!({
                "type": "auth_blocker",
                "message": message.unwrap_or("provider authentication blocker"),
            }))
        }
        "unsupported_omp_surface" | "unsupported_surface" | "unsupported_provider_surface" => {
            Some(json!({
                "type": "unsupported_omp_surface",
                "message": message.unwrap_or("unsupported OMP ACP surface"),
            }))
        }
        "owner_question" => Some(json!({
            "type": "owner_question",
            "question": message.unwrap_or("owner question"),
        })),
        "blocked" => object.and_then(|object| {
            object
                .get("blocker_kind")
                .or_else(|| object.get("blocker"))
                .or_else(|| object.get("reason"))
                .and_then(Value::as_str)
                .and_then(|kind| normalized_stop_reason_from_parts(kind, message, None))
        }),
        _ => Some(json!({"type": normalized})),
    }
}

fn normalize_eval_results(object: &mut Map<String, Value>) {
    let Some(results) = object.get_mut("eval_results").and_then(Value::as_array_mut) else {
        return;
    };

    for result in results {
        let Some(result_object) = result.as_object_mut() else {
            continue;
        };
        if result_object
            .get("suite")
            .is_none_or(|value| value.is_null() || value.as_str().is_some_and(str::is_empty))
        {
            result_object.insert(
                "suite".to_owned(),
                Value::String("runner-evaluation".to_owned()),
            );
        }
        normalize_object_string_field(result_object, "suite", structured_summary_label);
        normalize_object_string_field(
            result_object,
            "failure_fingerprint",
            structured_summary_label,
        );
        normalize_object_string_field(result_object, "details", structured_summary_label);
        normalize_object_string_field(result_object, "evidence_ref", structured_summary_label);
        remove_null_object_fields(
            result_object,
            &["failure_fingerprint", "details", "evidence_ref"],
        );
    }
}

fn remove_null_object_fields(object: &mut Map<String, Value>, fields: &[&str]) {
    for field in fields {
        if object.get(*field).is_some_and(Value::is_null) {
            object.remove(*field);
        }
    }
}

fn compact_eval_object_passed(eval: &Value) -> bool {
    if eval
        .get("failure_fingerprint")
        .and_then(Value::as_str)
        .is_some_and(|fingerprint| !fingerprint.trim().is_empty())
    {
        return false;
    }

    let mut saw_positive = false;
    if let Some(passed) = eval.get("passed").and_then(Value::as_bool) {
        if !passed {
            return false;
        }
        saw_positive = true;
    }

    for key in [
        "outcome",
        "recommendation",
        "status",
        "result",
        "verdict",
        "review",
        "review_verdict",
        "evaluator",
        "evaluator_recommendation",
    ] {
        let Some(value) = eval.get(key).and_then(Value::as_str) else {
            continue;
        };
        match compact_eval_status(value) {
            Some(false) => return false,
            Some(true) => saw_positive = true,
            None => {}
        }
    }

    let mut saw_command = false;
    let mut all_commands_passed = true;
    if let Some(commands) = eval.get("commands").and_then(Value::as_array) {
        for command in commands {
            saw_command = true;
            match command
                .get("status")
                .and_then(Value::as_str)
                .and_then(compact_eval_status)
            {
                Some(false) => return false,
                Some(true) => {}
                None => all_commands_passed = false,
            }
        }
    }

    saw_positive || (saw_command && all_commands_passed)
}

fn compact_eval_status(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "accept" | "accepted" | "pass" | "passed" | "success" | "succeeded" => Some(true),
        "fail" | "failed" | "failure" | "error" | "reject" | "rejected" | "needs_repair"
        | "needs-repair" | "blocked" | "blocker" => Some(false),
        _ => None,
    }
}

fn normalize_string_array_field(
    object: &mut Map<String, Value>,
    field: &str,
    formatter: fn(&Value) -> String,
) {
    let Some(value) = object.get_mut(field) else {
        return;
    };
    match value {
        Value::Array(items) => {
            for item in items {
                if !item.is_string() {
                    *item = Value::String(formatter(item));
                }
            }
        }
        other if !other.is_string() => {
            *other = Value::Array(vec![Value::String(formatter(other))]);
        }
        Value::String(_) => {
            let item = value.take();
            *value = Value::Array(vec![item]);
        }
        _ => {}
    }
}

fn normalize_object_string_field(
    object: &mut Map<String, Value>,
    field: &str,
    formatter: fn(&Value) -> String,
) {
    let Some(value) = object.get_mut(field) else {
        return;
    };
    if !value.is_null() && !value.is_string() {
        *value = Value::String(formatter(value));
    }
}

fn structured_agent_label(value: &Value) -> String {
    let Some(object) = value.as_object() else {
        return structured_summary_label(value);
    };
    let name = object
        .get("name")
        .or_else(|| object.get("agent"))
        .or_else(|| object.get("role"))
        .or_else(|| object.get("id"))
        .and_then(Value::as_str);
    let session = object
        .get("session_id")
        .or_else(|| object.get("session"))
        .and_then(Value::as_str);
    match (name, session) {
        (Some(name), Some(session)) => format!("{name}:{session}"),
        (Some(name), None) => name.to_owned(),
        _ => structured_summary_label(value),
    }
}

fn structured_changed_file_label(value: &Value) -> String {
    let Some(object) = value.as_object() else {
        return structured_summary_label(value);
    };
    let Some(path) = object
        .get("path")
        .or_else(|| object.get("file"))
        .or_else(|| object.get("filepath"))
        .and_then(Value::as_str)
    else {
        return structured_summary_label(value);
    };
    let start = object
        .get("start")
        .or_else(|| object.get("start_line"))
        .or_else(|| object.get("line_start"))
        .and_then(Value::as_u64);
    let end = object
        .get("end")
        .or_else(|| object.get("end_line"))
        .or_else(|| object.get("line_end"))
        .and_then(Value::as_u64);
    match (start, end) {
        (Some(start), Some(end)) => format!("{path}:{start}-{end}"),
        (Some(line), None) | (None, Some(line)) => format!("{path}:{line}"),
        _ => object
            .get("line_span")
            .and_then(Value::as_str)
            .map(|span| format!("{path}:{span}"))
            .unwrap_or_else(|| path.to_owned()),
    }
}

fn structured_stage_label(value: &Value) -> String {
    value
        .as_object()
        .and_then(|object| {
            object
                .get("stage")
                .or_else(|| object.get("name"))
                .or_else(|| object.get("type"))
                .and_then(Value::as_str)
        })
        .map(str::to_owned)
        .unwrap_or_else(|| structured_summary_label(value))
}

fn structured_summary_label(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.to_owned();
    }
    if let Some(object) = value.as_object()
        && let Some(text) = object
            .get("summary")
            .or_else(|| object.get("message"))
            .or_else(|| object.get("description"))
            .or_else(|| object.get("name"))
            .or_else(|| object.get("risk"))
            .or_else(|| object.get("ref"))
            .or_else(|| object.get("path"))
            .and_then(Value::as_str)
    {
        return text.to_owned();
    }
    value.to_string()
}

fn canonical_handoff_stage(stage: &str) -> Option<&'static str> {
    match stage {
        "planning" | "implementation" | "repair" | "failure_analysis" | "commit"
        | "commit_push" | "final_commit" | "final_push" => Some("running"),
        "repair_intake" | "base_fetch" | "merge_origin_master" | "conflict_resolution" | "push" => {
            Some("running")
        }
        "git_closure_repair" => Some("running"),
        "verification" | "evaluation" | "final_verification" | "final_evaluation" => Some("eval"),
        "review" | "code_review" | "final_review" => Some("review"),
        "handoff" | "final_handoff" => Some("handoff"),
        "completed" => Some("completed"),
        "failed" => Some("failed"),
        "starting" => Some("starting"),
        "running" => Some("running"),
        "eval" => Some("eval"),
        "silent" => Some("silent"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::runner::{RunnerHandoff, RunnerStage, RunnerStopReason};

    #[test]
    fn normalizes_runner_orchestrator_handoff_dialect() {
        let mut value = json!({
            "schema_version": "symphony-runner-handoff/v1",
            "status": "completed",
            "session_id": "ses_test",
            "task_id": "task-1",
            "lifecycle_stages": [
                "planning", "implementation", "verification", "review", "repair",
                "evaluation", "commit", "push", "handoff"
            ],
            "subagents_used": ["rust-engineer", "code-reviewer", "evaluator"],
            "eval_results": {
                "outcome": "accept",
                "review_verdict": "pass",
                "evaluator_recommendation": "accept",
                "details": "review and evaluator passed",
                "commands": [{"command": "cargo test", "status": "pass"}]
            },
            "changed_files": ["src/lib.rs:1-10"],
            "git": {
                "branch": "feature/test",
                "head_sha": "0123456789abcdef",
                "worktree_path": "/tmp/worktree",
                "remote": "origin",
                "remote_ref": "refs/heads/feature/test",
                "pushed": true
            },
            "recall": {"canonical_evidence_workspace": "/home/agent/proj/recall"},
            "risks": [],
            "stop_reason": "accepted"
        });

        normalize_handoff_sidecar_value(&mut value, "/tmp/worktree");
        let handoff: RunnerHandoff =
            serde_json::from_value(value).expect("normalized handoff should parse");

        assert_eq!(handoff.session_id, "ses_test");
        assert_eq!(
            handoff.lifecycle_stages,
            vec![
                RunnerStage::Running,
                RunnerStage::Running,
                RunnerStage::Eval,
                RunnerStage::Review,
                RunnerStage::Running,
                RunnerStage::Eval,
                RunnerStage::Running,
                RunnerStage::Running,
                RunnerStage::Handoff,
            ]
        );
        assert_eq!(
            handoff.subagents,
            ["rust-engineer", "code-reviewer", "evaluator"]
        );
        assert!(handoff.eval_results[0].passed);
        assert_eq!(handoff.stop_reason, RunnerStopReason::Success);
        let git = handoff.git.expect("git evidence");
        assert_eq!(git.branch, "feature/test");
        assert_eq!(git.head_sha.as_deref(), Some("0123456789abcdef"));
    }

    #[test]
    fn normalizes_compact_review_and_evaluator_eval_object_as_passed() {
        let mut value = json!({
            "status": "completed",
            "session_id": "ses_nrv_30",
            "lifecycle_stages": [
                "planning", "implementation", "verification", "review", "repair",
                "verification", "review", "evaluation", "commit", "push"
            ],
            "subagents_used": ["rust-engineer", "code-reviewer", "evaluator"],
            "eval_results": {
                "verification": [
                    "cargo fmt --all -- --check",
                    "cargo check -p nervure-types",
                    "cargo nextest run -p nervure-types (348 passed)",
                    "cargo run -p nervure-cli -- generate-schemas --out schemas",
                    "git diff --check"
                ],
                "review": "pass",
                "evaluator": "accept"
            },
            "changed_files": [
                "crates/nervure-types/src/lib.rs:16-17",
                "schemas/pi-companion-profile.schema.json:1-379"
            ],
            "git": {
                "branch": "feature/nrv-30-define-pi-oh-my-pi-companion-integration-profile",
                "commit": "dcc20c501b62fe3a7b5f368287b33985040a58da",
                "pushed": true,
                "remote": "origin"
            },
            "recall": {"task_id": "3d0058f5-49bf-45df-a63f-2e3c779a7f6f"},
            "risks": [],
            "stop_reason": "accepted"
        });

        normalize_handoff_sidecar_value(&mut value, "/tmp/nrv-30");
        let handoff: RunnerHandoff =
            serde_json::from_value(value).expect("compact eval handoff should parse");

        assert_eq!(handoff.session_id, "ses_nrv_30");
        assert_eq!(handoff.stop_reason, RunnerStopReason::Success);
        assert_eq!(handoff.eval_results.len(), 1);
        assert!(handoff.eval_results[0].passed);
        assert_eq!(handoff.eval_results[0].suite, "runner-evaluation");
        assert_eq!(
            handoff.eval_results[0].details.as_deref(),
            Some(
                "cargo fmt --all -- --check\ncargo check -p nervure-types\ncargo nextest run -p nervure-types (348 passed)\ncargo run -p nervure-cli -- generate-schemas --out schemas\ngit diff --check"
            )
        );
        let git = handoff.git.expect("git evidence");
        assert_eq!(
            git.head_sha.as_deref(),
            Some("dcc20c501b62fe3a7b5f368287b33985040a58da")
        );
        assert_eq!(git.worktree_path, "/tmp/nrv-30");
    }

    #[test]
    fn derives_success_stop_reason_from_status_when_stop_reason_is_absent() {
        let mut value = json!({
            "status": "completed",
            "session_id": "ses_test",
            "lifecycle_stages": ["completed"],
            "subagents": [],
            "eval_results": [],
            "changed_files": [],
            "git": {
                "branch": "feature/test",
                "head_sha": null,
                "worktree_path": "/tmp/worktree"
            },
            "risks": []
        });

        normalize_handoff_sidecar_value(&mut value, "/tmp/worktree");
        let handoff: RunnerHandoff =
            serde_json::from_value(value).expect("status-derived handoff should parse");

        assert_eq!(handoff.stop_reason, RunnerStopReason::Success);
    }

    #[test]
    fn normalizes_structured_string_fields_from_runner_handoff() {
        let mut value = json!({
            "session_id": "ses_structured",
            "lifecycle_stages": [{"stage": "final_review"}, "completed"],
            "subagents": [
                {"agent": "rust-engineer", "session_id": "ses_child"},
                {"summary": "evaluator"}
            ],
            "eval_results": [{
                "suite": {"name": "runner-evaluation"},
                "passed": true,
                "failure_fingerprint": null,
                "details": {"summary": "all validation passed"},
                "evidence_ref": {"path": "docs/evidence.md"}
            }],
            "changed_files": [
                {"path": "src/lib.rs", "start_line": 1, "end_line": 20},
                {"path": "scripts/tools/gate.py", "line_span": "1-771"}
            ],
            "git": {
                "branch": {"ref": "feature/mne-215"},
                "head_sha": {"ref": "0123456789abcdef"},
                "worktree_path": {"path": "/tmp/worktree"},
                "pr_url": {"ref": "https://example.test/pr/1"},
                "commit_message": "test: commit message should be ignored"
            },
            "risks": [{"risk": "none"}],
            "stop_reason": {"type": "success"}
        });

        normalize_handoff_sidecar_value(&mut value, "/tmp/worktree");
        let handoff: RunnerHandoff =
            serde_json::from_value(value).expect("structured string fields should parse");

        assert_eq!(
            handoff.lifecycle_stages,
            vec![RunnerStage::Review, RunnerStage::Completed]
        );
        assert_eq!(handoff.subagents, ["rust-engineer:ses_child", "evaluator"]);
        assert_eq!(handoff.eval_results[0].suite, "runner-evaluation");
        assert_eq!(
            handoff.eval_results[0].details.as_deref(),
            Some("all validation passed")
        );
        assert_eq!(
            handoff.eval_results[0].evidence_ref.as_deref(),
            Some("docs/evidence.md")
        );
        assert_eq!(
            handoff.changed_files,
            ["src/lib.rs:1-20", "scripts/tools/gate.py:1-771"]
        );
        assert_eq!(handoff.risks, ["none"]);
        let git = handoff.git.expect("git evidence");
        assert_eq!(git.branch, "feature/mne-215");
        assert_eq!(git.head_sha.as_deref(), Some("0123456789abcdef"));
        assert_eq!(git.worktree_path, "/tmp/worktree");
        assert_eq!(git.pr_url.as_deref(), Some("https://example.test/pr/1"));
    }

    #[test]
    fn compact_eval_explicit_passed_false_takes_precedence() {
        assert!(!compact_eval_object_passed(&json!({
            "passed": false,
            "outcome": "accept",
            "commands": [{"status": "pass"}]
        })));
    }

    #[test]
    fn compact_eval_failure_fingerprint_takes_precedence() {
        assert!(!compact_eval_object_passed(&json!({
            "passed": true,
            "outcome": "accept",
            "failure_fingerprint": "tests-failed",
            "commands": [{"status": "pass"}]
        })));
    }
}
