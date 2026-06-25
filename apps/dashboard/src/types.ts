export type LifecycleStage = "queued" | "running" | "blocked" | "completed" | "failed" | "canceled" | string;

export type DashboardMetadata = {
  polling_fallback_endpoint?: string;
  live_events_endpoint?: string;
};

export type ProjectCapacity = {
  max_sessions: number;
  running_sessions: number;
  available_sessions: number;
};

export type RuntimeLiveness = {
  status: string;
  reason: string;
  primary_reason_code: string;
  primary_reason_detail: string;
  last_poll_at?: string | null;
  last_successful_candidate_scan_at?: string | null;
  capacity: ProjectCapacity;
};

export type DashboardTokenMetrics = {
  accounted_total_token_count: number;
  non_cached_token_count: number;
  cached_token_count: number;
  input_token_count: number;
  output_token_count: number;
  reasoning_token_count: number;
  cache_read_token_count: number;
  cache_write_token_count: number;
  reported_total_token_count: number;
  metrics_status: string;
  metrics_source: string;
};

export type RunningIssueSummary = {
  project_id: string;
  project_name: string;
  issue_id: string;
  identifier: string;
  title: string;
  display_status: string;
  session_id?: string | null;
  preferred_runner_session_id?: string | null;
  provider_mode?: string | null;
  provider_id?: string | null;
  process_id?: number | null;
  process_alive?: boolean | null;
  lifecycle_stage?: LifecycleStage | null;
  stage?: string | null;
  agent?: string | null;
  model?: string | null;
  active_agent?: string | null;
  active_model?: string | null;
  token_count: number;
  cached_token_count?: number | null;
  token_metrics?: DashboardTokenMetrics;
  subagents_used: number;
  running_tool_count: number;
  pending_tool_count: number;
  todo_count: number;
  started_at_ms?: number | null;
  duration_ms?: number | null;
  last_event?: string | null;
  runtime_failure_kind?: string | null;
  acp_frame_count?: number;
  session_evidence_refs?: string[];
  silence_observed?: boolean;
  worktree_path?: string | null;
};

export type SelfDefectRouteSummary = {
  fingerprint: string;
  severity?: string | null;
  kind?: string | null;
  defect_kind?: string | null;
  relation?: string | null;
  relation_mode?: string | null;
  source_issue_id?: string | null;
  source_issue_identifier?: string | null;
  managed_issue_id?: string | null;
  managed_issue_identifier?: string | null;
  occurrence_count?: number | null;
  first_seen_at?: string | null;
  last_seen_at?: string | null;
  next_action?: string | null;
};

export type DashboardProjectCard = {
  project_id: string;
  name: string;
  enabled: boolean;
  active_count: number;
  parked_count: number;
  terminal_count: number;
  runner_health: string;
  last_event: string;
  capacity: ProjectCapacity;
  liveness: RuntimeLiveness;
  cleanup_status: string;
  running_tokens: number;
  running_cached_tokens?: number | null;
  token_metrics?: DashboardTokenMetrics;
  recorded_tokens: number;
  running_issues: RunningIssueSummary[];
  self_defect_routes?: SelfDefectRouteSummary[];
};

export type DashboardTotals = {
  project_count: number;
  enabled_project_count: number;
  running_issue_count: number;
  available_sessions: number;
  max_sessions: number;
  running_tokens: number;
  running_cached_tokens?: number | null;
  recorded_tokens: number;
  token_metrics?: DashboardTokenMetrics;
};

export type AggregateDashboard = {
  metadata?: DashboardMetadata;
  totals: DashboardTotals;
  projects: DashboardProjectCard[];
};

export type CandidateSuppression = {
  issue_id: string;
  identifier: string;
  reason_kind: string;
  reason: string;
};

export type SelectedCandidate = {
  issue_id: string;
  identifier: string;
  lifecycle_stage: LifecycleStage;
  reason: string;
};

export type BlockerRecord = {
  kind: string;
  message: string;
  observed_at?: string | null;
};

export type FailureRecord = {
  kind: string;
  message: string;
  fingerprint?: string | null;
  occurrence_count: number;
};

export type RuntimeDefect = {
  classification: string;
  fingerprint?: string | null;
  repair_attempt_count: number;
  next_action: string;
};

