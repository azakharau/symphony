pub(super) const CANDIDATE_ISSUES_QUERY: &str = r#"
query CandidateIssues($teamKey: String!, $projectId: ID!, $states: [String!], $after: String) {
  issues(
    filter: {
      team: { key: { eq: $teamKey } }
      project: { id: { eq: $projectId } }
      state: { name: { in: $states } }
    }
    first: 100
    after: $after
  ) {
    pageInfo {
      hasNextPage
      endCursor
    }
    nodes {
      id
      identifier
      title
      description
      state { name }
      priority
      branchName
      url
      projectMilestone { id name }
      labels { nodes { name } }
      comments(last: 50, orderBy: createdAt) {
        nodes {
          body
          parent { id }
          createdAt
        }
      }
      relations {
        nodes {
          type
          relatedIssue {
            id
            identifier
            state { name }
          }
        }
      }
      inverseRelations {
        nodes {
          type
          issue {
            id
            identifier
            state { name }
          }
        }
      }
      createdAt
      updatedAt
    }
  }
}
"#;

pub(super) const ISSUE_STATES_QUERY: &str = r#"
query IssueStates($issueId: String!) {
  issue(id: $issueId) {
    team {
      states {
        nodes {
          id
          name
        }
      }
    }
  }
}
"#;

pub(super) const UPDATE_ISSUE_STATE_MUTATION: &str = r#"
mutation UpdateIssueState($issueId: String!, $stateId: String!) {
  issueUpdate(id: $issueId, input: { stateId: $stateId }) {
    success
  }
}
"#;

pub(super) const CREATE_COMMENT_MUTATION: &str = r#"
mutation CreateIssueEvidence($issueId: String!, $body: String!) {
  commentCreate(input: { issueId: $issueId, body: $body }) {
    success
  }
}
"#;

pub(super) const TEAM_CREATE_CONTEXT_QUERY: &str = r#"
query TeamCreateContext($teamKey: String!) {
  teams(filter: { key: { eq: $teamKey } }, first: 1) {
    nodes {
      id
      states {
        nodes {
          id
          name
        }
      }
    }
  }
}
"#;

pub(super) const CREATE_MANAGED_ISSUE_MUTATION: &str = r#"
mutation CreateManagedIssue($input: IssueCreateInput!) {
  issueCreate(input: $input) {
    success
    issue {
      id
      identifier
      title
      description
      state { name }
      priority
      branchName
      url
      projectMilestone { id name }
      labels { nodes { name } }
      comments(last: 50, orderBy: createdAt) {
        nodes {
          body
          parent { id }
          createdAt
        }
      }
      relations {
        nodes {
          type
          relatedIssue {
            id
            identifier
            state { name }
          }
        }
      }
      inverseRelations {
        nodes {
          type
          issue {
            id
            identifier
            state { name }
          }
        }
      }
      createdAt
      updatedAt
    }
  }
}
"#;

pub(super) const CREATE_ISSUE_RELATION_MUTATION: &str = r#"
mutation CreateIssueRelation($issueId: String!, $relatedIssueId: String!, $type: IssueRelationType!) {
  issueRelationCreate(input: { issueId: $issueId, relatedIssueId: $relatedIssueId, type: $type }) {
    success
  }
}
"#;
