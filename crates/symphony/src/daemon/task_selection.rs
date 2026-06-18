use std::cmp::Ordering;

use crate::{config::ProjectConfig, linear::LinearIssue, state::BlockerRecord};

use super::DispatchCandidate;

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

    pub(super) const fn class(&self) -> TaskClass {
        self.class
    }
}

pub(super) fn compare_dispatch_selections(
    left: &DispatchSelection,
    right: &DispatchSelection,
) -> Ordering {
    task_class_order(left.class)
        .cmp(&task_class_order(right.class))
        .then_with(|| {
            priority_order(left.issue().priority).cmp(&priority_order(right.issue().priority))
        })
        .then_with(|| left.project_index.cmp(&right.project_index))
        .then_with(|| left.issue().identifier.cmp(&right.issue().identifier))
        .then_with(|| left.issue().id.cmp(&right.issue().id))
}

pub(super) fn self_bug_default_suppression(issue: &LinearIssue) -> Option<BlockerRecord> {
    if !is_managed_self_defect_issue(issue) || issue.priority.unwrap_or(i64::MAX) <= 1 {
        return None;
    }
    if self_bug_execution_promoted(issue) {
        return None;
    }
    Some(BlockerRecord {
        kind: "managed_self_defect_policy".into(),
        message: "P1/P2 Symphony self-bugs are non-executable until owner or policy promotion"
            .into(),
        observed_at: issue.updated_at.clone(),
    })
}

pub(super) fn p0_self_bug_preemption_suppression(issue: &LinearIssue) -> BlockerRecord {
    BlockerRecord {
        kind: "p0_self_bug_preemption".into(),
        message: "P0 Symphony self-bug preempted otherwise-runnable product work".into(),
        observed_at: issue.updated_at.clone(),
    }
}

pub(super) fn is_managed_self_defect_issue(issue: &LinearIssue) -> bool {
    issue.title.starts_with("Symphony self-defect:")
        || issue
            .description
            .as_deref()
            .is_some_and(|description| description.contains("symphony:managed-self-bug"))
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

fn self_bug_execution_promoted(issue: &LinearIssue) -> bool {
    issue
        .labels
        .iter()
        .any(|label| label == "symphony-self-bug-executable")
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
    use crate::linear::LinearIssue;

    fn issue(identifier: &str, priority: Option<i64>) -> LinearIssue {
        LinearIssue {
            id: identifier.to_lowercase(),
            identifier: identifier.into(),
            title: "Symphony self-defect: test".into(),
            description: None,
            state: "Todo".into(),
            priority,
            branch_name: None,
            url: None,
            labels: Vec::new(),
            project_milestone: None,
            blocked_by: Vec::new(),
            has_new_owner_answer: false,
            owner_answer_created_at: None,
            created_at: None,
            updated_at: None,
        }
    }

    #[test]
    fn p1_self_bug_is_suppressed_until_explicit_label_promotion() {
        let mut p1 = issue("SYM-1", Some(2));
        assert_eq!(
            self_bug_default_suppression(&p1).expect("suppressed").kind,
            "managed_self_defect_policy"
        );

        p1.labels.push("symphony-self-bug-executable".into());
        assert!(self_bug_default_suppression(&p1).is_none());
    }

    #[test]
    fn p0_preemption_suppression_is_not_default_self_bug_policy() {
        let product = LinearIssue {
            title: "Product work".into(),
            project_milestone: Some(crate::linear::LinearMilestone {
                id: "milestone".into(),
                name: "Milestone".into(),
            }),
            ..issue("ALPHA-1", Some(1))
        };

        let blocker = p0_self_bug_preemption_suppression(&product);

        assert_eq!(blocker.kind, "p0_self_bug_preemption");
        assert!(self_bug_default_suppression(&product).is_none());
    }
}