export type GitRef = {
  branch: string;
  worktree_path: string;
  head_sha?: string | null;
  pr_url?: string | null;
};

export type EvalRun = {
  run_id: string;
  suite: string;
  status: string;
  details_json?: string | null;
};

export type SessionActivity = {
  session_id: string;
  parent_session_id?: string | null;
  title: string;
  directory: string;
  agent?: string | null;
  model?: string | null;
  is_subagent: boolean;
  tokens_input: number;
  tokens_output: number;
  tokens_reasoning: number;
  tokens_cache_read: number;
  tokens_cache_write: number;
  time_created_ms: number;
  time_updated_ms: number;
};

export type TodoActivity = {
  session_id: string;
  content: string;
  status: string;
  priority: string;
  position: number;
  time_updated_ms: number;
};

export type TimelineEvent = {
  session_id: string;
  part_id: string;
  time_created_ms: number;
  time_updated_ms: number;
  kind: string;
  tool?: string | null;
  status?: string | null;
  title?: string | null;
  summary: string;
};

export type SessionTreeActivity = {
  root_session_id: string;
  sessions: SessionActivity[];
  subagents: SessionActivity[];
  todos: TodoActivity[];
  timeline: TimelineEvent[];
  running_tool_count: number;
  pending_tool_count: number;
  last_updated_ms?: number | null;
};

export type RunnerSession = {
  runner_session_id: string;
  provider_mode: string;
  provider_id?: string | null;
  agent: string;
  model?: string | null;
  worktree_path: string;
  process_id?: number | null;
  process_alive?: boolean | null;
  lifecycle_stage: LifecycleStage;
  current_stage: string;
  stage_history: string[];
  active_agent?: string | null;
  active_model?: string | null;
  subagents_used: number;
  eval_stage?: string | null;
  message_count: number;
  todo_count: number;
  part_count: number;
  token_count: number;
  cached_token_count?: number | null;
  token_metrics?: DashboardTokenMetrics;
  started_at_ms?: number | null;
  duration_ms?: number | null;
  last_event?: string | null;
  runtime_failure_kind?: string | null;
  acp_frame_count: number;
  session_evidence_refs: string[];
  silence_observed: boolean;
  activity?: SessionTreeActivity | null;
  activity_error?: string | null;
};

export type SelfDefectRouting = {
  fingerprint?: string | null;
  severity?: string | null;
  kind?: string | null;
  defect_kind?: string | null;
  relation?: string | null;
  relation_mode?: string | null;
  source_issue_id?: string | null;
  source_issue_identifier?: string | null;
  managed_issue_id?: string | null;
  managed_issue_identifier?: string | null;
  occurrence_count?: number | null;
  first_seen_at?: string | null;
  last_seen_at?: string | null;
  next_action?: string | null;
};

export type IssueDetail = {
  metadata?: DashboardMetadata;
  project_id: string;
  issue_id: string;
  identifier: string;
  title: string;
  lifecycle_stage: LifecycleStage;
  display_status: string;
  blocker?: BlockerRecord | null;
  failure?: FailureRecord | null;
  runtime_defect?: RuntimeDefect | null;
  self_defect_routing?: SelfDefectRouting | null;
  git_ref?: GitRef | null;
  cleanup_status: string;
  stop_reason?: string | null;
  last_runner_event?: string | null;
  preferred_runner_session_id?: string | null;
  runner_sessions: RunnerSession[];
  token_metrics?: DashboardTokenMetrics;
  eval_results: EvalRun[];
};

export type ProjectDetail = {
  metadata?: DashboardMetadata;
  project_id: string;
  name: string;
  enabled: boolean;
  lifecycle_stage: LifecycleStage;
  cleanup_status: string;
  capacity: ProjectCapacity;
  liveness: RuntimeLiveness;
  selected_candidate?: SelectedCandidate | null;
  suppression_reasons: CandidateSuppression[];
  active_issues: IssueDetail[];
  token_metrics?: DashboardTokenMetrics;
  history_issues: IssueDetail[];
};

export type DashboardUnavailable = {
  status: "unavailable";
  reason: string;
  message: string;
};
