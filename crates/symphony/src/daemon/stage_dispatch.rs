use std::error::Error as StdError;

use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::{
    config::{ProjectConfig, WorkflowStage},
    linear::{LinearClient, LinearIssue, LinearIssueEvidence},
    runner::{
        ProcessTreeTerminationEvidence, RunnerLaunchObserver, RunnerLauncher, RunnerProcessStarted,
        RunnerSessionCreated, RunnerStartedSession, build_acp_launch_spec_for_stage,
        new_session_record_for_stage,
    },
    state::{
        BlockerRecord, CleanupStatus, FailureRecord, LifecycleStage, RunnerSessionRecord,
        RunnerStage, StageInvocationRecord,
    },
    storage::SqliteStore,
};

use super::{OrchestrationReport, transition_issue_to_stage};
use super::{
    records::issue_record,
    self_defects::{RuntimeSelfDefectInput, record_runtime_self_defect},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum DispatchCandidate {
    Promote(LinearIssue),
    StageEntry(LinearIssue),
}

impl DispatchCandidate {
    pub(super) const fn issue(&self) -> &LinearIssue {
        match self {
            Self::Promote(issue) | Self::StageEntry(issue) => issue,
        }
    }

    pub(super) const fn order(&self) -> u8 {
        match self {
            Self::StageEntry(_) => 0,
            Self::Promote(_) => 1,
        }
    }
}

pub(super) fn stage_invocation_is_open(invocation: &StageInvocationRecord) -> bool {
    !matches!(
        invocation.status.as_str(),
        "left_stage" | "completed" | "canceled"
    )
}

pub(super) fn labels_hash(labels: &[String]) -> String {
    let mut labels = labels.iter().map(String::as_str).collect::<Vec<_>>();
    labels.sort_unstable();
    stable_hash_values("labels-v1", labels)
}

pub(super) fn blockers_hash(issue: &LinearIssue) -> String {
    let mut blockers = issue
        .blocked_by
        .iter()
        .map(|blocker| {
            format!(
                "{}\u{1f}{}\u{1f}{}",
                blocker.id.as_deref().unwrap_or_default(),
                blocker.identifier.as_deref().unwrap_or_default(),
                blocker.state.as_deref().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>();
    blockers.sort_unstable();
    stable_hash_owned_values("blockers-v1", &blockers)
}

pub(super) async fn dispatch_candidate(
    project: &ProjectConfig,
    self_defect_project: &ProjectConfig,
    store: &SqliteStore,
    linear: &impl LinearClient,
    runner: &impl RunnerLauncher,
    candidate: DispatchCandidate,
    report: &mut OrchestrationReport,
) -> anyhow::Result<()> {
    let issue = candidate.issue();
    if matches!(candidate, DispatchCandidate::Promote(_)) {
        promote_todo_issue(project, store, linear, issue).await?;
        return Ok(());
    }

    launch_stage_entry(
        project,
        self_defect_project,
        store,
        linear,
        runner,
        candidate,
        report,
    )
    .await
}

async fn promote_todo_issue(
    project: &ProjectConfig,
    store: &SqliteStore,
    linear: &impl LinearClient,
    issue: &LinearIssue,
) -> anyhow::Result<()> {
    info!(
        project_id = %project.id,
        issue = %issue.identifier,
        "promoting eligible Todo issue to In Progress"
    );
    transition_issue_to_stage(linear, project, &issue.id, WorkflowStage::InProgress).await?;
    let existing = store.issue(&project.id, &issue.id).await?;
    let mut record = issue_record(
        project,
        issue,
        LifecycleStage::Queued,
        None,
        CleanupStatus::Clean,
    );
    if let Some(existing) = existing {
        record.failure = existing.failure;
        record.git_ref = existing.git_ref;
        record.cleanup_status = existing.cleanup_status;
    }
    store.upsert_issue(&record).await?;
    Ok(())
}

async fn launch_stage_entry(
    project: &ProjectConfig,
    self_defect_project: &ProjectConfig,
    store: &SqliteStore,
    linear: &impl LinearClient,
    runner: &impl RunnerLauncher,
    candidate: DispatchCandidate,
    report: &mut OrchestrationReport,
) -> anyhow::Result<()> {
    let DispatchCandidate::StageEntry(issue) = candidate else {
        unreachable!("Todo promotion returns before runner launch")
    };
    let Some(stage) = executable_stage(project, &issue) else {
        return Ok(());
    };
    let launch_spec = build_acp_launch_spec_for_stage(project, &issue, stage);
    let stage_fingerprint = stage_invocation_fingerprint(project, &issue, stage);
    if store
        .stage_invocation(&project.id, &issue.id, &stage_fingerprint)
        .await?
        .is_some()
    {
        info!(
            project_id = %project.id,
            issue = %issue.identifier,
            stage_fingerprint = %stage_fingerprint,
            "stage-entry invocation already recorded; skipping duplicate runner launch"
        );
        return Ok(());
    }

    info!(
        project_id = %project.id,
        issue = %issue.identifier,
        "dispatching issue to runner"
    );
    let existing_record = store.issue(&project.id, &issue.id).await?;
    let mut record = issue_record(
        project,
        &issue,
        LifecycleStage::Running,
        None,
        CleanupStatus::Clean,
    );
    if let Some(existing) = &existing_record {
        record.failure.clone_from(&existing.failure);
        record.git_ref.clone_from(&existing.git_ref);
        record.cleanup_status = existing.cleanup_status;
    }
    store.upsert_issue(&record).await?;

    let invocation =
        stage_invocation_record(project, &issue, &launch_spec, stage, &stage_fingerprint);
    if !store.insert_stage_invocation_if_absent(&invocation).await? {
        info!(
            project_id = %project.id,
            issue = %issue.identifier,
            stage_fingerprint = %stage_fingerprint,
            "stage-entry invocation was concurrently recorded; skipping duplicate runner launch"
        );
        return Ok(());
    }

    let observer = RuntimeLaunchObserver::new(project, &issue, &launch_spec, stage, store);
    match runner.launch_observed(&launch_spec, &observer).await {
        Ok(started) => {
            let session =
                new_session_record_for_stage(project, &issue, started, &launch_spec, stage);
            info!(
                project_id = %project.id,
                issue = %issue.identifier,
                session_id = %session.session_id,
                worktree_path = %session.worktree_path,
                "runner session recorded"
            );
            store
                .mark_stage_invocation_started(
                    &project.id,
                    &issue.id,
                    &stage_fingerprint,
                    &session.session_id,
                )
                .await?;
            upsert_observed_launch_session(store, session).await?;
            report.dispatched.push(issue.identifier);
        }
        Err(error) => {
            store
                .mark_stage_invocation_status(
                    &project.id,
                    &issue.id,
                    &stage_fingerprint,
                    "launch_failed",
                )
                .await?;
            handle_launch_failure(
                project,
                self_defect_project,
                store,
                linear,
                &issue,
                &launch_spec,
                error,
            )
            .await?;
        }
    }
    Ok(())
}

fn executable_stage(project: &ProjectConfig, issue: &LinearIssue) -> Option<WorkflowStage> {
    match project.workflow.stage_for_linear_state(&issue.state) {
        Some(stage @ (WorkflowStage::InProgress | WorkflowStage::InReview)) => Some(stage),
        _ => None,
    }
}

fn stage_invocation_fingerprint(
    project: &ProjectConfig,
    issue: &LinearIssue,
    stage: WorkflowStage,
) -> String {
    stable_hash(&[
        "stage-entry-v2",
        stage.as_str(),
        &project.id,
        &issue.id,
        issue.state_id.as_deref().unwrap_or_default(),
        &issue.state,
        issue.updated_at.as_deref().unwrap_or_default(),
    ])
}

fn stage_invocation_record(
    project: &ProjectConfig,
    issue: &LinearIssue,
    launch_spec: &crate::runner::RunnerLaunchSpec,
    stage: WorkflowStage,
    fingerprint: &str,
) -> StageInvocationRecord {
    let route = project.workflow.agent_route_for_stage(stage, &issue.labels);
    StageInvocationRecord {
        project_id: project.id.clone(),
        issue_id: issue.id.clone(),
        fingerprint: fingerprint.to_owned(),
        state_id: issue.state_id.clone(),
        state_name: issue.state.clone(),
        issue_updated_at: issue.updated_at.clone(),
        labels_hash: labels_hash(&issue.labels),
        blockers_hash: blockers_hash(issue),
        selected_agent: launch_spec.agent.clone(),
        agent_routing_reason: route
            .as_ref()
            .map(|route| route.reason.as_str().to_owned())
            .unwrap_or_else(|| "fallback".into()),
        agent_routing_label: route.and_then(|route| route.selected_label),
        provider: launch_spec
            .provider_id
            .clone()
            .unwrap_or_else(|| launch_spec.provider_mode.as_str().to_owned()),
        session_id: None,
        status: "reserved".into(),
        created_at: None,
        updated_at: None,
    }
}

async fn upsert_observed_launch_session(
    store: &SqliteStore,
    mut session: RunnerSessionRecord,
) -> anyhow::Result<()> {
    if let Some(existing) = store
        .runner_session(&session.project_id, &session.issue_id, &session.session_id)
        .await?
        && is_observed_launch_marker(existing.lifecycle_marker.as_deref())
    {
        session.lifecycle_marker = existing.lifecycle_marker;
        session.last_event = existing.last_event;
    }

    store.upsert_runner_session(&session).await?;
    Ok(())
}

fn is_observed_launch_marker(marker: Option<&str>) -> bool {
    matches!(marker, Some("acp_process_started" | "acp_session_attached"))
}

struct RuntimeLaunchObserver<'a> {
    project: &'a ProjectConfig,
    issue: &'a LinearIssue,
    launch_spec: &'a crate::runner::RunnerLaunchSpec,
    stage: WorkflowStage,
    store: &'a SqliteStore,
    provisional_session_id: Mutex<Option<String>>,
}

impl<'a> RuntimeLaunchObserver<'a> {
    fn new(
        project: &'a ProjectConfig,
        issue: &'a LinearIssue,
        launch_spec: &'a crate::runner::RunnerLaunchSpec,
        stage: WorkflowStage,
        store: &'a SqliteStore,
    ) -> Self {
        Self {
            project,
            issue,
            launch_spec,
            stage,
            store,
            provisional_session_id: Mutex::new(None),
        }
    }
}

#[async_trait::async_trait]
impl RunnerLaunchObserver for RuntimeLaunchObserver<'_> {
    async fn process_started(
        &self,
        event: RunnerProcessStarted,
    ) -> Result<(), crate::runner::RunnerError> {
        let session_id = provisional_session_id(self.issue, event.process_id);
        {
            let mut provisional_session_id = self.provisional_session_id.lock().await;
            *provisional_session_id = Some(session_id.clone());
        }

        let mut session = new_session_record_for_stage(
            self.project,
            self.issue,
            RunnerStartedSession {
                session_id,
                process_id: event.process_id,
                acp_frame_count: 0,
                session_evidence_refs: Vec::new(),
            },
            self.launch_spec,
            self.stage,
        );
        session.lifecycle_marker = Some("acp_process_started".into());
        session.last_event = Some(
            event
                .process_id
                .map(|process_id| format!("acp_process_started:{process_id}"))
                .unwrap_or_else(|| "acp_process_started:no_pid".into()),
        );
        self.store
            .upsert_runner_session(&session)
            .await
            .map_err(|error| crate::runner::RunnerError::LaunchObserver(error.to_string()))
    }

    async fn session_created(
        &self,
        event: RunnerSessionCreated,
    ) -> Result<(), crate::runner::RunnerError> {
        let provisional_session_id = {
            let mut provisional_session_id = self.provisional_session_id.lock().await;
            provisional_session_id.take()
        };
        if let Some(session_id) = provisional_session_id {
            self.store
                .delete_runner_session(&self.project.id, &self.issue.id, &session_id)
                .await
                .map_err(|error| crate::runner::RunnerError::LaunchObserver(error.to_string()))?;
        }

        let mut session = new_session_record_for_stage(
            self.project,
            self.issue,
            RunnerStartedSession {
                session_id: event.session_id,
                process_id: event.process_id,
                acp_frame_count: 0,
                session_evidence_refs: Vec::new(),
            },
            self.launch_spec,
            self.stage,
        );
        session.lifecycle_marker = Some("acp_session_attached".into());
        session.last_event = Some(
            event
                .process_id
                .map(|process_id| format!("acp_session_attached:{process_id}"))
                .unwrap_or_else(|| "acp_session_attached:no_pid".into()),
        );
        self.store
            .upsert_runner_session(&session)
            .await
            .map_err(|error| crate::runner::RunnerError::LaunchObserver(error.to_string()))
    }
}

fn provisional_session_id(issue: &LinearIssue, process_id: Option<u32>) -> String {
    process_id
        .map(|process_id| format!("starting:{}:{process_id}", issue.identifier))
        .unwrap_or_else(|| format!("starting:{}:no_pid", issue.identifier))
}

async fn handle_launch_failure(
    project: &ProjectConfig,
    self_defect_project: &ProjectConfig,
    store: &SqliteStore,
    linear: &impl LinearClient,
    issue: &LinearIssue,
    launch_spec: &crate::runner::RunnerLaunchSpec,
    error: crate::runner::RunnerError,
) -> anyhow::Result<()> {
    let failure_reason = error_chain(&error);
    let occurrence_count = launch_failure_occurrence_count(store, project, issue).await?;
    warn!(
        project_id = %project.id,
        issue_id = %issue.id,
        issue = %issue.identifier,
        worktree_path = %launch_spec.cwd.display(),
        expected_branch = %launch_spec.branch_name,
        failure_reason = %failure_reason,
        "runner launch failed after Linear transition"
    );
    linear
        .record_issue_evidence(
            &issue.id,
            LinearIssueEvidence {
                kind: "runtime_defect".into(),
                body: launch_failure_evidence_body(issue, launch_spec, &failure_reason),
            },
        )
        .await?;

    let failure = FailureRecord {
        kind: "runtime_defect".into(),
        message: failure_reason,
        fingerprint: Some("launch_failed".into()),
        occurrence_count,
    };
    let mut record = issue_record(
        project,
        issue,
        LifecycleStage::Failed,
        Some(BlockerRecord {
            kind: "runtime_defect".into(),
            message: "runner launch failed after Linear transition".into(),
            observed_at: issue.updated_at.clone(),
        }),
        CleanupStatus::Clean,
    );
    record.failure = Some(failure.clone());
    store.upsert_issue(&record).await?;
    let session = launch_failure_session(project, issue, launch_spec, &error);
    record_runtime_self_defect(
        project,
        self_defect_project,
        store,
        linear,
        RuntimeSelfDefectInput {
            issue,
            evidence_kind: "runtime_defect",
            message: "runner launch failed after Linear transition",
            failure: &failure,
            session: &session,
        },
    )
    .await?;
    transition_issue_to_stage(linear, project, &issue.id, WorkflowStage::Todo).await?;
    if matches!(
        launch_spec.provider_mode,
        crate::state::RuntimeProviderMode::OmpAcp
    ) || matches!(error, crate::runner::RunnerError::AcpSetupFailed { .. })
    {
        store.upsert_runner_session(&session).await?;
    }
    Ok(())
}

async fn launch_failure_occurrence_count(
    store: &SqliteStore,
    project: &ProjectConfig,
    issue: &LinearIssue,
) -> anyhow::Result<u32> {
    let previous_count = store
        .issue(&project.id, &issue.id)
        .await?
        .and_then(|record| record.failure)
        .filter(|failure| failure.fingerprint.as_deref() == Some("launch_failed"))
        .map(|failure| failure.occurrence_count.max(1))
        .unwrap_or(0);
    Ok(previous_count.saturating_add(1))
}

fn launch_failure_session(
    project: &ProjectConfig,
    issue: &LinearIssue,
    launch_spec: &crate::runner::RunnerLaunchSpec,
    error: &crate::runner::RunnerError,
) -> RunnerSessionRecord {
    let route = project
        .workflow
        .agent_route_for_stage(WorkflowStage::InProgress, &issue.labels);
    let agent_routing_reason = route
        .as_ref()
        .map(|route| route.reason.as_str().to_owned())
        .unwrap_or_else(|| "fallback".into());
    let agent_routing_label = route.and_then(|route| route.selected_label);
    setup_failure_session(project, issue, launch_spec, error).unwrap_or_else(|| {
        RunnerSessionRecord {
            project_id: project.id.clone(),
            issue_id: issue.id.clone(),
            session_id: format!("launch-failed:{}", issue.identifier),
            provider_mode: launch_spec.provider_mode,
            provider_id: launch_spec.provider_id.clone(),
            agent: launch_spec.agent.clone(),
            agent_routing_reason,
            agent_routing_label,
            model: launch_spec.model.clone(),
            worktree_path: launch_spec.cwd.display().to_string(),
            process_id: None,
            lifecycle_stage: LifecycleStage::Failed,
            stage: RunnerStage::Failed,
            active_agent: Some(launch_spec.agent.clone()),
            active_model: launch_spec.model.clone(),
            message_count: 0,
            todo_count: 0,
            part_count: 0,
            token_count: 0,
            tokens_input: 0,
            tokens_output: 0,
            tokens_reasoning: 0,
            tokens_cache_read: 0,
            tokens_cache_write: 0,
            tokens_reported_total: 0,
            token_usage_status: "missing".into(),
            token_usage_source: "none".into(),
            cost_micros: 0,
            subagent_count: 0,
            eval_stage: None,
            lifecycle_marker: Some("launch_failed".into()),
            last_event: Some("launch_failed".into()),
            runtime_failure_kind: launch_failure_kind(error),
            acp_frame_count: 0,
            session_evidence_refs: Vec::new(),
            silence_observed: false,
        }
    })
}

fn setup_failure_session(
    project: &ProjectConfig,
    issue: &LinearIssue,
    launch_spec: &crate::runner::RunnerLaunchSpec,
    error: &crate::runner::RunnerError,
) -> Option<RunnerSessionRecord> {
    let crate::runner::RunnerError::AcpSetupFailed {
        process_id,
        session_id,
        reason,
        termination,
        ..
    } = error
    else {
        return None;
    };
    let session_id = session_id
        .clone()
        .unwrap_or_else(|| format!("setup-failed:{}", issue.identifier));
    let route = project
        .workflow
        .agent_route_for_stage(WorkflowStage::InProgress, &issue.labels);
    let agent_routing_reason = route
        .as_ref()
        .map(|route| route.reason.as_str().to_owned())
        .unwrap_or_else(|| "fallback".into());
    let agent_routing_label = route.and_then(|route| route.selected_label);
    Some(RunnerSessionRecord {
        project_id: project.id.clone(),
        issue_id: issue.id.clone(),
        session_id,
        provider_mode: launch_spec.provider_mode,
        provider_id: launch_spec.provider_id.clone(),
        agent: launch_spec.agent.clone(),
        agent_routing_reason,
        agent_routing_label,
        model: launch_spec.model.clone(),
        worktree_path: launch_spec.cwd.display().to_string(),
        process_id: *process_id,
        lifecycle_stage: LifecycleStage::Failed,
        stage: RunnerStage::Failed,
        active_agent: Some(launch_spec.agent.clone()),
        active_model: launch_spec.model.clone(),
        message_count: 0,
        todo_count: 0,
        part_count: 0,
        token_count: 0,
        tokens_input: 0,
        tokens_output: 0,
        tokens_reasoning: 0,
        tokens_cache_read: 0,
        tokens_cache_write: 0,
        tokens_reported_total: 0,
        token_usage_status: "missing".into(),
        token_usage_source: "none".into(),
        cost_micros: 0,
        subagent_count: 0,
        eval_stage: Some(project.eval.default_suite.clone()),
        lifecycle_marker: Some(format!("setup_failed:{reason}")),
        last_event: Some(setup_failure_last_event(*process_id, termination)),
        runtime_failure_kind: (launch_spec.provider_mode
            == crate::state::RuntimeProviderMode::OmpAcp)
            .then(|| crate::runner::classify_omp_acp_failure_kind(reason)),
        acp_frame_count: 0,
        session_evidence_refs: Vec::new(),
        silence_observed: false,
    })
}

fn launch_failure_kind(
    error: &crate::runner::RunnerError,
) -> Option<crate::state::RuntimeFailureKind> {
    match error {
        crate::runner::RunnerError::RuntimeFailure { kind, .. } => Some(kind.clone()),
        crate::runner::RunnerError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => {
            Some(crate::state::RuntimeFailureKind::MissingBinary)
        }
        _ => None,
    }
}

fn setup_failure_last_event(
    process_id: Option<u32>,
    termination: &ProcessTreeTerminationEvidence,
) -> String {
    let process = process_id
        .map(|pid| pid.to_string())
        .unwrap_or_else(|| "no_pid".into());
    format!(
        "setup_failed:{process}:term={}:kill={}:alive={}",
        termination.term_signal_sent, termination.kill_signal_sent, termination.still_alive
    )
}

fn launch_failure_evidence_body(
    issue: &LinearIssue,
    launch_spec: &crate::runner::RunnerLaunchSpec,
    failure_reason: &str,
) -> String {
    format!(
        "runtime_defect: launch_failed\nissue_id: {}\nissue_identifier: {}\nattempted_worktree_path: {}\nexpected_branch: {}\nelapsed_seconds: unknown\nfailure_reason: {}",
        issue.id,
        issue.identifier,
        launch_spec.cwd.display(),
        launch_spec.branch_name,
        failure_reason
    )
}

fn error_chain(error: &(dyn StdError + 'static)) -> String {
    let mut parts = vec![error.to_string()];
    let mut source = error.source();
    while let Some(error) = source {
        parts.push(error.to_string());
        source = error.source();
    }
    parts.join(": ")
}

fn stable_hash_values(prefix: &str, values: Vec<&str>) -> String {
    let mut hash = FNV_OFFSET_BASIS;
    update_stable_hash(&mut hash, prefix);
    for value in values {
        update_stable_hash(&mut hash, value);
    }
    format!("fnv1a64:{hash:016x}")
}

fn stable_hash_owned_values(prefix: &str, values: &[String]) -> String {
    let mut hash = FNV_OFFSET_BASIS;
    update_stable_hash(&mut hash, prefix);
    for value in values {
        update_stable_hash(&mut hash, value);
    }
    format!("fnv1a64:{hash:016x}")
}

fn stable_hash(values: &[&str]) -> String {
    let mut hash = FNV_OFFSET_BASIS;
    for value in values {
        update_stable_hash(&mut hash, value);
    }
    format!("fnv1a64:{hash:016x}")
}

fn update_stable_hash(hash: &mut u64, value: &str) {
    for byte in value.len().to_le_bytes() {
        *hash ^= u64::from(byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
    for byte in value.as_bytes() {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
