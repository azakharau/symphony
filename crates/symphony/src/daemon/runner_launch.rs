use crate::{
    config::{ProjectConfig, WorkflowStage},
    linear::LinearIssue,
    runner::{
        RunnerLaunchSpec, RunnerStartedSession, build_acp_launch_spec,
        build_acp_launch_spec_for_stage,
    },
    state::{RunnerSessionRecord, RuntimeProviderMode},
};

pub(super) fn stage_aware_launch_spec(
    project: &ProjectConfig,
    issue: &LinearIssue,
) -> RunnerLaunchSpec {
    if project.runner.provider_mode != RuntimeProviderMode::Acp {
        return build_acp_launch_spec(project, issue);
    }

    let stage = project
        .workflow
        .stage_for_linear_state(&issue.state)
        .unwrap_or(WorkflowStage::InProgress);
    build_acp_launch_spec_for_stage(project, issue, stage)
}

pub(super) fn apply_acp_agent_metadata(session: &mut RunnerSessionRecord, spec: &RunnerLaunchSpec) {
    if spec.provider_mode == RuntimeProviderMode::Acp {
        session.agent.clone_from(&spec.agent);
        session.active_agent = Some(spec.agent.clone());
    }
}

pub(super) fn apply_started_session_metadata(
    session: &mut RunnerSessionRecord,
    spec: &RunnerLaunchSpec,
    started: &RunnerStartedSession,
) {
    session.process_id = started.process_id;
    apply_acp_agent_metadata(session, spec);
}
