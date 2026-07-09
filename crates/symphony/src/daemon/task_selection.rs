use std::{cmp::Ordering, collections::HashSet};

use crate::{config::ProjectConfig, linear::LinearIssue, state::BlockerRecord};

use super::stage_dispatch::DispatchCandidate;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TaskClass {
    P0SelfBug,
    Product,
    PromotedSelfBug,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct DispatchSelection {
    pub project_index: usize,
    pub project_id: String,
    pub candidate: DispatchCandidate,
    class: TaskClass,
}

impl DispatchSelection {
    pub(super) fn new(
        project_index: usize,
        project: &ProjectConfig,
        self_defect_project_id: &str,
        candidate: DispatchCandidate,
    ) -> Self {
        let class = classify_issue(project, self_defect_project_id, candidate.issue());
        Self {
            project_index,
            project_id: project.id.clone(),
            candidate,
            class,
        }
    }

    pub(super) const fn issue(&self) -> &LinearIssue {
        self.candidate.issue()
    }
}

pub(super) fn compare_dispatch_selections(
    left: &DispatchSelection,
    right: &DispatchSelection,
) -> Ordering {
    left.candidate
        .order()
        .cmp(&right.candidate.order())
        .then_with(|| task_class_order(left.class).cmp(&task_class_order(right.class)))
        .then_with(|| {
            priority_order(left.issue().priority).cmp(&priority_order(right.issue().priority))
        })
        .then_with(|| left.project_index.cmp(&right.project_index))
        .then_with(|| left.issue().identifier.cmp(&right.issue().identifier))
        .then_with(|| left.issue().id.cmp(&right.issue().id))
}

pub(super) fn partition_ambiguous_milestone_promotions(
    promotions: Vec<DispatchCandidate>,
) -> (Vec<DispatchCandidate>, Vec<DispatchCandidate>, usize) {
    let runnable_milestones = promotions
        .iter()
        .filter_map(|candidate| candidate.issue().project_milestone.as_ref())
        .map(|milestone| milestone.id.as_str())
        .collect::<HashSet<_>>();
    let milestone_count = runnable_milestones.len();
    if milestone_count <= 1 {
        return (promotions, Vec::new(), milestone_count);
    }

    let (allowed, suppressed) = promotions
        .into_iter()
        .partition(|candidate| candidate.issue().project_milestone.is_none());
    (allowed, suppressed, milestone_count)
}

pub(super) fn self_bug_default_suppression(
    project: &ProjectConfig,
    issue: &LinearIssue,
) -> Option<BlockerRecord> {
    if !is_managed_self_defect_issue(issue) || issue.priority.unwrap_or(i64::MAX) <= 1 {
        return None;
    }
    if project
        .workflow
        .self_defect_execution_promoted(&issue.labels)
    {
        return None;
    }
    Some(BlockerRecord {
        kind: "managed_self_defect_policy".into(),
        message: "P1/P2 Symphony self-bugs are non-executable until owner or policy promotion"
            .into(),
        observed_at: issue.updated_at.clone(),
    })
}

pub(super) fn is_managed_self_defect_issue(issue: &LinearIssue) -> bool {
    issue.title.starts_with("Symphony self-defect:")
}

fn classify_issue(
    project: &ProjectConfig,
    self_defect_project_id: &str,
    issue: &LinearIssue,
) -> TaskClass {
    if project.id == self_defect_project_id && is_managed_self_defect_issue(issue) {
        if issue.priority.unwrap_or(i64::MAX) <= 1 {
            TaskClass::P0SelfBug
        } else {
            TaskClass::PromotedSelfBug
        }
    } else {
        TaskClass::Product
    }
}

fn task_class_order(class: TaskClass) -> u8 {
    match class {
        TaskClass::P0SelfBug => 0,
        TaskClass::Product => 1,
        TaskClass::PromotedSelfBug => 2,
    }
}

fn priority_order(priority: Option<i64>) -> (i64, i64) {
    priority.map_or((1, i64::MAX), |priority| (0, priority))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::ProjectWorkflow, linear::LinearIssue};
    fn issue(identifier: &str, priority: Option<i64>) -> LinearIssue {
        LinearIssue {
            id: identifier.to_lowercase(),
            identifier: identifier.into(),
            title: "Symphony self-defect: test".into(),
            description: None,
            state: "Todo".into(),
            state_id: None,
            priority,
            branch_name: None,
            url: None,
            labels: Vec::new(),
            project_milestone: None,
            blocked_by: Vec::new(),
            upstream_context: Vec::new(),
            has_new_owner_answer: false,
            owner_answer_created_at: None,
            created_at: None,
            updated_at: None,
        }
    }

    #[test]
    fn p1_self_bug_is_suppressed_until_explicit_label_promotion() {
        let mut project = ProjectConfig {
            id: "symphony".into(),
            name: "Symphony".into(),
            enabled: true,
            workflow_path: "/tmp/workflow".into(),
            repo_path: "/tmp/repo".into(),
            branch: crate::config::BranchPolicy {
                base: "main".into(),
                worktree_root: "/tmp/worktrees".into(),
            },
            linear: crate::linear::LinearProjectConfig {
                team_key: "SYM".into(),
                project_id: Some("linear-project".into()),
            },
            runner: crate::runner::RunnerRuntimeConfig {
                provider_mode: crate::state::RuntimeProviderMode::Acp,
                command: "runner".into(),
                args: Vec::new(),
                agent: "build".into(),
                model: None,
                effort: None,
                permission_policy: crate::runner::PermissionPolicy::Reject,
            },
            omp_acp_providers: Vec::new(),
            eval: crate::config::EvalDefaults {
                default_suite: "default".into(),
                max_identical_failure_fingerprints: 2,
            },
            concurrency: crate::config::ConcurrencyConfig { max_sessions: 1 },
            workflow: ProjectWorkflow::default(),
        };
        project.workflow.self_defects.executable_label = Some("self-defect-executable".into());
        let mut p1 = issue("SYM-1", Some(2));
        assert_eq!(
            self_bug_default_suppression(&project, &p1)
                .expect("suppressed")
                .kind,
            "managed_self_defect_policy"
        );

        p1.labels.push("self-defect-executable".into());
        assert!(self_bug_default_suppression(&project, &p1).is_none());
    }

    #[test]
    fn product_issue_is_not_suppressed_by_self_bug_policy() {
        let product = LinearIssue {
            title: "Product work".into(),
            project_milestone: Some(crate::linear::LinearMilestone {
                id: "milestone".into(),
                name: "Milestone".into(),
            }),
            ..issue("ALPHA-1", Some(1))
        };

        assert!(
            self_bug_default_suppression(
                &ProjectConfig {
                    id: "symphony".into(),
                    name: "Symphony".into(),
                    enabled: true,
                    workflow_path: "/tmp/workflow".into(),
                    repo_path: "/tmp/repo".into(),
                    branch: crate::config::BranchPolicy {
                        base: "main".into(),
                        worktree_root: "/tmp/worktrees".into(),
                    },
                    linear: crate::linear::LinearProjectConfig {
                        team_key: "SYM".into(),
                        project_id: Some("linear-project".into()),
                    },
                    runner: crate::runner::RunnerRuntimeConfig {
                        provider_mode: crate::state::RuntimeProviderMode::Acp,
                        command: "runner".into(),
                        args: Vec::new(),
                        agent: "build".into(),
                        model: None,
                        effort: None,
                        permission_policy: crate::runner::PermissionPolicy::Reject,
                    },
                    omp_acp_providers: Vec::new(),
                    eval: crate::config::EvalDefaults {
                        default_suite: "default".into(),
                        max_identical_failure_fingerprints: 2,
                    },
                    concurrency: crate::config::ConcurrencyConfig { max_sessions: 1 },
                    workflow: ProjectWorkflow::default(),
                },
                &product
            )
            .is_none()
        );
    }

    #[test]
    fn one_runnable_milestone_keeps_all_promotions() {
        let first = LinearIssue {
            project_milestone: Some(crate::linear::LinearMilestone {
                id: "milestone-a".into(),
                name: "Milestone A".into(),
            }),
            ..issue("SYM-1", Some(1))
        };
        let second = LinearIssue {
            project_milestone: first.project_milestone.clone(),
            ..issue("SYM-2", Some(2))
        };

        let (allowed, suppressed, milestone_count) =
            partition_ambiguous_milestone_promotions(vec![
                DispatchCandidate::Promote(first),
                DispatchCandidate::Promote(second),
            ]);

        assert_eq!(allowed.len(), 2);
        assert!(suppressed.is_empty());
        assert_eq!(milestone_count, 1);
    }

    #[test]
    fn multiple_runnable_milestones_suppress_only_milestone_work() {
        let first = LinearIssue {
            project_milestone: Some(crate::linear::LinearMilestone {
                id: "milestone-a".into(),
                name: "Milestone A".into(),
            }),
            ..issue("SYM-1", Some(1))
        };
        let second = LinearIssue {
            project_milestone: Some(crate::linear::LinearMilestone {
                id: "milestone-b".into(),
                name: "Milestone B".into(),
            }),
            ..issue("SYM-2", Some(2))
        };
        let self_defect = issue("SYM-3", Some(1));

        let (allowed, suppressed, milestone_count) =
            partition_ambiguous_milestone_promotions(vec![
                DispatchCandidate::Promote(first),
                DispatchCandidate::Promote(second),
                DispatchCandidate::Promote(self_defect),
            ]);

        assert_eq!(allowed.len(), 1);
        assert_eq!(allowed[0].issue().identifier, "SYM-3");
        assert_eq!(suppressed.len(), 2);
        assert_eq!(milestone_count, 2);
    }
}
