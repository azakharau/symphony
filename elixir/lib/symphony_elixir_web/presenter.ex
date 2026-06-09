defmodule SymphonyElixirWeb.Presenter do
  @moduledoc """
  Shared projections for the observability API and dashboard.
  """

  alias SymphonyElixir.{Config, Orchestrator, ProjectRegistry, RootConfigStore, StatusDashboard}

  @spec state_payload(GenServer.name(), timeout()) :: map()
  def state_payload(orchestrator, snapshot_timeout_ms) do
    state_payload(orchestrator, snapshot_timeout_ms, nil)
  end

  @spec state_payload(GenServer.name(), timeout(), (-> map()) | nil) :: map()
  def state_payload(orchestrator, snapshot_timeout_ms, project_states_provider) do
    case project_states(project_states_provider) do
      nil -> single_state_payload(orchestrator, snapshot_timeout_ms)
      project_states -> aggregate_state_payload(project_states, snapshot_timeout_ms)
    end
  end

  defp single_state_payload(orchestrator, snapshot_timeout_ms) do
    generated_at = DateTime.utc_now() |> DateTime.truncate(:second) |> DateTime.to_iso8601()

    case Orchestrator.snapshot(orchestrator, snapshot_timeout_ms) do
      %{} = snapshot ->
        snapshot_payload(generated_at, snapshot)

      :timeout ->
        %{generated_at: generated_at, error: %{code: "snapshot_timeout", message: "Snapshot timed out"}}

      :unavailable ->
        %{generated_at: generated_at, error: %{code: "snapshot_unavailable", message: "Snapshot unavailable"}}
    end
  end

  @spec project_state_payload(String.t(), (-> map()) | nil, timeout()) :: {:ok, map()} | {:error, atom()}
  def project_state_payload(project_id, project_states_provider, snapshot_timeout_ms) when is_binary(project_id) do
    with {:ok, project_state} <- project_state(project_id, project_states_provider),
         {:ok, orchestrator} <- project_orchestrator(project_state) do
      case single_state_payload(orchestrator, snapshot_timeout_ms) do
        %{error: %{code: code}} when code in ["snapshot_timeout", "snapshot_unavailable"] ->
          {:error, :project_unavailable}

        payload ->
          {:ok, maybe_project_scoped_payload(payload, project_state)}
      end
    end
  end

  @spec project_issue_payload(String.t(), String.t(), (-> map()) | nil, timeout()) :: {:ok, map()} | {:error, atom()}
  def project_issue_payload(project_id, issue_identifier, project_states_provider, snapshot_timeout_ms)
      when is_binary(project_id) and is_binary(issue_identifier) do
    with {:ok, project_state} <- project_state(project_id, project_states_provider),
         {:ok, orchestrator} <- project_orchestrator(project_state) do
      single_issue_payload(issue_identifier, orchestrator, snapshot_timeout_ms)
    end
  end

  @spec project_refresh_payload(String.t(), (-> map()) | nil) :: {:ok, map()} | {:error, atom()}
  def project_refresh_payload(project_id, project_states_provider) when is_binary(project_id) do
    with {:ok, project_state} <- project_state(project_id, project_states_provider),
         {:ok, orchestrator} <- project_orchestrator(project_state) do
      refresh_payload(orchestrator)
    end
  end

  defp runner_runtime_totals(snapshot) when is_map(snapshot) do
    Map.get(snapshot, :runner_runtime_totals) || %{seconds_running: Map.get(Map.get(snapshot, :codex_totals, %{}), :seconds_running, 0)}
  end

  defp snapshot_payload(generated_at, snapshot) do
    running = Enum.map(snapshot.running, &running_entry_payload/1)
    retrying = Enum.map(snapshot.retrying, &retry_entry_payload/1)
    blocked = Enum.map(Map.get(snapshot, :blocked, []), &blocked_entry_payload/1)
    dependency_blocked_items = dependency_blocked_items(snapshot)
    cleanup_problem_items = cleanup_problem_items(snapshot)
    attention = attention_payload(snapshot, running, retrying, blocked, dependency_blocked_items, cleanup_problem_items)

    %{
      generated_at: generated_at,
      counts: counts_payload(running, retrying, blocked),
      attention: attention,
      issue_queue: optional_list(snapshot, :issue_queue),
      review_items: optional_list(snapshot, :review_items),
      owner_input_items: optional_list(snapshot, :owner_input_items),
      rca_required_items: optional_list(snapshot, :rca_required_items),
      stale_states: optional_list(snapshot, :stale_states),
      recent_failures: optional_list(snapshot, :recent_failures),
      cleanup_status: optional_value(snapshot, :cleanup_status, %{}),
      recent_activity: optional_list(snapshot, :recent_activity),
      stewardship: optional_value(snapshot, :stewardship, %{}),
      dispatch_summary: optional_value(snapshot, :dispatch_summary, %{}),
      running: running,
      retrying: retrying,
      blocked: blocked,
      codex_totals: snapshot.codex_totals,
      runner_runtime_totals: runner_runtime_totals(snapshot),
      suppression_events: Map.get(snapshot, :suppression_events, []),
      suppression_counts: Map.get(snapshot, :suppression_counts, %{}),
      rate_limits: snapshot.rate_limits,
      polling: Map.get(snapshot, :polling),
      active_milestone: Map.get(snapshot, :active_milestone)
    }
    |> put_nonempty(:dependency_blocked_items, dependency_blocked_items)
    |> put_nonempty(:cleanup_problem_items, cleanup_problem_items)
  end

  defp aggregate_state_payload(project_states, snapshot_timeout_ms) do
    generated_at = DateTime.utc_now() |> DateTime.truncate(:second) |> DateTime.to_iso8601()

    project_payloads =
      project_states
      |> ordered_project_states()
      |> Enum.map(&project_aggregate_payload(&1, snapshot_timeout_ms))

    running = project_payloads |> Enum.flat_map(& &1.running)
    retrying = project_payloads |> Enum.flat_map(& &1.retrying)
    blocked = project_payloads |> Enum.flat_map(& &1.blocked)

    attention = aggregate_attention_payload(project_payloads, running, retrying, blocked)

    %{
      generated_at: generated_at,
      counts: counts_payload(running, retrying, blocked),
      attention: attention,
      counts_by_project: Map.new(project_payloads, &{&1.project.id, &1.project.counts}),
      counts_by_runner_kind: counts_by_runner_kind(running, retrying, blocked),
      projects: Enum.map(project_payloads, & &1.project),
      running: running,
      retrying: retrying,
      blocked: blocked,
      codex_totals: sum_codex_totals(project_payloads),
      runner_runtime_totals: sum_runner_runtime_totals(project_payloads),
      issue_queue: project_payloads |> Enum.flat_map(& &1.issue_queue),
      dependency_blocked_items: project_payloads |> Enum.flat_map(& &1.dependency_blocked_items),
      review_items: project_payloads |> Enum.flat_map(& &1.review_items),
      owner_input_items: project_payloads |> Enum.flat_map(& &1.owner_input_items),
      rca_required_items: project_payloads |> Enum.flat_map(& &1.rca_required_items),
      stale_states: project_payloads |> Enum.flat_map(& &1.stale_states),
      recent_failures: project_payloads |> Enum.flat_map(& &1.recent_failures),
      cleanup_status: Map.new(project_payloads, &{&1.project.id, &1.cleanup_status}),
      cleanup_problem_items: project_payloads |> Enum.flat_map(& &1.cleanup_problem_items),
      recent_activity: project_payloads |> Enum.flat_map(& &1.recent_activity),
      stewardship: aggregate_stewardship_payload(project_payloads),
      dispatch_summary: aggregate_dispatch_summary_payload(project_payloads),
      suppression_events: project_payloads |> Enum.flat_map(& &1.suppression_events),
      suppression_counts: sum_map_counts(project_payloads, :suppression_counts),
      rate_limits: nil,
      polling: nil,
      active_milestone: nil
    }
  end

  defp project_aggregate_payload({project_id, project_state}, snapshot_timeout_ms) do
    project = base_project_payload(project_id, project_state)

    case project_orchestrator(project_state) do
      {:ok, orchestrator} ->
        case Orchestrator.snapshot(orchestrator, snapshot_timeout_ms) do
          %{} = snapshot ->
            running =
              snapshot.running
              |> Enum.map(&enrich_entry_project(&1, project))
              |> Enum.map(&running_entry_payload/1)

            retrying =
              snapshot.retrying
              |> Enum.map(&enrich_entry_project(&1, project))
              |> Enum.map(&retry_entry_payload/1)

            blocked =
              snapshot
              |> Map.get(:blocked, [])
              |> Enum.map(&enrich_entry_project(&1, project))
              |> Enum.map(&blocked_entry_payload/1)

            dependency_blocked_items = scoped_dependency_blocked_items(snapshot, project)
            cleanup_problem_items = scoped_cleanup_problem_items(snapshot, project)

            project =
              project_summary_payload(
                project,
                snapshot,
                running,
                retrying,
                blocked,
                dependency_blocked_items,
                cleanup_problem_items
              )

            %{
              project: project,
              running: running,
              retrying: retrying,
              blocked: blocked,
              codex_totals: Map.get(snapshot, :codex_totals, %{}),
              runner_runtime_totals: runner_runtime_totals(snapshot),
              issue_queue: scoped_optional_list(snapshot, :issue_queue, project),
              dependency_blocked_items: dependency_blocked_items,
              review_items: scoped_optional_list(snapshot, :review_items, project),
              owner_input_items: scoped_optional_list(snapshot, :owner_input_items, project),
              rca_required_items: scoped_optional_list(snapshot, :rca_required_items, project),
              stale_states: scoped_optional_list(snapshot, :stale_states, project),
              recent_failures: scoped_optional_list(snapshot, :recent_failures, project),
              cleanup_status: optional_value(snapshot, :cleanup_status, %{}),
              cleanup_problem_items: cleanup_problem_items,
              recent_activity: scoped_optional_list(snapshot, :recent_activity, project),
              stewardship: scope_project_value(optional_value(snapshot, :stewardship, %{}), project),
              dispatch_summary: scope_project_value(optional_value(snapshot, :dispatch_summary, %{}), project),
              suppression_events: Map.get(snapshot, :suppression_events, []),
              suppression_counts: Map.get(snapshot, :suppression_counts, %{})
            }

          :timeout ->
            empty_project_aggregate(
              Map.merge(project, %{
                worker_health: "unavailable",
                error: %{code: "snapshot_timeout", message: "Snapshot timed out"}
              })
            )

          :unavailable ->
            empty_project_aggregate(
              Map.merge(project, %{
                worker_health: "unavailable",
                error: %{code: "snapshot_unavailable", message: "Snapshot unavailable"}
              })
            )
        end

      {:error, _reason} ->
        empty_project_aggregate(project)
    end
  end

  defp empty_project_aggregate(project) do
    project = Map.merge(project, %{counts: counts_payload([], [], []), worker_health: Map.get(project, :worker_health, "unavailable")})

    %{
      project: project,
      running: [],
      retrying: [],
      blocked: [],
      codex_totals: %{},
      runner_runtime_totals: %{},
      issue_queue: [],
      dependency_blocked_items: [],
      review_items: [],
      owner_input_items: [],
      rca_required_items: [],
      stale_states: [],
      recent_failures: [],
      cleanup_status: %{},
      cleanup_problem_items: [],
      recent_activity: [],
      stewardship: %{},
      dispatch_summary: %{},
      suppression_events: [],
      suppression_counts: %{}
    }
  end

  defp maybe_project_scoped_payload(%{error: _} = payload, _project_state), do: payload

  defp maybe_project_scoped_payload(payload, project_state) do
    project =
      project_state
      |> project_id_and_state()
      |> then(fn {project_id, state} -> base_project_payload(project_id, state) end)
      |> Map.merge(%{counts: payload.counts, worker_health: "available"})

    payload
    |> Map.put(:projects, [Map.merge(project, project_summary_from_payload(payload))])
    |> Map.put_new(:attention, attention_payload(payload, payload.running, payload.retrying, payload.blocked))
  end

  defp project_summary_payload(project, snapshot, running, retrying, blocked, dependency_blocked_items, cleanup_problem_items) do
    counts = counts_payload(running, retrying, blocked)
    issue_queue = optional_list(snapshot, :issue_queue)
    review_items = optional_list(snapshot, :review_items)
    owner_input_items = optional_list(snapshot, :owner_input_items)
    rca_required_items = optional_list(snapshot, :rca_required_items)
    stale_states = optional_list(snapshot, :stale_states)
    recent_failures = optional_list(snapshot, :recent_failures)
    attention = attention_payload(snapshot, running, retrying, blocked, dependency_blocked_items, cleanup_problem_items)

    Map.merge(project, %{
      worker_health: "available",
      counts: counts,
      attention: attention,
      active_milestone: optional_value(snapshot, :active_milestone),
      active_issue: active_issue_payload(running, blocked, issue_queue),
      active_session: active_session_payload(running, blocked),
      queue_depth: length(issue_queue),
      dependency_blocked_count: length(dependency_blocked_items),
      review_count: length(review_items),
      owner_input_count: length(owner_input_items),
      rca_required_count: length(rca_required_items),
      stale_count: length(stale_states),
      recent_failure_count: length(recent_failures),
      cleanup_problem_count: length(cleanup_problem_items),
      last_runner_event: last_runner_event(running, blocked),
      tokens: optional_value(snapshot, :codex_totals, %{}),
      runner_runtime_totals: runner_runtime_totals(snapshot),
      cleanup_status: optional_value(snapshot, :cleanup_status, %{}),
      cleanup_problem_items: cleanup_problem_items,
      recent_activity: optional_list(snapshot, :recent_activity),
      stewardship: optional_value(snapshot, :stewardship, %{}),
      dispatch_summary: optional_value(snapshot, :dispatch_summary, %{})
    })
  end

  defp project_summary_from_payload(payload) do
    %{
      attention: Map.get(payload, :attention),
      active_milestone: Map.get(payload, :active_milestone),
      active_issue: active_issue_payload(payload.running, payload.blocked, Map.get(payload, :issue_queue, [])),
      active_session: active_session_payload(payload.running, payload.blocked),
      queue_depth: length(Map.get(payload, :issue_queue, [])),
      dependency_blocked_count: length(Map.get(payload, :dependency_blocked_items, [])),
      review_count: length(Map.get(payload, :review_items, [])),
      owner_input_count: length(Map.get(payload, :owner_input_items, [])),
      rca_required_count: length(Map.get(payload, :rca_required_items, [])),
      stale_count: length(Map.get(payload, :stale_states, [])),
      recent_failure_count: length(Map.get(payload, :recent_failures, [])),
      cleanup_problem_count: length(Map.get(payload, :cleanup_problem_items, [])),
      last_runner_event: last_runner_event(payload.running, payload.blocked),
      tokens: Map.get(payload, :codex_totals, %{}),
      runner_runtime_totals: Map.get(payload, :runner_runtime_totals, %{}),
      cleanup_status: Map.get(payload, :cleanup_status, %{}),
      cleanup_problem_items: Map.get(payload, :cleanup_problem_items, []),
      recent_activity: Map.get(payload, :recent_activity, []),
      stewardship: Map.get(payload, :stewardship, %{}),
      dispatch_summary: Map.get(payload, :dispatch_summary, %{})
    }
  end

  defp aggregate_stewardship_payload(project_payloads) do
    %{
      active_milestone: nil,
      active_project_milestone_id: nil,
      eligible_issue_count: sum_nested_numbers(project_payloads, :stewardship, :eligible_issue_count),
      dependency_blocked_count: sum_nested_numbers(project_payloads, :stewardship, :dependency_blocked_count),
      running_count: sum_nested_numbers(project_payloads, :stewardship, :running_count),
      retrying_count: sum_nested_numbers(project_payloads, :stewardship, :retrying_count),
      blocked_count: sum_nested_numbers(project_payloads, :stewardship, :blocked_count),
      owner_input_count: sum_nested_numbers(project_payloads, :stewardship, :owner_input_count),
      recent_suppression_reasons:
        project_payloads
        |> Enum.flat_map(&(get_in(&1, [:stewardship, :recent_suppression_reasons]) || []))
        |> Enum.uniq()
        |> Enum.take(10)
    }
  end

  defp aggregate_dispatch_summary_payload(project_payloads) do
    stewardship = aggregate_stewardship_payload(project_payloads)

    {dispatch_state, reason} =
      project_payloads
      |> Enum.map(&get_in(&1, [:dispatch_summary, :dispatch_state]))
      |> aggregate_dispatch_state()

    Map.merge(stewardship, %{dispatch_state: dispatch_state, reason: reason})
  end

  defp aggregate_dispatch_state(states) do
    cond do
      Enum.any?(states, &(&1 in [:dependency_blocked, "dependency_blocked"])) ->
        {:dependency_blocked, "At least one project has queued work blocked by Linear dependencies."}

      Enum.any?(states, &(&1 in [:owner_blocked, "owner_blocked"])) ->
        {:owner_blocked, "At least one project is owner-blocked."}

      Enum.any?(states, &(&1 in [:retry_waiting, "retry_waiting"])) ->
        {:retry_waiting, "At least one project is waiting for retry."}

      Enum.any?(states, &(&1 in [:unchanged, "unchanged"])) ->
        {:unchanged, "At least one project recently suppressed unchanged work."}

      Enum.any?(states, &(&1 in [:done_processed, "done_processed"])) ->
        {:done_processed, "At least one project recently processed completed work."}

      Enum.any?(states, &(&1 in [:eligible_work, "eligible_work"])) ->
        {:eligible_work, "At least one project has eligible work."}

      true ->
        {:no_eligible_work, "No project has eligible work."}
    end
  end

  defp sum_nested_numbers(items, outer_key, inner_key) do
    items
    |> Enum.map(&(get_in(&1, [outer_key, inner_key]) || 0))
    |> sum_numbers()
  end

  defp aggregate_attention_payload(project_payloads, running, retrying, blocked) do
    %{
      active_projects: Enum.count(project_payloads, &(&1.project.enabled == true)),
      runnable_todo: project_payloads |> Enum.flat_map(& &1.issue_queue) |> Enum.count(&runnable_issue?/1),
      running: length(running),
      retrying: length(retrying),
      blocked: length(blocked),
      in_review: project_payloads |> Enum.map(&Map.get(&1.project, :review_count)) |> sum_numbers(),
      owner_input: project_payloads |> Enum.map(&Map.get(&1.project, :owner_input_count)) |> sum_numbers(),
      rca_required: project_payloads |> Enum.map(&Map.get(&1.project, :rca_required_count)) |> sum_numbers(),
      stale: project_payloads |> Enum.map(&Map.get(&1.project, :stale_count)) |> sum_numbers(),
      recent_failures: project_payloads |> Enum.map(&Map.get(&1.project, :recent_failure_count)) |> sum_numbers()
    }
    |> put_positive(
      :dependency_blocked,
      project_payloads |> Enum.map(&Map.get(&1.project, :dependency_blocked_count)) |> sum_numbers()
    )
    |> put_positive(
      :cleanup_problems,
      project_payloads |> Enum.map(&Map.get(&1.project, :cleanup_problem_count)) |> sum_numbers()
    )
  end

  defp attention_payload(snapshot, running, retrying, blocked) do
    attention_payload(
      snapshot,
      running,
      retrying,
      blocked,
      dependency_blocked_items(snapshot),
      cleanup_problem_items(snapshot)
    )
  end

  defp attention_payload(snapshot, running, retrying, blocked, dependency_blocked_items, cleanup_problem_items) do
    %{
      active_projects: 1,
      runnable_todo: optional_list(snapshot, :issue_queue) |> Enum.count(&runnable_issue?/1),
      running: length(running),
      retrying: length(retrying),
      blocked: length(blocked),
      in_review: optional_list(snapshot, :review_items) |> length(),
      owner_input: optional_list(snapshot, :owner_input_items) |> length(),
      rca_required: optional_list(snapshot, :rca_required_items) |> length(),
      stale: optional_list(snapshot, :stale_states) |> length(),
      recent_failures: optional_list(snapshot, :recent_failures) |> length()
    }
    |> put_positive(:dependency_blocked, length(dependency_blocked_items))
    |> put_positive(:cleanup_problems, length(cleanup_problem_items))
  end

  defp scoped_dependency_blocked_items(snapshot, project) do
    snapshot
    |> dependency_blocked_items()
    |> Enum.map(&scope_project_value(&1, project))
  end

  defp scoped_cleanup_problem_items(snapshot, project) do
    snapshot
    |> cleanup_problem_items()
    |> Enum.map(&scope_project_value(&1, project))
  end

  defp dependency_blocked_items(snapshot) do
    explicit_dependency_blocked =
      optional_list(snapshot, :dependency_blocked_items) ++
        optional_list(snapshot, :blocked_queue_items) ++
        optional_list(snapshot, :blocked_issue_queue)

    queued_dependency_blocked =
      snapshot
      |> optional_list(:issue_queue)
      |> Enum.filter(&dependency_blocked_issue?/1)

    (explicit_dependency_blocked ++ queued_dependency_blocked)
    |> Enum.map(&dependency_blocked_item/1)
    |> Enum.uniq_by(&dependency_blocked_item_key/1)
  end

  defp dependency_blocked_issue?(issue) when is_map(issue) do
    issue_state = issue_state(issue)
    blocked? = optional_value(issue, :blocked, false) in [true, "true"]
    blockers = blocker_details(issue)

    issue_state in ["todo", "queued", "preparing", "ready", "runnable"] and (blocked? or blockers != [])
  end

  defp dependency_blocked_issue?(_issue), do: false

  defp dependency_blocked_item(issue) when is_map(issue) do
    issue
    |> Map.put_new(:blocked, true)
    |> Map.put_new(:blockers, blocker_details(issue))
    |> Map.put_new(:block_reason, block_reason(issue))
  end

  defp dependency_blocked_item(issue), do: issue

  defp dependency_blocked_item_key(item) when is_map(item) do
    optional_value(item, :issue_id) || optional_value(item, :id) || optional_value(item, :identifier) || item
  end

  defp dependency_blocked_item_key(item), do: item

  defp blocker_details(issue) do
    optional_list(issue, :blocked_by) ++ optional_list(issue, :blockers) ++ optional_list(issue, :dependencies)
  end

  defp block_reason(issue) do
    optional_value(issue, :block_reason) || optional_value(issue, :blocked_reason) ||
      optional_value(issue, :reason) || "dependency_blocked"
  end

  defp cleanup_problem_items(snapshot) do
    explicit_items = optional_list(snapshot, :cleanup_problem_items) ++ optional_list(snapshot, :cleanup_problems)

    cleanup_status = optional_value(snapshot, :cleanup_status, %{})

    status_items = cleanup_status_problem_items(cleanup_status)

    explicit_items ++ status_items
  end

  defp cleanup_status_problem_items(status) when is_map(status) do
    cond do
      not cleanup_attempt_evidence?(status) ->
        []

      status_problem = cleanup_status_problem(status) ->
        [status_problem]

      true ->
        cleanup_attempt_problem_items(status)
    end
  end

  defp cleanup_status_problem_items(_status), do: []

  defp cleanup_status_problem(status) do
    status_value =
      optional_value(status, :status) || optional_value(status, :result) || optional_value(status, :outcome)

    problem =
      optional_value(status, :error) || optional_value(status, :failure) || optional_value(status, :problem)

    cond do
      problem ->
        %{check: :cleanup, status: status_value || :reported, reason: cleanup_problem_reason(problem)}

      not cleanup_ok?(status_value) ->
        %{check: :cleanup, status: status_value, reason: cleanup_problem_reason(status_value)}

      true ->
        nil
    end
  end

  defp cleanup_attempt_problem_items(status) do
    status
    |> cleanup_attempts()
    |> Enum.reject(&cleanup_ok?/1)
    |> Enum.map(fn attempt ->
      %{check: optional_value(attempt, :kind, :cleanup), status: attempt, reason: cleanup_problem_reason(attempt)}
    end)
  end

  defp cleanup_attempt_evidence?(status) when is_map(status), do: cleanup_attempts(status) != []

  defp cleanup_attempts(status) when is_map(status) do
    attempts = optional_list(status, :attempts)

    case optional_value(status, :last_attempt) do
      attempt when is_map(attempt) -> Enum.uniq([attempt | attempts])
      _ -> attempts
    end
  end

  defp cleanup_ok?(nil), do: true
  defp cleanup_ok?(%DateTime{}), do: true
  defp cleanup_ok?(value) when value in [:ok, :healthy, :idle, "ok", "healthy", "idle", true], do: true
  defp cleanup_ok?(value) when value in [:error, :failed, :stale, :blocked, "error", "failed", "stale", "blocked", false], do: false

  defp cleanup_ok?(value) when is_map(value) do
    state = optional_value(value, :status) || optional_value(value, :state) || optional_value(value, :result)
    state == nil or cleanup_ok?(state)
  end

  defp cleanup_ok?(_value), do: true

  defp cleanup_problem_reason(value) when is_map(value) do
    optional_value(value, :reason) || optional_value(value, :error) || optional_value(value, :message) || inspect(value)
  end

  defp cleanup_problem_reason(value), do: to_string(value)

  defp active_issue_payload([running | _], _blocked, _issue_queue), do: active_issue_from_entry(running, "running")
  defp active_issue_payload([], [blocked | _], _issue_queue), do: active_issue_from_entry(blocked, "blocked")
  defp active_issue_payload([], [], [issue | _]), do: issue
  defp active_issue_payload([], [], _issue_queue), do: nil

  defp active_issue_from_entry(entry, status) do
    %{
      issue_id: Map.get(entry, :issue_id),
      issue_identifier: Map.get(entry, :issue_identifier),
      status: status
    }
  end

  defp active_session_payload([running | _], _blocked), do: session_payload(running)
  defp active_session_payload([], [blocked | _]), do: session_payload(blocked)
  defp active_session_payload([], []), do: nil

  defp session_payload(entry) do
    %{session_id: Map.get(entry, :session_id), runner: Map.get(entry, :runner), workspace_path: Map.get(entry, :workspace_path)}
  end

  defp last_runner_event([entry | _], _blocked), do: runner_event_payload(entry)
  defp last_runner_event([], [entry | _]), do: runner_event_payload(entry)
  defp last_runner_event([], []), do: nil

  defp runner_event_payload(entry) do
    %{
      event: Map.get(entry, :last_runner_event),
      message: Map.get(entry, :last_runner_message),
      at: Map.get(entry, :last_runner_event_at)
    }
  end

  defp scoped_optional_list(snapshot, field, project) do
    snapshot
    |> optional_list(field)
    |> Enum.map(&scope_project_value(&1, project))
  end

  defp scope_project_value(value, project) when is_map(value) do
    value
    |> put_default(:project_id, project.id)
    |> put_default(:project_name, project.name)
  end

  defp scope_project_value(value, _project), do: value

  defp optional_list(source, field) do
    case optional_value(source, field, []) do
      list when is_list(list) -> list
      _ -> []
    end
  end

  defp optional_value(source, field, default \\ nil)

  defp optional_value(source, field, default) when is_map(source) do
    Map.get(source, field) || Map.get(source, Atom.to_string(field)) || default
  end

  defp optional_value(_source, _field, default), do: default

  defp put_nonempty(map, _key, []), do: map
  defp put_nonempty(map, key, value), do: Map.put(map, key, value)

  defp put_positive(map, _key, 0), do: map
  defp put_positive(map, key, value), do: Map.put(map, key, value)

  defp runnable_issue?(issue) when is_map(issue) do
    issue_state = issue_state(issue)
    blocked? = optional_value(issue, :blocked, false) in [true, "true"]
    issue_state in ["todo", "queued", "preparing", "ready", "runnable"] and not blocked?
  end

  defp runnable_issue?(_issue), do: false

  defp issue_state(issue), do: optional_value(issue, :state, optional_value(issue, :status, "")) |> to_string() |> String.downcase()

  defp sum_numbers(values), do: Enum.reduce(values, 0, &(&2 + number_value(&1)))

  defp aggregate_issue_payload(issue_identifier, project_states, snapshot_timeout_ms) do
    {running_entries, retry_entries, blocked_entries} =
      project_states
      |> ordered_project_states()
      |> Enum.reduce({[], [], []}, fn {project_id, project_state}, entries ->
        aggregate_project_issue_entries(
          issue_identifier,
          project_id,
          project_state,
          snapshot_timeout_ms,
          entries
        )
      end)

    running = List.first(running_entries)
    retry = List.first(retry_entries)
    blocked = List.first(blocked_entries)

    if is_nil(running) and is_nil(retry) and is_nil(blocked) do
      {:error, :issue_not_found}
    else
      issue_payload =
        issue_payload_body(issue_identifier, running, retry, blocked, running_entries, retry_entries, blocked_entries)

      {:ok, issue_payload}
    end
  end

  defp aggregate_project_issue_entries(issue_identifier, project_id, project_state, snapshot_timeout_ms, entries) do
    case project_orchestrator(project_state) do
      {:ok, orchestrator} ->
        project = base_project_payload(project_id, project_state)

        orchestrator
        |> Orchestrator.snapshot(snapshot_timeout_ms)
        |> issue_entries_from_snapshot(issue_identifier, project, entries)

      {:error, _reason} ->
        entries
    end
  end

  defp issue_entries_from_snapshot(%{} = snapshot, issue_identifier, project, {running_acc, retry_acc, blocked_acc}) do
    running = snapshot.running |> matching_issue_entries(issue_identifier) |> Enum.map(&enrich_entry_project(&1, project))
    retrying = snapshot.retrying |> matching_issue_entries(issue_identifier) |> Enum.map(&enrich_entry_project(&1, project))

    blocked =
      snapshot
      |> Map.get(:blocked, [])
      |> matching_issue_entries(issue_identifier)
      |> Enum.map(&enrich_entry_project(&1, project))

    {running_acc ++ running, retry_acc ++ retrying, blocked_acc ++ blocked}
  end

  defp issue_entries_from_snapshot(_snapshot, _issue_identifier, _project, entries), do: entries

  defp matching_issue_entries(entries, issue_identifier) do
    Enum.filter(entries, &(&1.identifier == issue_identifier))
  end

  defp counts_payload(running, retrying, blocked) do
    %{running: length(running), retrying: length(retrying), blocked: length(blocked)}
  end

  defp counts_by_runner_kind(running, retrying, blocked) do
    (running ++ retrying ++ blocked)
    |> Enum.map(&get_in(&1, [:runner, :kind]))
    |> Enum.reject(&is_nil/1)
    |> Enum.frequencies()
  end

  defp sum_codex_totals(project_payloads) do
    sum_nested_counts(project_payloads, :codex_totals, [:input_tokens, :output_tokens, :total_tokens, :seconds_running])
  end

  defp sum_runner_runtime_totals(project_payloads) do
    sum_nested_counts(project_payloads, :runner_runtime_totals, [:seconds_running])
  end

  defp sum_nested_counts(project_payloads, field, keys) do
    Map.new(keys, fn key ->
      {key, Enum.reduce(project_payloads, 0, fn payload, acc -> acc + number_value(get_in(payload, [field, key])) end)}
    end)
  end

  defp sum_map_counts(project_payloads, field) do
    Enum.reduce(project_payloads, %{}, fn payload, acc ->
      payload
      |> Map.get(field, %{})
      |> Enum.reduce(acc, fn {key, value}, acc -> Map.update(acc, key, number_value(value), &(&1 + number_value(value))) end)
    end)
  end

  defp number_value(value) when is_number(value), do: value
  defp number_value(_value), do: 0

  defp project_states(provider) when is_function(provider, 0), do: provider.()

  defp project_states(nil) do
    if Process.whereis(RootConfigStore) do
      RootConfigStore.project_states()
    end
  end

  defp project_states(_provider), do: nil

  defp project_state(project_id, project_states_provider) do
    case project_states(project_states_provider) do
      %{} = states ->
        case Map.fetch(states, project_id) do
          {:ok, state} -> {:ok, state}
          :error -> {:error, :project_not_found}
        end

      _ ->
        {:error, :project_not_found}
    end
  end

  defp project_orchestrator(%{status: :running, context: context}) do
    case get_in(context_value(context, :process_names), [:orchestrator]) do
      nil -> {:error, :project_unavailable}
      orchestrator -> resolve_orchestrator_name(orchestrator)
    end
  end

  defp project_orchestrator(_project_state), do: {:error, :project_unavailable}

  defp resolve_orchestrator_name(pid) when is_pid(pid), do: {:ok, pid}

  defp resolve_orchestrator_name({:via, Registry, {ProjectRegistry, key}}) do
    resolve_project_registry_key(key)
  end

  defp resolve_orchestrator_name(name) when is_atom(name), do: {:ok, name}
  defp resolve_orchestrator_name(key), do: resolve_project_registry_key(key)

  defp resolve_project_registry_key(key) do
    case ProjectRegistry.whereis(key) do
      pid when is_pid(pid) -> {:ok, pid}
      nil -> {:error, :project_unavailable}
    end
  end

  defp ordered_project_states(project_states) do
    project_states
    |> Enum.sort_by(fn {project_id, project_state} ->
      context = Map.get(project_state, :context, %{})
      {context_value(context, :dashboard_order) || 0, context_value(context, :name) || project_id, project_id}
    end)
  end

  defp project_id_and_state(%{context: context} = state), do: {context_value(context, :project_id), state}

  defp base_project_payload(project_id, project_state) do
    context = Map.get(project_state, :context, %{})
    execution = context_value(context, :execution) || %{}
    gates = context_value(context, :gates) || %{}

    %{
      id: context_value(context, :project_id) || project_id,
      name: context_value(context, :name) || project_id,
      order: context_value(context, :dashboard_order),
      root: context_value(context, :repo_root),
      app_root: context_value(context, :app_root),
      enabled: context_value(context, :enabled) == true,
      status: project_state |> Map.get(:status, :unknown) |> to_string(),
      execution_enabled: Map.get(execution, "enabled", true) not in [false, "false"],
      gate_enabled: Map.get(gates, "dispatch_enabled", true) not in [false, "false"],
      runner_kind: get_in(context_value(context, :runner) || %{}, ["default"]),
      worker_health: if(Map.get(project_state, :status) == :running, do: "available", else: "unavailable"),
      error: error_payload(Map.get(project_state, :error))
    }
  end

  defp enrich_entry_project(entry, project) do
    entry
    |> put_default(:project_id, project.id)
    |> put_default(:project_name, project.name)
    |> put_default(:project_root, project.root)
    |> put_default(:runner_kind, project.runner_kind)
    |> put_default(:runner_owner, project.runner_kind)
  end

  defp put_default(entry, key, value) do
    if Map.get(entry, key) in [nil, ""] do
      Map.put(entry, key, value)
    else
      entry
    end
  end

  defp context_value(context, key) when is_map(context), do: Map.get(context, key) || Map.get(context, Atom.to_string(key))
  defp context_value(_context, _key), do: nil

  defp error_payload(nil), do: nil
  defp error_payload(error), do: inspect(error)

  @spec issue_payload(String.t(), GenServer.name(), timeout()) :: {:ok, map()} | {:error, :issue_not_found}
  def issue_payload(issue_identifier, orchestrator, snapshot_timeout_ms) when is_binary(issue_identifier) do
    issue_payload(issue_identifier, orchestrator, snapshot_timeout_ms, nil)
  end

  @spec issue_payload(String.t(), GenServer.name(), timeout(), (-> map()) | nil) :: {:ok, map()} | {:error, :issue_not_found}
  def issue_payload(issue_identifier, orchestrator, snapshot_timeout_ms, project_states_provider)
      when is_binary(issue_identifier) do
    case project_states(project_states_provider) do
      nil -> single_issue_payload(issue_identifier, orchestrator, snapshot_timeout_ms)
      project_states -> aggregate_issue_payload(issue_identifier, project_states, snapshot_timeout_ms)
    end
  end

  defp single_issue_payload(issue_identifier, orchestrator, snapshot_timeout_ms) do
    case Orchestrator.snapshot(orchestrator, snapshot_timeout_ms) do
      %{} = snapshot ->
        running_entries = Enum.filter(snapshot.running, &(&1.identifier == issue_identifier))
        retry_entries = Enum.filter(snapshot.retrying, &(&1.identifier == issue_identifier))
        blocked_entries = Enum.filter(Map.get(snapshot, :blocked, []), &(&1.identifier == issue_identifier))

        running = List.first(running_entries)
        retry = List.first(retry_entries)
        blocked = List.first(blocked_entries)

        if is_nil(running) and is_nil(retry) and is_nil(blocked) do
          {:error, :issue_not_found}
        else
          {:ok,
           issue_payload_body(
             issue_identifier,
             running,
             retry,
             blocked,
             running_entries,
             retry_entries,
             blocked_entries
           )}
        end

      _ ->
        {:error, :issue_not_found}
    end
  end

  @spec refresh_payload(GenServer.name()) :: {:ok, map()} | {:error, :unavailable}
  def refresh_payload(orchestrator) do
    single_refresh_payload(orchestrator)
  end

  @spec refresh_payload(GenServer.name(), (-> map()) | nil) :: {:ok, map()} | {:error, :unavailable}
  def refresh_payload(orchestrator, project_states_provider) do
    case project_states(project_states_provider) do
      nil -> single_refresh_payload(orchestrator)
      project_states -> aggregate_refresh_payload(project_states)
    end
  end

  defp single_refresh_payload(orchestrator) do
    case Orchestrator.request_refresh(orchestrator) do
      :unavailable ->
        {:error, :unavailable}

      payload ->
        {:ok, refresh_payload_body(payload)}
    end
  end

  defp aggregate_refresh_payload(project_states) do
    project_refreshes =
      project_states
      |> ordered_project_states()
      |> Enum.reduce([], fn {project_id, project_state}, acc ->
        project = base_project_payload(project_id, project_state)

        with {:ok, orchestrator} <- project_orchestrator(project_state),
             payload when payload != :unavailable <- Orchestrator.request_refresh(orchestrator) do
          [Map.merge(project, %{refresh: refresh_payload_body(payload)}) | acc]
        else
          _ -> acc
        end
      end)
      |> Enum.reverse()

    case project_refreshes do
      [] ->
        {:error, :unavailable}

      refreshes ->
        {:ok,
         %{
           queued: Enum.any?(refreshes, &get_in(&1, [:refresh, :queued])),
           coalesced: Enum.all?(refreshes, &get_in(&1, [:refresh, :coalesced])),
           requested_at: DateTime.utc_now() |> DateTime.truncate(:second) |> DateTime.to_iso8601(),
           operations: refreshes |> Enum.flat_map(&get_in(&1, [:refresh, :operations])) |> Enum.uniq(),
           projects: refreshes
         }}
    end
  end

  defp refresh_payload_body(payload) do
    Map.update!(payload, :requested_at, &DateTime.to_iso8601/1)
  end

  defp issue_payload_body(issue_identifier, running, retry, blocked, running_entries, retry_entries, blocked_entries) do
    selected_entry = running || retry || blocked

    %{
      issue_identifier: issue_identifier,
      issue_id: issue_id_from_entries(running, retry, blocked),
      project: project_payload(selected_entry),
      status: issue_status(running, retry, blocked),
      workspace: workspace_payload(issue_identifier, running, retry, blocked),
      attempts: retry_attempts_payload(retry),
      running: optional_issue_payload(running, &running_issue_payload/1),
      retry: optional_issue_payload(retry, &retry_issue_payload/1),
      blocked: optional_issue_payload(blocked, &blocked_issue_payload/1),
      matches: issue_matches_payload(running_entries, retry_entries, blocked_entries),
      logs: %{codex_session_logs: []},
      recent_events: recent_events_payload(running || blocked),
      last_error: (blocked && blocked.error) || (retry && retry.error),
      tracked: %{}
    }
  end

  defp workspace_payload(issue_identifier, running, retry, blocked) do
    %{
      path: workspace_path(issue_identifier, running, retry, blocked),
      host: workspace_host(running, retry, blocked)
    }
  end

  defp retry_attempts_payload(retry) do
    %{
      restart_count: restart_count(retry),
      current_retry_attempt: retry_attempt(retry)
    }
  end

  defp optional_issue_payload(nil, _payload_fun), do: nil
  defp optional_issue_payload(entry, payload_fun), do: payload_fun.(entry)

  defp issue_id_from_entries(running, retry, blocked),
    do: (running && running.issue_id) || (retry && retry.issue_id) || (blocked && blocked.issue_id)

  defp restart_count(retry), do: max(retry_attempt(retry) - 1, 0)
  defp retry_attempt(nil), do: 0
  defp retry_attempt(retry), do: retry.attempt || 0

  defp issue_status(running, _retry, _blocked) when not is_nil(running), do: "running"
  defp issue_status(nil, retry, _blocked) when not is_nil(retry), do: "retrying"
  defp issue_status(nil, nil, _blocked), do: "blocked"

  defp running_entry_payload(entry) do
    %{
      issue_id: entry.issue_id,
      issue_identifier: entry.identifier,
      project: project_payload(entry),
      state: entry.state,
      worker_host: Map.get(entry, :worker_host),
      workspace_path: Map.get(entry, :workspace_path),
      runner: runner_payload(entry),
      session_id: entry.session_id,
      turn_count: Map.get(entry, :turn_count, 0),
      last_runner_event: entry.last_codex_event,
      last_runner_message: summarize_message(entry.last_codex_message),
      started_at: iso8601(entry.started_at),
      last_runner_event_at: iso8601(entry.last_codex_timestamp),
      tokens: %{
        input_tokens: entry.codex_input_tokens,
        output_tokens: entry.codex_output_tokens,
        total_tokens: entry.codex_total_tokens
      }
    }
  end

  defp retry_entry_payload(entry) do
    %{
      issue_id: entry.issue_id,
      issue_identifier: entry.identifier,
      project: project_payload(entry),
      attempt: entry.attempt,
      due_at: due_at_iso8601(entry.due_in_ms),
      error: entry.error,
      runner: runner_payload(entry),
      session_id: Map.get(entry, :session_id),
      last_runner_event: Map.get(entry, :last_codex_event),
      last_runner_message: summarize_message(Map.get(entry, :last_codex_message)),
      last_runner_event_at: iso8601(Map.get(entry, :last_codex_timestamp)),
      worker_host: Map.get(entry, :worker_host),
      workspace_path: Map.get(entry, :workspace_path)
    }
  end

  defp blocked_entry_payload(entry) do
    %{
      issue_id: entry.issue_id,
      issue_identifier: entry.identifier,
      project: project_payload(entry),
      state: entry.state,
      error: entry.error,
      runner: runner_payload(entry),
      worker_host: Map.get(entry, :worker_host),
      workspace_path: Map.get(entry, :workspace_path),
      session_id: entry.session_id,
      blocked_at: iso8601(entry.blocked_at),
      last_runner_event: entry.last_codex_event,
      last_runner_message: summarize_message(entry.last_codex_message),
      last_runner_event_at: iso8601(entry.last_codex_timestamp)
    }
  end

  defp running_issue_payload(running) do
    %{
      worker_host: Map.get(running, :worker_host),
      workspace_path: Map.get(running, :workspace_path),
      project: project_payload(running),
      runner: runner_payload(running),
      session_id: running.session_id,
      turn_count: Map.get(running, :turn_count, 0),
      state: running.state,
      started_at: iso8601(running.started_at),
      last_runner_event: running.last_codex_event,
      last_runner_message: summarize_message(running.last_codex_message),
      last_runner_event_at: iso8601(running.last_codex_timestamp),
      tokens: %{
        input_tokens: running.codex_input_tokens,
        output_tokens: running.codex_output_tokens,
        total_tokens: running.codex_total_tokens
      }
    }
  end

  defp retry_issue_payload(retry) do
    %{
      attempt: retry.attempt,
      due_at: due_at_iso8601(retry.due_in_ms),
      error: retry.error,
      runner: runner_payload(retry),
      worker_host: Map.get(retry, :worker_host),
      workspace_path: Map.get(retry, :workspace_path),
      project: project_payload(retry)
    }
  end

  defp blocked_issue_payload(blocked) do
    %{
      worker_host: Map.get(blocked, :worker_host),
      workspace_path: Map.get(blocked, :workspace_path),
      project: project_payload(blocked),
      runner: runner_payload(blocked),
      session_id: blocked.session_id,
      state: blocked.state,
      error: blocked.error,
      blocked_at: iso8601(blocked.blocked_at),
      last_runner_event: blocked.last_codex_event,
      last_runner_message: summarize_message(blocked.last_codex_message),
      last_runner_event_at: iso8601(blocked.last_codex_timestamp)
    }
  end

  defp workspace_path(issue_identifier, running, retry, blocked) do
    (running && Map.get(running, :workspace_path)) ||
      (retry && Map.get(retry, :workspace_path)) ||
      (blocked && Map.get(blocked, :workspace_path)) ||
      Path.join(Config.settings!().workspace.root, issue_identifier)
  end

  defp workspace_host(running, retry, blocked) do
    (running && Map.get(running, :worker_host)) ||
      (retry && Map.get(retry, :worker_host)) ||
      (blocked && Map.get(blocked, :worker_host))
  end

  defp runner_payload(entry) when is_map(entry) do
    %{
      kind: Map.get(entry, :runner_kind),
      owner: Map.get(entry, :runner_owner) || Map.get(entry, :runner_kind),
      phase: Map.get(entry, :runner_phase),
      project_root: Map.get(entry, :runner_project_root),
      command: Map.get(entry, :runner_command),
      session_id: Map.get(entry, :session_id),
      attach_url: Map.get(entry, :runner_attach_url),
      result_state: Map.get(entry, :runner_result_state),
      failure: Map.get(entry, :runner_failure)
    }
  end

  defp runner_payload(_entry), do: %{}

  defp issue_matches_payload(running_entries, retry_entries, blocked_entries) do
    Enum.map(running_entries, &issue_match_payload(&1, "running")) ++
      Enum.map(retry_entries, &issue_match_payload(&1, "retrying")) ++
      Enum.map(blocked_entries, &issue_match_payload(&1, "blocked"))
  end

  defp issue_match_payload(entry, status) do
    %{
      issue_id: entry.issue_id,
      issue_identifier: entry.identifier,
      status: status,
      project: project_payload(entry),
      workspace_path: Map.get(entry, :workspace_path),
      runner: runner_payload(entry),
      session_id: Map.get(entry, :session_id)
    }
  end

  defp project_payload(entry) when is_map(entry) do
    %{
      id: Map.get(entry, :project_id),
      name: Map.get(entry, :project_name),
      root: Map.get(entry, :project_root) || Map.get(entry, :runner_project_root)
    }
  end

  defp project_payload(_entry), do: %{id: nil, name: nil, root: nil}

  defp recent_events_payload(nil), do: []

  defp recent_events_payload(entry) do
    [
      %{
        at: iso8601(entry.last_codex_timestamp),
        event: entry.last_codex_event,
        message: summarize_message(entry.last_codex_message)
      }
    ]
    |> Enum.reject(&is_nil(&1.at))
  end

  defp summarize_message(nil), do: nil
  defp summarize_message(message), do: StatusDashboard.humanize_codex_message(message)

  defp due_at_iso8601(due_in_ms) when is_integer(due_in_ms) do
    DateTime.utc_now()
    |> DateTime.add(div(due_in_ms, 1_000), :second)
    |> DateTime.truncate(:second)
    |> DateTime.to_iso8601()
  end

  defp due_at_iso8601(_due_in_ms), do: nil

  defp iso8601(%DateTime{} = datetime) do
    datetime
    |> DateTime.truncate(:second)
    |> DateTime.to_iso8601()
  end

  defp iso8601(_datetime), do: nil
end
