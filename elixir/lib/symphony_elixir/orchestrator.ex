defmodule SymphonyElixir.Orchestrator do
  @moduledoc """
  Polls Linear and dispatches repository copies to Codex-backed workers.
  """

  use GenServer
  require Logger
  import Bitwise, only: [<<<: 2]

  alias SymphonyElixir.{AgentRunner, Config, ProjectContext, ProjectRegistry, StatusDashboard, Tracker, Workspace}
  alias SymphonyElixir.Linear.Issue
  alias SymphonyElixir.Runner.{CodexAdapter, OpenCodeDispatch, Outcome}

  @continuation_retry_delay_ms 1_000
  @failure_retry_base_ms 10_000
  # Slightly above the dashboard render interval so "checking now…" can render.
  @poll_transition_render_delay_ms 20
  @empty_codex_totals %{
    input_tokens: 0,
    output_tokens: 0,
    total_tokens: 0,
    seconds_running: 0
  }
  @empty_runner_runtime_totals %{
    seconds_running: 0
  }

  defmodule State do
    @moduledoc """
    Runtime state for the orchestrator polling loop.
    """

    defstruct [
      :poll_interval_ms,
      :full_poll_interval_ms,
      :last_full_poll_at_ms,
      :fast_poll_states,
      :max_concurrent_agents,
      :next_poll_due_at_ms,
      :poll_check_in_progress,
      :tick_timer_ref,
      :tick_token,
      :project_context,
      :task_supervisor,
      running: %{},
      completed: MapSet.new(),
      claimed: MapSet.new(),
      blocked: %{},
      retry_attempts: %{},
      continuation_pulsed: MapSet.new(),
      owner_input_pulsed: MapSet.new(),
      active_project_milestone_id: nil,
      codex_totals: nil,
      runner_runtime_totals: nil,
      codex_rate_limits: nil,
      dispatch_paused?: false
    ]

    @type t :: %__MODULE__{}
  end

  @spec start_link(keyword()) :: GenServer.on_start()
  def start_link(opts \\ []) do
    name = Keyword.get(opts, :name, __MODULE__)
    GenServer.start_link(__MODULE__, opts, name: name)
  end

  @impl true
  def init(opts) do
    dispatch_paused? = Keyword.get(opts, :dispatch_paused?, false)

    if dispatch_paused? do
      {:ok, paused_state(opts)}
    else
      {:ok, active_state(opts)}
    end
  end

  defp paused_state(opts) do
    project_context = Keyword.get(opts, :project_context)

    %State{
      project_context: project_context,
      task_supervisor: task_supervisor_name(project_context),
      poll_interval_ms: nil,
      full_poll_interval_ms: nil,
      last_full_poll_at_ms: nil,
      fast_poll_states: [],
      max_concurrent_agents: 0,
      next_poll_due_at_ms: nil,
      poll_check_in_progress: false,
      tick_timer_ref: nil,
      tick_token: nil,
      codex_totals: @empty_codex_totals,
      runner_runtime_totals: @empty_runner_runtime_totals,
      codex_rate_limits: nil,
      dispatch_paused?: true
    }
  end

  defp active_state(opts) do
    now_ms = System.monotonic_time(:millisecond)
    project_context = Keyword.get(opts, :project_context)
    config = Config.settings!(project_context)

    state = %State{
      project_context: project_context,
      task_supervisor: task_supervisor_name(project_context),
      poll_interval_ms: config.polling.interval_ms,
      full_poll_interval_ms: config.polling.full_interval_ms,
      last_full_poll_at_ms: nil,
      fast_poll_states: config.polling.fast_states,
      max_concurrent_agents: config.agent.max_concurrent_agents,
      next_poll_due_at_ms: now_ms,
      poll_check_in_progress: false,
      tick_timer_ref: nil,
      tick_token: nil,
      codex_totals: @empty_codex_totals,
      runner_runtime_totals: @empty_runner_runtime_totals,
      codex_rate_limits: nil,
      dispatch_paused?: false
    }

    run_terminal_workspace_cleanup(state)
    schedule_tick(state, 0)
  end

  @impl true
  def handle_info({:tick, tick_token}, %{dispatch_paused?: true, tick_token: tick_token} = state)
      when is_reference(tick_token) do
    {:noreply, state}
  end

  def handle_info({:tick, tick_token}, %{tick_token: tick_token} = state)
      when is_reference(tick_token) do
    state = refresh_runtime_config(state)

    state = %{
      state
      | poll_check_in_progress: true,
        next_poll_due_at_ms: nil,
        tick_timer_ref: nil,
        tick_token: nil
    }

    notify_dashboard()
    :ok = schedule_poll_cycle_start()
    {:noreply, state}
  end

  def handle_info({:tick, _tick_token}, state), do: {:noreply, state}

  def handle_info(:tick, %{dispatch_paused?: true} = state), do: {:noreply, state}

  def handle_info(:tick, state) do
    state = refresh_runtime_config(state)

    state = %{
      state
      | poll_check_in_progress: true,
        next_poll_due_at_ms: nil,
        tick_timer_ref: nil,
        tick_token: nil
    }

    notify_dashboard()
    :ok = schedule_poll_cycle_start()
    {:noreply, state}
  end

  def handle_info(:run_poll_cycle, %{dispatch_paused?: true} = state), do: {:noreply, state}

  def handle_info(:run_poll_cycle, state) do
    state = refresh_runtime_config(state)
    state = maybe_dispatch(state)
    state = schedule_tick(state, state.poll_interval_ms)
    state = %{state | poll_check_in_progress: false}

    notify_dashboard()
    {:noreply, state}
  end

  def handle_info(
        {:DOWN, ref, :process, _pid, reason},
        %{running: running} = state
      ) do
    case find_issue_id_for_ref(running, ref) do
      nil ->
        {:noreply, state}

      issue_id ->
        {running_entry, state} = pop_running_entry(state, issue_id)
        state = record_session_completion_totals(state, running_entry)
        session_id = running_entry_session_id(running_entry)

        state = handle_agent_down(reason, state, issue_id, running_entry, session_id)

        Logger.info("Agent task finished for issue_id=#{issue_id} session_id=#{session_id} reason=#{inspect(reason)}")

        notify_dashboard()
        {:noreply, state}
    end
  end

  def handle_info({:worker_runtime_info, issue_id, runtime_info}, %{running: running} = state)
      when is_binary(issue_id) and is_map(runtime_info) do
    case Map.get(running, issue_id) do
      nil ->
        {:noreply, state}

      running_entry ->
        updated_running_entry =
          running_entry
          |> maybe_put_runtime_value(:worker_host, runtime_info[:worker_host])
          |> maybe_put_runtime_value(:workspace_path, runtime_info[:workspace_path])

        notify_dashboard()
        {:noreply, %{state | running: Map.put(running, issue_id, updated_running_entry)}}
    end
  end

  def handle_info(
        {:codex_worker_update, issue_id, %{event: _, timestamp: _} = update},
        %{running: running} = state
      ) do
    case Map.get(running, issue_id) do
      nil ->
        {:noreply, state}

      running_entry ->
        {updated_running_entry, token_delta} = integrate_codex_update(running_entry, update)

        state =
          state
          |> apply_codex_token_delta(token_delta)
          |> apply_codex_rate_limits(update)

        notify_dashboard()
        {:noreply, %{state | running: Map.put(running, issue_id, updated_running_entry)}}
    end
  end

  def handle_info({:codex_worker_update, _issue_id, _update}, state), do: {:noreply, state}

  def handle_info(
        {:runner_worker_update, issue_id, %{event: _, timestamp: _} = update},
        %{running: running} = state
      ) do
    case Map.get(running, issue_id) do
      nil ->
        {:noreply, state}

      running_entry ->
        updated_running_entry = integrate_runner_update(running_entry, update)

        notify_dashboard()
        {:noreply, %{state | running: Map.put(running, issue_id, updated_running_entry)}}
    end
  end

  def handle_info({:runner_worker_update, _issue_id, _update}, state), do: {:noreply, state}

  def handle_info({:retry_issue, issue_id, retry_token}, state) do
    result =
      case pop_retry_attempt_state(state, issue_id, retry_token) do
        {:ok, attempt, metadata, state} -> handle_retry_issue(state, issue_id, attempt, metadata)
        :missing -> {:noreply, state}
      end

    notify_dashboard()
    result
  end

  def handle_info({:retry_issue, _issue_id}, state), do: {:noreply, state}

  def handle_info(msg, state) do
    Logger.debug("Orchestrator ignored message: #{inspect(msg)}")
    {:noreply, state}
  end

  defp handle_agent_down(:normal, state, issue_id, running_entry, session_id) do
    cond do
      input_required_blocker?(running_entry) ->
        block_input_required_agent_down(state, issue_id, running_entry, session_id, :normal)

      runner_outcome_kind(running_entry) == :rerouted ->
        Logger.info("Agent task rerouted for issue_id=#{issue_id} session_id=#{session_id}; no continuation retry scheduled")

        state
        |> complete_issue(issue_id)
        |> release_issue_claim(issue_id)

      runner_outcome_kind(running_entry) == :policy_blocked ->
        error = runner_policy_block_error(running_entry)
        Logger.warning("Agent task policy-blocked for issue_id=#{issue_id} issue_identifier=#{running_entry.identifier} session_id=#{session_id}: #{error}")
        block_issue_from_entry(state, issue_id, running_entry, error)

      true ->
        Logger.info("Agent task completed for issue_id=#{issue_id} session_id=#{session_id}; scheduling active-state continuation check")

        state
        |> complete_issue(issue_id)
        |> schedule_issue_retry(issue_id, 1, %{
          identifier: running_entry.identifier,
          delay_type: :continuation,
          worker_host: Map.get(running_entry, :worker_host),
          workspace_path: Map.get(running_entry, :workspace_path)
        })
    end
  end

  defp handle_agent_down(reason, state, issue_id, running_entry, session_id) do
    if input_required_blocker?(running_entry) do
      block_input_required_agent_down(state, issue_id, running_entry, session_id, reason)
    else
      retry_agent_down(state, issue_id, running_entry, session_id, reason)
    end
  end

  defp block_input_required_agent_down(state, issue_id, running_entry, session_id, reason) do
    error = blocker_error(running_entry, "agent exited: #{inspect(reason)}")

    Logger.warning("Agent task blocked for issue_id=#{issue_id} issue_identifier=#{running_entry.identifier} session_id=#{session_id}: #{error}")

    block_issue_from_entry(state, issue_id, running_entry, error)
  end

  defp retry_agent_down(state, issue_id, running_entry, session_id, reason) do
    Logger.warning("Agent task exited for issue_id=#{issue_id} session_id=#{session_id} reason=#{inspect(reason)}; scheduling retry")

    next_attempt = next_retry_attempt_from_running(running_entry)

    schedule_issue_retry(state, issue_id, next_attempt, %{
      identifier: running_entry.identifier,
      error: "agent exited: #{inspect(reason)}",
      worker_host: Map.get(running_entry, :worker_host),
      workspace_path: Map.get(running_entry, :workspace_path),
      runner_kind: Map.get(running_entry, :runner_kind),
      runner_owner: Map.get(running_entry, :runner_owner),
      runner_phase: Map.get(running_entry, :runner_phase),
      runner_project_root: Map.get(running_entry, :runner_project_root),
      runner_command: Map.get(running_entry, :runner_command),
      runner_result_state: Map.get(running_entry, :runner_result_state),
      runner_failure: Map.get(running_entry, :runner_failure)
    })
  end

  defp maybe_dispatch(%State{} = state) do
    {full_poll?, state} = maybe_mark_full_poll(state)

    state =
      if full_poll? do
        state
        |> reconcile_running_issues()
        |> reconcile_blocked_issues()
      else
        state
      end

    with :ok <- validate_runtime_config(state),
         {:ok, issues} <- fetch_poll_candidate_issues(state, full_poll?),
         true <- available_slots(state) > 0 do
      issues
      |> choose_issues(state)
      |> maybe_dispatch_idle_pulse(issues, full_poll?)
      |> maybe_dispatch_milestone_planning(issues, full_poll?)
    else
      {:error, :missing_linear_api_token} ->
        Logger.error("Linear API token missing in WORKFLOW.md")
        state

      {:error, :missing_linear_project_slug} ->
        Logger.error("Linear project slug missing in WORKFLOW.md")
        state

      {:error, :missing_tracker_kind} ->
        Logger.error("Tracker kind missing in WORKFLOW.md")

        state

      {:error, {:unsupported_tracker_kind, kind}} ->
        Logger.error("Unsupported tracker kind in WORKFLOW.md: #{inspect(kind)}")

        state

      {:error, {:invalid_workflow_config, message}} ->
        Logger.error("Invalid WORKFLOW.md config: #{message}")
        state

      {:error, {:missing_workflow_file, path, reason}} ->
        Logger.error("Missing WORKFLOW.md at #{path}: #{inspect(reason)}")
        state

      {:error, :workflow_front_matter_not_a_map} ->
        Logger.error("Failed to parse WORKFLOW.md: workflow front matter must decode to a map")
        state

      {:error, {:workflow_parse_error, reason}} ->
        Logger.error("Failed to parse WORKFLOW.md: #{inspect(reason)}")
        state

      {:error, reason} ->
        Logger.error("Failed to fetch from Linear: #{inspect(reason)}")
        state

      false ->
        state
    end
  end

  @doc false
  @spec poll_states_for_test(State.t(), boolean()) :: [String.t()]
  def poll_states_for_test(%State{} = state, full_poll?) when is_boolean(full_poll?) do
    poll_states(state, full_poll?)
  end

  @doc false
  @spec full_poll_due_for_test(State.t(), integer()) :: boolean()
  def full_poll_due_for_test(%State{} = state, now_ms) when is_integer(now_ms) do
    full_poll_due?(state, now_ms)
  end

  defp maybe_mark_full_poll(%State{} = state) do
    now_ms = System.monotonic_time(:millisecond)

    if full_poll_due?(state, now_ms) do
      {true, %{state | last_full_poll_at_ms: now_ms}}
    else
      {false, state}
    end
  end

  defp full_poll_due?(%State{last_full_poll_at_ms: nil}, _now_ms), do: true

  defp full_poll_due?(%State{full_poll_interval_ms: interval_ms, last_full_poll_at_ms: last_ms}, now_ms)
       when is_integer(interval_ms) and interval_ms > 0 and is_integer(last_ms) and is_integer(now_ms) do
    now_ms - last_ms >= interval_ms
  end

  defp full_poll_due?(_state, _now_ms), do: true

  defp fetch_poll_candidate_issues(%State{} = state, full_poll?) when is_boolean(full_poll?) do
    case poll_states(state, full_poll?) do
      [] -> {:ok, []}
      states -> Tracker.fetch_issues_by_states(states, state.project_context)
    end
  end

  defp poll_states(%State{} = state, true), do: settings_for_state(state).tracker.active_states

  defp poll_states(%State{} = state, false) do
    active_states = active_state_set(state)

    state.fast_poll_states
    |> List.wrap()
    |> Enum.filter(&is_binary/1)
    |> Enum.map(&String.trim/1)
    |> Enum.reject(&(&1 == ""))
    |> Enum.filter(fn state_name -> MapSet.member?(active_states, normalize_issue_state(state_name)) end)
    |> Enum.uniq_by(&normalize_issue_state/1)
  end

  defp reconcile_running_issues(%State{} = state) do
    state = reconcile_stalled_running_issues(state)
    running_ids = Map.keys(state.running)

    if running_ids == [] do
      state
    else
      case Tracker.fetch_issue_states_by_ids(running_ids, state.project_context) do
        {:ok, issues} ->
          issues
          |> reconcile_running_issue_states(
            state,
            active_state_set(state),
            terminal_state_set(state)
          )
          |> reconcile_missing_running_issue_ids(running_ids, issues)

        {:error, reason} ->
          Logger.debug("Failed to refresh running issue states: #{inspect(reason)}; keeping active workers")

          state
      end
    end
  end

  defp reconcile_blocked_issues(%State{} = state) do
    blocked_ids = Map.keys(state.blocked)

    if blocked_ids == [] do
      state
    else
      case Tracker.fetch_issue_states_by_ids(blocked_ids, state.project_context) do
        {:ok, issues} ->
          issues
          |> reconcile_blocked_issue_states(
            state,
            active_state_set(state),
            terminal_state_set(state)
          )
          |> reconcile_missing_blocked_issue_ids(blocked_ids, issues)

        {:error, reason} ->
          Logger.debug("Failed to refresh blocked issue states: #{inspect(reason)}; keeping blocked issues")

          state
      end
    end
  end

  @doc false
  @spec reconcile_issue_states_for_test([Issue.t()], term()) :: term()
  def reconcile_issue_states_for_test(issues, %State{} = state) when is_list(issues) do
    reconcile_running_issue_states(issues, state, active_state_set(state), terminal_state_set(state))
  end

  def reconcile_issue_states_for_test(issues, state) when is_list(issues) do
    reconcile_running_issue_states(issues, state, active_state_set(state), terminal_state_set(state))
  end

  @doc false
  @spec should_dispatch_issue_for_test(Issue.t(), term()) :: boolean()
  def should_dispatch_issue_for_test(%Issue{} = issue, %State{} = state) do
    should_dispatch_issue?(issue, state, active_state_set(state), terminal_state_set(state))
  end

  @doc false
  @spec revalidate_issue_for_dispatch_for_test(Issue.t(), ([String.t()] -> term())) ::
          {:ok, Issue.t()} | {:skip, Issue.t() | :missing} | {:error, term()}
  def revalidate_issue_for_dispatch_for_test(%Issue{} = issue, issue_fetcher)
      when is_function(issue_fetcher, 1) do
    state = %State{}
    revalidate_issue_for_dispatch(issue, issue_fetcher, state, terminal_state_set(state))
  end

  @doc false
  @spec latest_owner_input_issue_for_pulse_for_test([Issue.t()], term()) :: Issue.t() | nil
  def latest_owner_input_issue_for_pulse_for_test(issues, %State{} = state)
      when is_list(issues) do
    latest_owner_input_issue_for_pulse(issues, state)
  end

  @doc false
  @spec latest_done_issue_for_continuation_for_test([Issue.t()], term()) :: Issue.t() | nil
  def latest_done_issue_for_continuation_for_test(issues, %State{} = state)
      when is_list(issues) do
    latest_done_issue_for_continuation(issues, state)
  end

  @doc false
  @spec active_issues_blocking_idle_pulse_for_test([Issue.t()]) :: [Issue.t()]
  def active_issues_blocking_idle_pulse_for_test(issues) when is_list(issues) do
    active_issues_blocking_idle_pulse(issues)
  end

  @doc false
  @spec milestone_planning_issues_for_test([map()], [Issue.t()], term()) :: [Issue.t()]
  def milestone_planning_issues_for_test(milestones, issues, %State{} = state)
      when is_list(milestones) and is_list(issues) do
    milestone_planning_issues(milestones, issues, state, active_state_set(state), terminal_state_set(state))
  end

  @doc false
  @spec sort_issues_for_dispatch_for_test([Issue.t()]) :: [Issue.t()]
  def sort_issues_for_dispatch_for_test(issues) when is_list(issues) do
    sort_issues_for_dispatch(issues)
  end

  @doc false
  @spec select_worker_host_for_test(term(), String.t() | nil) ::
          String.t() | nil | :no_worker_capacity
  def select_worker_host_for_test(%State{} = state, preferred_worker_host) do
    select_worker_host(state, preferred_worker_host, "codex")
  end

  @doc false
  @spec select_worker_host_for_test(term(), String.t() | nil, String.t()) ::
          String.t() | nil | :no_worker_capacity
  def select_worker_host_for_test(%State{} = state, preferred_worker_host, runner_kind) do
    select_worker_host(state, preferred_worker_host, runner_kind)
  end

  @doc false
  @spec reconcile_stalled_running_issues_for_test(term()) :: term()
  def reconcile_stalled_running_issues_for_test(%State{} = state) do
    reconcile_stalled_running_issues(state)
  end

  defp reconcile_running_issue_states([], state, _active_states, _terminal_states), do: state

  defp reconcile_running_issue_states([issue | rest], state, active_states, terminal_states) do
    reconcile_running_issue_states(
      rest,
      reconcile_issue_state(issue, state, active_states, terminal_states),
      active_states,
      terminal_states
    )
  end

  defp reconcile_issue_state(%Issue{} = issue, state, active_states, terminal_states) do
    cond do
      pulse_issue_state_allowed?(issue, state) ->
        Logger.debug("Keeping pulse agent running for #{issue_context(issue)} state=#{issue.state}")

        refresh_running_issue_state(state, issue)

      terminal_issue_state?(issue.state, terminal_states) ->
        Logger.info("Issue moved to terminal state: #{issue_context(issue)} state=#{issue.state}; stopping active agent")

        terminate_running_issue(state, issue.id, true)

      !issue_routable_to_worker?(issue) ->
        Logger.info("Issue no longer routed to this worker: #{issue_context(issue)} assignee=#{inspect(issue.assignee_id)}; stopping active agent")

        terminate_running_issue(state, issue.id, false)

      active_issue_state?(issue.state, active_states) ->
        refresh_running_issue_state(state, issue)

      true ->
        Logger.info("Issue moved to non-active state: #{issue_context(issue)} state=#{issue.state}; stopping active agent")

        terminate_running_issue(state, issue.id, false)
    end
  end

  defp reconcile_issue_state(_issue, state, _active_states, _terminal_states), do: state

  defp reconcile_blocked_issue_states([], state, _active_states, _terminal_states), do: state

  defp reconcile_blocked_issue_states([issue | rest], state, active_states, terminal_states) do
    reconcile_blocked_issue_states(
      rest,
      reconcile_blocked_issue_state(issue, state, active_states, terminal_states),
      active_states,
      terminal_states
    )
  end

  defp reconcile_blocked_issue_state(%Issue{} = issue, state, active_states, terminal_states) do
    cond do
      terminal_issue_state?(issue.state, terminal_states) ->
        Logger.info("Blocked issue moved to terminal state: #{issue_context(issue)} state=#{issue.state}; releasing block")

        cleanup_issue_workspace(state, issue.identifier, blocked_issue_worker_host(state, issue.id))
        release_issue_claim(state, issue.id)

      !issue_routable_to_worker?(issue) ->
        Logger.info("Blocked issue no longer routed to this worker: #{issue_context(issue)} assignee=#{inspect(issue.assignee_id)}; releasing block")

        release_issue_claim(state, issue.id)

      active_issue_state?(issue.state, active_states) ->
        refresh_blocked_issue_state(state, issue)

      true ->
        Logger.info("Blocked issue moved to non-active state: #{issue_context(issue)} state=#{issue.state}; releasing block")

        release_issue_claim(state, issue.id)
    end
  end

  defp reconcile_blocked_issue_state(_issue, state, _active_states, _terminal_states), do: state

  defp reconcile_missing_running_issue_ids(%State{} = state, requested_issue_ids, issues)
       when is_list(requested_issue_ids) and is_list(issues) do
    visible_issue_ids =
      issues
      |> Enum.flat_map(fn
        %Issue{id: issue_id} when is_binary(issue_id) -> [issue_id]
        _ -> []
      end)
      |> MapSet.new()

    Enum.reduce(requested_issue_ids, state, fn issue_id, state_acc ->
      if MapSet.member?(visible_issue_ids, issue_id) do
        state_acc
      else
        log_missing_running_issue(state_acc, issue_id)
        terminate_running_issue(state_acc, issue_id, false)
      end
    end)
  end

  defp reconcile_missing_running_issue_ids(state, _requested_issue_ids, _issues), do: state

  defp reconcile_missing_blocked_issue_ids(%State{} = state, requested_issue_ids, issues)
       when is_list(requested_issue_ids) and is_list(issues) do
    visible_issue_ids =
      issues
      |> Enum.flat_map(fn
        %Issue{id: issue_id} when is_binary(issue_id) -> [issue_id]
        _ -> []
      end)
      |> MapSet.new()

    Enum.reduce(requested_issue_ids, state, fn issue_id, state_acc ->
      if MapSet.member?(visible_issue_ids, issue_id) do
        state_acc
      else
        Logger.info("Blocked issue no longer visible during state refresh: issue_id=#{issue_id}; releasing block")

        release_issue_claim(state_acc, issue_id)
      end
    end)
  end

  defp reconcile_missing_blocked_issue_ids(state, _requested_issue_ids, _issues), do: state

  defp log_missing_running_issue(%State{} = state, issue_id) when is_binary(issue_id) do
    case Map.get(state.running, issue_id) do
      %{identifier: identifier} ->
        Logger.info("Issue no longer visible during running-state refresh: issue_id=#{issue_id} issue_identifier=#{identifier}; stopping active agent")

      _ ->
        Logger.info("Issue no longer visible during running-state refresh: issue_id=#{issue_id}; stopping active agent")
    end
  end

  defp log_missing_running_issue(_state, _issue_id), do: :ok

  defp refresh_running_issue_state(%State{} = state, %Issue{} = issue) do
    case Map.get(state.running, issue.id) do
      %{issue: _} = running_entry ->
        %{state | running: Map.put(state.running, issue.id, %{running_entry | issue: issue})}

      _ ->
        state
    end
  end

  defp pulse_issue_state_allowed?(%Issue{id: issue_id, state: state_name}, %State{} = state)
       when is_binary(issue_id) and is_binary(state_name) do
    normalized_state = normalize_issue_state(state_name)

    case Map.get(state.running, issue_id) do
      %{pulse_kind: :done_continuation} -> normalized_state == "done"
      %{pulse_kind: :owner_input} -> normalized_state == "need owner input"
      _ -> false
    end
  end

  defp pulse_issue_state_allowed?(_issue, _state), do: false

  defp refresh_blocked_issue_state(%State{} = state, %Issue{} = issue) do
    case Map.get(state.blocked, issue.id) do
      %{issue: _} = blocked_entry ->
        %{state | blocked: Map.put(state.blocked, issue.id, %{blocked_entry | issue: issue})}

      _ ->
        state
    end
  end

  defp terminate_running_issue(%State{} = state, issue_id, cleanup_workspace) do
    case Map.get(state.running, issue_id) do
      nil ->
        release_issue_claim(state, issue_id)

      %{pid: pid, ref: ref, identifier: identifier} = running_entry ->
        state = record_session_completion_totals(state, running_entry)
        worker_host = Map.get(running_entry, :worker_host)

        if cleanup_workspace do
          cleanup_issue_workspace(state, identifier, worker_host)
        end

        stop_running_task(pid, ref, Map.get(running_entry, :task_supervisor, state.task_supervisor))

        %{
          state
          | running: Map.delete(state.running, issue_id),
            claimed: MapSet.delete(state.claimed, issue_id),
            blocked: Map.delete(state.blocked, issue_id),
            retry_attempts: Map.delete(state.retry_attempts, issue_id)
        }

      _ ->
        release_issue_claim(state, issue_id)
    end
  end

  defp reconcile_stalled_running_issues(%State{} = state) do
    timeout_ms = settings_for_state(state).codex.stall_timeout_ms

    cond do
      timeout_ms <= 0 ->
        state

      map_size(state.running) == 0 ->
        state

      true ->
        now = DateTime.utc_now()

        Enum.reduce(state.running, state, fn {issue_id, running_entry}, state_acc ->
          maybe_restart_stalled_issue(state_acc, issue_id, running_entry, now, timeout_ms)
        end)
    end
  end

  defp maybe_restart_stalled_issue(
         state,
         _issue_id,
         %{runner_kind: runner_kind},
         _now,
         _timeout_ms
       )
       when runner_kind != "codex" do
    state
  end

  defp maybe_restart_stalled_issue(state, issue_id, running_entry, now, timeout_ms) do
    if Map.has_key?(state.blocked, issue_id) do
      state
    else
      restart_stalled_issue(state, issue_id, running_entry, now, timeout_ms)
    end
  end

  defp restart_stalled_issue(state, issue_id, running_entry, now, timeout_ms) do
    elapsed_ms = stall_elapsed_ms(running_entry, now)

    if is_integer(elapsed_ms) and elapsed_ms > timeout_ms do
      identifier = Map.get(running_entry, :identifier, issue_id)
      session_id = running_entry_session_id(running_entry)

      if input_required_blocker?(running_entry) do
        error =
          blocker_error(
            running_entry,
            "stalled for #{elapsed_ms}ms after Codex requested operator input"
          )

        Logger.warning("Issue blocked: issue_id=#{issue_id} issue_identifier=#{identifier} session_id=#{session_id} elapsed_ms=#{elapsed_ms}; #{error}")

        state
        |> record_session_completion_totals(running_entry)
        |> stop_and_block_issue(issue_id, running_entry, error)
      else
        Logger.warning("Issue stalled: issue_id=#{issue_id} issue_identifier=#{identifier} session_id=#{session_id} elapsed_ms=#{elapsed_ms}; restarting with backoff")

        next_attempt = next_retry_attempt_from_running(running_entry)

        state
        |> terminate_running_issue(issue_id, false)
        |> schedule_issue_retry(issue_id, next_attempt, %{
          identifier: identifier,
          error: "stalled for #{elapsed_ms}ms without codex activity"
        })
      end
    else
      state
    end
  end

  defp stall_elapsed_ms(running_entry, now) do
    running_entry
    |> last_activity_timestamp()
    |> case do
      %DateTime{} = timestamp ->
        max(0, DateTime.diff(now, timestamp, :millisecond))

      _ ->
        nil
    end
  end

  defp last_activity_timestamp(running_entry) when is_map(running_entry) do
    Map.get(running_entry, :last_codex_timestamp) || Map.get(running_entry, :started_at)
  end

  defp last_activity_timestamp(_running_entry), do: nil

  defp input_required_blocker?(running_entry) when is_map(running_entry) do
    Map.get(running_entry, :last_codex_event) in [:turn_input_required, :approval_required] or
      not is_nil(input_required_completion_outcome(Map.get(running_entry, :completion))) or
      codex_message_method(Map.get(running_entry, :last_codex_message)) ==
        "mcpServer/elicitation/request"
  end

  defp input_required_blocker?(_running_entry), do: false

  defp input_required_completion_outcome(completion) when is_map(completion) do
    outcome = Map.get(completion, :outcome) || Map.get(completion, "outcome")
    normalize_input_required_outcome(outcome)
  end

  defp input_required_completion_outcome(_completion), do: nil

  defp normalize_input_required_outcome(outcome)
       when outcome in [:input_required, :needs_input, :approval_required],
       do: outcome

  defp normalize_input_required_outcome(outcome) when is_binary(outcome) do
    case outcome do
      "input_required" -> :input_required
      "needs_input" -> :needs_input
      "approval_required" -> :approval_required
      _ -> nil
    end
  end

  defp normalize_input_required_outcome(_outcome), do: nil

  defp blocker_error(running_entry, fallback) when is_map(running_entry) do
    codex_event_blocker_error(Map.get(running_entry, :last_codex_event)) ||
      completion_blocker_error(Map.get(running_entry, :completion)) ||
      codex_message_blocker_error(Map.get(running_entry, :last_codex_message)) ||
      fallback
  end

  defp blocker_error(_running_entry, fallback), do: fallback

  defp codex_event_blocker_error(:turn_input_required), do: "codex turn requires operator input"
  defp codex_event_blocker_error(:approval_required), do: "codex turn requires approval"
  defp codex_event_blocker_error(_event), do: nil

  defp completion_blocker_error(completion) do
    case input_required_completion_outcome(completion) do
      outcome when outcome in [:input_required, :needs_input] ->
        "codex turn requires operator input"

      :approval_required ->
        "codex turn requires approval"

      nil ->
        nil
    end
  end

  defp codex_message_blocker_error(message) do
    if codex_message_method(message) == "mcpServer/elicitation/request" do
      "codex MCP elicitation requires operator input"
    end
  end

  defp codex_message_method(%{message: %{"method" => method}}) when is_binary(method), do: method
  defp codex_message_method(%{message: %{method: method}}) when is_binary(method), do: method
  defp codex_message_method(%{"method" => method}) when is_binary(method), do: method
  defp codex_message_method(%{method: method}) when is_binary(method), do: method
  defp codex_message_method(_message), do: nil

  defp terminate_task(pid, task_supervisor) when is_pid(pid) do
    case Task.Supervisor.terminate_child(task_supervisor || SymphonyElixir.TaskSupervisor, pid) do
      :ok ->
        :ok

      {:error, :not_found} ->
        Process.exit(pid, :shutdown)
    end
  end

  defp terminate_task(_pid, _task_supervisor), do: :ok

  defp stop_running_task(pid, ref, task_supervisor \\ SymphonyElixir.TaskSupervisor) do
    if is_pid(pid) do
      terminate_task(pid, task_supervisor)
    end

    if is_reference(ref) do
      Process.demonitor(ref, [:flush])
    end

    :ok
  end

  defp stop_and_block_issue(%State{} = state, issue_id, running_entry, error) do
    stop_running_task(Map.get(running_entry, :pid), Map.get(running_entry, :ref), Map.get(running_entry, :task_supervisor))
    block_issue_from_entry(state, issue_id, running_entry, error)
  end

  defp block_issue_from_entry(%State{} = state, issue_id, running_entry, error) do
    blocked_entry = %{
      issue_id: issue_id,
      identifier: Map.get(running_entry, :identifier, issue_id),
      issue: Map.get(running_entry, :issue),
      worker_host: Map.get(running_entry, :worker_host),
      workspace_path: Map.get(running_entry, :workspace_path),
      runner_kind: Map.get(running_entry, :runner_kind),
      runner_owner: Map.get(running_entry, :runner_owner),
      runner_phase: Map.get(running_entry, :runner_phase),
      runner_command: Map.get(running_entry, :runner_command),
      runner_project_root: Map.get(running_entry, :runner_project_root),
      runner_attach_url: Map.get(running_entry, :runner_attach_url),
      runner_result_state: Map.get(running_entry, :runner_result_state),
      runner_failure: Map.get(running_entry, :runner_failure),
      session_id: running_entry_session_id(running_entry),
      error: error,
      blocked_at: DateTime.utc_now(),
      last_codex_message: Map.get(running_entry, :last_codex_message),
      last_codex_event: Map.get(running_entry, :last_codex_event),
      last_codex_timestamp: Map.get(running_entry, :last_codex_timestamp)
    }

    %{
      state
      | running: Map.delete(state.running, issue_id),
        retry_attempts: Map.delete(state.retry_attempts, issue_id),
        claimed: MapSet.put(state.claimed, issue_id),
        blocked: Map.put(state.blocked, issue_id, blocked_entry)
    }
  end

  defp choose_issues(issues, state) do
    active_states = active_state_set(state)
    terminal_states = terminal_state_set(state)
    state = maybe_release_active_project_milestone(state, issues, active_states, terminal_states)

    issues
    |> sort_issues_for_dispatch()
    |> Enum.reduce(state, fn issue, state_acc ->
      if should_dispatch_issue?(issue, state_acc, active_states, terminal_states) do
        dispatch_issue(state_acc, issue)
      else
        state_acc
      end
    end)
  end

  defp maybe_dispatch_idle_pulse(%State{} = state, active_issues, full_poll?) when is_list(active_issues) do
    if idle_pulse_dispatch_allowed?(state, active_issues) do
      dispatch_idle_pulse(state, full_poll?)
    else
      state
    end
  end

  defp maybe_dispatch_idle_pulse(state, _active_issues, _full_poll?), do: state

  defp idle_pulse_dispatch_allowed?(%State{} = state, active_issues) do
    active_issues_blocking_idle_pulse(active_issues) == [] and
      map_size(state.running) == 0 and
      map_size(state.retry_attempts) == 0 and
      available_slots(state) > 0 and
      pulse_dispatch_enabled?(state)
  end

  defp dispatch_idle_pulse(%State{} = state, full_poll?) do
    case dispatch_latest_owner_input_pulse(state) do
      {:dispatched, state} -> state
      {:none, state} when full_poll? -> dispatch_latest_done_continuation_pulse(state)
      {:none, state} -> state
      {:error, state} -> state
    end
  end

  defp maybe_dispatch_milestone_planning(%State{} = state, active_issues, full_poll?) when is_list(active_issues) do
    active_states = active_state_set(state)
    terminal_states = terminal_state_set(state)

    cond do
      not full_poll? ->
        state

      milestone_planning_blocking_active_issues(active_issues, active_states, terminal_states) != [] ->
        state

      map_size(state.running) > 0 or map_size(state.blocked) > 0 or map_size(state.retry_attempts) > 0 ->
        state

      available_slots(state) <= 0 ->
        state

      not pulse_dispatch_enabled?(state) ->
        state

      true ->
        dispatch_next_milestone_planning_issue(state, active_issues, active_states, terminal_states)
    end
  end

  defp maybe_dispatch_milestone_planning(state, _active_issues, _full_poll?), do: state

  defp dispatch_next_milestone_planning_issue(%State{} = state, active_issues, active_states, terminal_states) do
    case Tracker.fetch_project_milestones(state.project_context) do
      {:ok, milestones} ->
        milestones
        |> milestone_planning_issues(active_issues, state, active_states, terminal_states)
        |> List.first()
        |> case do
          %Issue{} = issue -> do_dispatch_issue(state, issue, nil, nil, :project_milestone_planning)
          nil -> state
        end

      {:error, reason} ->
        Logger.warning("Skipping project milestone planning dispatch; failed to fetch project milestones: #{inspect(reason)}")
        state
    end
  end

  defp milestone_planning_blocking_active_issues(issues, active_states, terminal_states) when is_list(issues) do
    Enum.filter(issues, fn
      %Issue{} = issue -> candidate_issue?(issue, active_states, terminal_states)
      _issue -> false
    end)
  end

  defp active_issues_blocking_idle_pulse(issues) when is_list(issues) do
    Enum.reject(issues, fn
      %Issue{state: state_name} -> owner_input_issue_state?(state_name)
      _issue -> false
    end)
  end

  defp pulse_dispatch_enabled?(%State{} = state) do
    active_state_set(state)
    |> Enum.any?(fn state_name -> not String.starts_with?(state_name, "__") end)
  end

  defp dispatch_latest_owner_input_pulse(%State{} = state) do
    if MapSet.member?(active_state_set(state), "need owner input") do
      fetch_and_dispatch_owner_input_pulse(state)
    else
      {:none, state}
    end
  end

  defp fetch_and_dispatch_owner_input_pulse(%State{} = state) do
    case Tracker.fetch_issues_by_states(["Need Owner Input"], state.project_context) do
      {:ok, issues} ->
        dispatch_owner_input_pulse_candidate(latest_owner_input_issue_for_pulse(issues, state), state)

      {:error, reason} ->
        Logger.warning("Skipping Codex owner-input pulse; failed to fetch Need Owner Input issues: #{inspect(reason)}")

        {:error, state}
    end
  end

  defp dispatch_owner_input_pulse_candidate(%Issue{} = issue, %State{} = state) do
    fingerprint = owner_input_pulse_fingerprint(issue)

    Logger.info("Dispatching Codex owner-input pulse from updated issue: #{issue_context(issue)}")

    {:dispatched,
     state
     |> mark_owner_input_pulsed(fingerprint)
     |> do_dispatch_issue(issue, nil, nil, :owner_input)}
  end

  defp dispatch_owner_input_pulse_candidate(nil, %State{} = state), do: {:none, state}

  defp dispatch_latest_done_continuation_pulse(%State{} = state) do
    case Tracker.fetch_issues_by_states(["Done"], state.project_context) do
      {:ok, issues} ->
        issues
        |> latest_done_issue_for_continuation(state)
        |> case do
          %Issue{} = issue ->
            Logger.info("Dispatching Codex continuation pulse from latest Done issue: #{issue_context(issue)}")

            state
            |> mark_continuation_pulsed(issue.id)
            |> do_dispatch_issue(issue, nil, nil, :done_continuation)

          nil ->
            state
        end

      {:error, reason} ->
        Logger.warning("Skipping Codex continuation pulse; failed to fetch Done issues: #{inspect(reason)}")

        state
    end
  end

  defp latest_owner_input_issue_for_pulse(issues, %State{} = state) when is_list(issues) do
    issues
    |> Enum.filter(fn
      %Issue{id: id, state: state_name} = issue when is_binary(id) and is_binary(state_name) ->
        normalize_issue_state(state_name) == "need owner input" and
          milestone_dispatch_allowed?(issue) and
          milestone_batch_allowed?(issue, state) and
          not is_nil(owner_input_activity_at(issue)) and
          !MapSet.member?(state.owner_input_pulsed, owner_input_pulse_fingerprint(issue)) and
          !MapSet.member?(state.claimed, id) and
          !Map.has_key?(state.running, id) and
          !Map.has_key?(state.blocked, id)

      _ ->
        false
    end)
    |> Enum.sort_by(&owner_input_activity_sort_key/1, :desc)
    |> List.first()
  end

  defp latest_owner_input_issue_for_pulse(_issues, _state), do: nil

  defp latest_done_issue_for_continuation(issues, %State{} = state) when is_list(issues) do
    issues
    |> Enum.filter(fn
      %Issue{id: id, state: state_name} = issue when is_binary(id) and is_binary(state_name) ->
        normalize_issue_state(state_name) == "done" and
          milestone_dispatch_allowed?(issue) and
          milestone_batch_allowed?(issue, state) and
          !MapSet.member?(state.continuation_pulsed, id) and
          !MapSet.member?(state.claimed, id) and
          !Map.has_key?(state.running, id) and
          !Map.has_key?(state.blocked, id)

      _ ->
        false
    end)
    |> Enum.sort_by(&issue_updated_at_sort_key/1, :desc)
    |> List.first()
  end

  defp latest_done_issue_for_continuation(_issues, _state), do: nil

  defp owner_input_activity_sort_key(%Issue{} = issue) do
    issue
    |> owner_input_activity_at()
    |> datetime_sort_key()
  end

  defp issue_updated_at_sort_key(%Issue{updated_at: %DateTime{} = updated_at}) do
    datetime_sort_key(updated_at)
  end

  defp issue_updated_at_sort_key(%Issue{}), do: 0

  defp datetime_sort_key(%DateTime{} = datetime), do: DateTime.to_unix(datetime, :microsecond)
  defp datetime_sort_key(_datetime), do: 0

  defp mark_continuation_pulsed(%State{} = state, issue_id) when is_binary(issue_id) do
    %{state | continuation_pulsed: MapSet.put(state.continuation_pulsed, issue_id)}
  end

  defp mark_owner_input_pulsed(%State{} = state, fingerprint) when is_binary(fingerprint) do
    %{state | owner_input_pulsed: MapSet.put(state.owner_input_pulsed, fingerprint)}
  end

  defp owner_input_pulse_fingerprint(%Issue{id: id} = issue) when is_binary(id) do
    case owner_input_activity_at(issue) do
      %DateTime{} = activity_at -> id <> ":" <> DateTime.to_iso8601(activity_at)
      _ -> id <> ":unknown"
    end
  end

  defp owner_input_activity_at(%Issue{comments: comments}) when is_list(comments) do
    comments
    |> Enum.sort_by(&comment_activity_sort_key/1)
    |> List.last()
    |> case do
      %{created_at: %DateTime{} = created_at} = comment ->
        if owner_input_answer_comment?(comment), do: created_at

      _ ->
        nil
    end
  end

  defp owner_input_activity_at(%Issue{}), do: nil

  defp owner_input_answer_comment?(%{parent_id: parent_id}) when is_binary(parent_id) and parent_id != "",
    do: true

  defp owner_input_answer_comment?(%{body: body, parent_id: parent_id})
       when is_binary(body) and (is_nil(parent_id) or parent_id == "") do
    owner_top_level_answer?(body)
  end

  defp owner_input_answer_comment?(_comment), do: false

  defp owner_top_level_answer?(body) when is_binary(body) do
    normalized =
      body
      |> String.trim()
      |> String.downcase()

    normalized != "" and
      not machine_generated_owner_input_comment?(normalized) and
      not long_question_comment?(normalized)
  end

  defp machine_generated_owner_input_comment?(body) when is_binary(body) do
    Enum.any?(
      [
        "<!-- symphony:",
        "## opencode handoff",
        "## symphony stop rule",
        "## benchmark",
        "## validation",
        "## changed files",
        "```text\nstatus:",
        "symphony stop rule",
        "opencode handoff",
        "changed files",
        "validation results"
      ],
      &String.contains?(body, &1)
    )
  end

  defp long_question_comment?(body) when is_binary(body) do
    String.length(body) > 80 and String.contains?(body, "?")
  end

  defp comment_activity_sort_key(%{created_at: %DateTime{} = created_at}), do: DateTime.to_unix(created_at, :microsecond)
  defp comment_activity_sort_key(_comment), do: 0

  defp sort_issues_for_dispatch(issues) when is_list(issues) do
    Enum.sort_by(issues, fn
      %Issue{} = issue ->
        {priority_rank(issue.priority), issue_created_at_sort_key(issue), issue.identifier || issue.id || ""}

      _ ->
        {priority_rank(nil), issue_created_at_sort_key(nil), ""}
    end)
  end

  defp priority_rank(priority) when is_integer(priority) and priority in 1..4, do: priority
  defp priority_rank(_priority), do: 5

  defp issue_created_at_sort_key(%Issue{created_at: %DateTime{} = created_at}) do
    DateTime.to_unix(created_at, :microsecond)
  end

  defp issue_created_at_sort_key(%Issue{}), do: 9_223_372_036_854_775_807
  defp issue_created_at_sort_key(_issue), do: 9_223_372_036_854_775_807

  defp milestone_planning_issues(milestones, issues, %State{} = state, active_states, terminal_states)
       when is_list(milestones) and is_list(issues) do
    milestones
    |> Enum.filter(&milestone_planning_allowed?(&1, issues, state, active_states, terminal_states))
    |> Enum.map(&synthetic_milestone_planning_issue/1)
  end

  defp milestone_planning_issues(_milestones, _issues, _state, _active_states, _terminal_states), do: []

  defp milestone_planning_allowed?(milestone, issues, %State{} = state, active_states, terminal_states)
       when is_map(milestone) and is_list(issues) do
    milestone_id = milestone_id(milestone)
    planning_issue_id = synthetic_milestone_planning_issue_id(milestone_id)

    not is_nil(milestone_id) and
      milestone_phase_state?(milestone_description(milestone), "todo") and
      milestone_planning_batch_allowed?(milestone_id, state) and
      milestone_planning_issue_available?(planning_issue_id, state) and
      milestone_without_active_issue?(issues, milestone_id, active_states, terminal_states) and
      !active_project_milestone_has_runtime_state?(state, milestone_id)
  end

  defp milestone_planning_allowed?(_milestone, _issues, _state, _active_states, _terminal_states), do: false

  defp milestone_planning_batch_allowed?(milestone_id, %State{active_project_milestone_id: nil})
       when is_binary(milestone_id),
       do: true

  defp milestone_planning_batch_allowed?(milestone_id, %State{active_project_milestone_id: active_id})
       when is_binary(milestone_id) and is_binary(active_id),
       do: milestone_id == active_id

  defp milestone_planning_batch_allowed?(_milestone_id, _state), do: false

  defp milestone_planning_issue_available?(planning_issue_id, %State{} = state) do
    !MapSet.member?(state.claimed, planning_issue_id) and
      !Map.has_key?(state.running, planning_issue_id) and
      !Map.has_key?(state.blocked, planning_issue_id) and
      !Map.has_key?(state.retry_attempts, planning_issue_id)
  end

  defp milestone_without_active_issue?(issues, milestone_id, active_states, terminal_states) do
    !any_active_issue_in_other_milestone?(issues, milestone_id, active_states, terminal_states) and
      !milestone_has_active_issue?(issues, milestone_id, active_states, terminal_states)
  end

  defp any_active_issue_in_other_milestone?(issues, milestone_id, active_states, terminal_states) do
    Enum.any?(issues, fn
      %Issue{} = issue ->
        issue_project_milestone_id(issue) != nil and
          issue_project_milestone_id(issue) != milestone_id and
          candidate_issue?(issue, active_states, terminal_states) and
          milestone_dispatch_allowed?(issue)

      _ ->
        false
    end)
  end

  defp milestone_has_active_issue?(issues, milestone_id, active_states, terminal_states) do
    Enum.any?(issues, fn
      %Issue{} = issue ->
        issue_project_milestone_id(issue) == milestone_id and
          candidate_issue?(issue, active_states, terminal_states) and
          milestone_dispatch_allowed?(issue)

      _ ->
        false
    end)
  end

  defp synthetic_milestone_planning_issue(milestone) when is_map(milestone) do
    milestone_id = milestone_id(milestone)
    milestone_name = milestone_name(milestone)

    %Issue{
      id: synthetic_milestone_planning_issue_id(milestone_id),
      identifier: synthetic_milestone_planning_identifier(milestone_id),
      title: "Plan milestone: #{milestone_name}",
      description: synthetic_milestone_planning_description(milestone),
      priority: 1,
      state: "Todo",
      project_milestone: synthetic_milestone_context(milestone),
      synthetic_kind: :project_milestone_planning,
      labels: ["milestone-planning"],
      assigned_to_worker: true
    }
  end

  defp synthetic_milestone_planning_issue_id(milestone_id) when is_binary(milestone_id) do
    "project-milestone:#{milestone_id}:planning"
  end

  defp synthetic_milestone_planning_issue_id(_milestone_id), do: nil

  defp synthetic_milestone_planning_identifier(milestone_id) when is_binary(milestone_id) do
    "MILESTONE-#{milestone_id}"
  end

  defp milestone_id(%{} = milestone) do
    case Map.get(milestone, :id) || Map.get(milestone, "id") do
      id when is_binary(id) and id != "" -> id
      _ -> nil
    end
  end

  defp milestone_name(%{} = milestone) do
    case Map.get(milestone, :name) || Map.get(milestone, "name") do
      name when is_binary(name) and name != "" -> name
      _ -> "Project Milestone"
    end
  end

  defp synthetic_milestone_context(%{} = milestone) do
    %{
      id: milestone_id(milestone),
      name: milestone_name(milestone),
      description: milestone_description(milestone),
      status: milestone_value(milestone, :status, "status"),
      target_date: milestone_target_date(milestone)
    }
  end

  defp synthetic_milestone_planning_description(milestone) when is_map(milestone) do
    """
    Synthetic milestone planning task for Machine Architect.

    Project Milestone: #{milestone_name(milestone)}
    Milestone ID: #{milestone_id(milestone)}

    Milestone description:

    #{milestone_description(milestone)}

    Instructions:
    - Treat this as a Machine Architect planning turn, not an OpenCode implementation task.
    - Hydrate the matching Mnemesh context if refs are present in the milestone description.
    - Create or reuse executable Linear issues inside this Project Milestone.
    - Keep each executable issue scoped for the configured coding runner and preserve role boundaries.
    - Do not create new product milestones or choose the next global product direction.
    """
  end

  defp should_dispatch_issue?(
         %Issue{} = issue,
         %State{running: running, claimed: claimed, blocked: blocked} = state,
         active_states,
         terminal_states
       ) do
    candidate_issue?(issue, active_states, terminal_states) and
      milestone_dispatch_allowed?(issue) and
      milestone_batch_allowed?(issue, state) and
      !owner_input_issue_state?(issue.state) and
      !todo_issue_blocked_by_non_terminal?(issue, terminal_states) and
      issue_not_already_tracked?(issue.id, claimed, running, blocked) and
      available_slots(state) > 0 and
      state_slots_available?(issue, state) and
      worker_slots_available?(state, nil, runner_kind_for_issue(issue, state))
  end

  defp should_dispatch_issue?(_issue, _state, _active_states, _terminal_states), do: false

  defp issue_not_already_tracked?(issue_id, claimed, running, blocked) do
    !MapSet.member?(claimed, issue_id) and
      !Map.has_key?(running, issue_id) and
      !Map.has_key?(blocked, issue_id)
  end

  defp state_slots_available?(%Issue{state: issue_state}, %State{running: running} = state) when is_map(running) do
    limit = Config.max_concurrent_agents_for_state(issue_state, state.project_context)
    used = running_issue_count_for_state(running, issue_state)
    limit > used
  end

  defp state_slots_available?(%Issue{} = issue, running) when is_map(running) do
    state_slots_available?(issue, %State{running: running})
  end

  defp state_slots_available?(_issue, _running), do: false

  defp running_issue_count_for_state(running, issue_state) when is_map(running) do
    normalized_state = normalize_issue_state(issue_state)

    Enum.count(running, fn
      {_id, %{issue: %Issue{state: state_name}}} ->
        normalize_issue_state(state_name) == normalized_state

      _ ->
        false
    end)
  end

  defp candidate_issue?(
         %Issue{
           id: id,
           identifier: identifier,
           title: title,
           state: state_name
         } = issue,
         active_states,
         terminal_states
       )
       when is_binary(id) and is_binary(identifier) and is_binary(title) and is_binary(state_name) do
    issue_routable_to_worker?(issue) and
      active_issue_state?(state_name, active_states) and
      !terminal_issue_state?(state_name, terminal_states)
  end

  defp candidate_issue?(_issue, _active_states, _terminal_states), do: false

  defp milestone_dispatch_allowed?(%Issue{project_milestone: nil}), do: false

  defp milestone_dispatch_allowed?(%Issue{project_milestone: milestone}) when is_map(milestone) do
    description = milestone_description(milestone)

    cond do
      milestone_phase_state?(description, "paused") ->
        false

      milestone_phase_state?(description, "needs-decision") ->
        false

      milestone_phase_state?(description, "todo") ->
        true

      true ->
        false
    end
  end

  defp milestone_dispatch_allowed?(_issue), do: true

  defp milestone_batch_allowed?(%Issue{} = issue, %State{active_project_milestone_id: nil}) do
    not is_nil(issue_project_milestone_id(issue))
  end

  defp milestone_batch_allowed?(%Issue{} = issue, %State{active_project_milestone_id: milestone_id})
       when is_binary(milestone_id) do
    issue_project_milestone_id(issue) == milestone_id
  end

  defp milestone_batch_allowed?(_issue, _state), do: false

  defp maybe_release_active_project_milestone(
         %State{active_project_milestone_id: nil} = state,
         _issues,
         _active_states,
         _terminal_states
       ),
       do: state

  defp maybe_release_active_project_milestone(
         %State{active_project_milestone_id: milestone_id} = state,
         issues,
         active_states,
         terminal_states
       )
       when is_binary(milestone_id) and is_list(issues) do
    active_milestone_has_work? =
      Enum.any?(issues, fn
        %Issue{} = issue ->
          issue_project_milestone_id(issue) == milestone_id and
            candidate_issue?(issue, active_states, terminal_states) and
            milestone_dispatch_allowed?(issue)

        _ ->
          false
      end)

    if active_milestone_has_work? or active_project_milestone_has_runtime_state?(state, milestone_id) do
      state
    else
      %{state | active_project_milestone_id: nil}
    end
  end

  defp maybe_release_active_project_milestone(state, _issues, _active_states, _terminal_states), do: state

  defp active_project_milestone_has_runtime_state?(%State{} = state, milestone_id)
       when is_binary(milestone_id) do
    running_has_project_milestone?(state.running, milestone_id) or
      retry_has_project_milestone?(state.retry_attempts, milestone_id) or
      blocked_has_project_milestone?(state.blocked, milestone_id)
  end

  defp running_has_project_milestone?(running, milestone_id) when is_map(running) do
    Enum.any?(running, fn
      {_issue_id, %{issue: %Issue{} = issue}} -> issue_project_milestone_id(issue) == milestone_id
      _entry -> false
    end)
  end

  defp retry_has_project_milestone?(retry_attempts, milestone_id) when is_map(retry_attempts) do
    Enum.any?(retry_attempts, fn
      {_issue_id, %{issue: %Issue{} = issue}} -> issue_project_milestone_id(issue) == milestone_id
      _entry -> false
    end)
  end

  defp blocked_has_project_milestone?(blocked, milestone_id) when is_map(blocked) do
    Enum.any?(blocked, fn
      {_issue_id, %{issue: %Issue{} = issue}} -> issue_project_milestone_id(issue) == milestone_id
      _entry -> false
    end)
  end

  defp issue_project_milestone_id(%Issue{project_milestone: milestone}) when is_map(milestone) do
    case Map.get(milestone, :id) || Map.get(milestone, "id") do
      id when is_binary(id) and id != "" -> id
      _ -> nil
    end
  end

  defp issue_project_milestone_id(_issue), do: nil

  defp milestone_description(%{} = milestone) do
    Map.get(milestone, :description) || Map.get(milestone, "description") || ""
  end

  defp milestone_target_date(%{} = milestone) do
    milestone_value(milestone, :target_date, "target_date") ||
      milestone_value(milestone, :targetDate, "targetDate")
  end

  defp milestone_value(%{} = milestone, atom_key, string_key) do
    case Map.get(milestone, atom_key) || Map.get(milestone, string_key) do
      value when value in ["", nil] -> nil
      value -> value
    end
  end

  defp milestone_phase_state?(description, state) when is_binary(description) and is_binary(state) do
    milestone_phase_state(description) == normalize_issue_state(state)
  end

  defp milestone_phase_state?(_description, _state), do: false

  defp milestone_phase_state(description) when is_binary(description) do
    description
    |> String.split(~r/\R/, parts: 2, trim: false)
    |> List.first()
    |> case do
      first_line when is_binary(first_line) ->
        case Regex.run(~r/^phase_state: ([a-z0-9_-]+)$/, first_line) do
          [_match, state] -> normalize_issue_state(state)
          _ -> nil
        end

      _ ->
        nil
    end
  end

  defp owner_input_issue_state?(state_name) when is_binary(state_name) do
    normalize_issue_state(state_name) == "need owner input"
  end

  defp owner_input_issue_state?(_state_name), do: false

  defp issue_routable_to_worker?(%Issue{assigned_to_worker: assigned_to_worker})
       when is_boolean(assigned_to_worker),
       do: assigned_to_worker

  defp issue_routable_to_worker?(_issue), do: true

  defp todo_issue_blocked_by_non_terminal?(
         %Issue{state: issue_state, blocked_by: blockers},
         terminal_states
       )
       when is_binary(issue_state) and is_list(blockers) do
    normalize_issue_state(issue_state) == "todo" and
      Enum.any?(blockers, fn
        %{state: blocker_state} when is_binary(blocker_state) ->
          !terminal_issue_state?(blocker_state, terminal_states)

        _ ->
          true
      end)
  end

  defp todo_issue_blocked_by_non_terminal?(_issue, _terminal_states), do: false

  defp terminal_issue_state?(state_name, terminal_states) when is_binary(state_name) do
    MapSet.member?(terminal_states, normalize_issue_state(state_name))
  end

  defp terminal_issue_state?(_state_name, _terminal_states), do: false

  defp active_issue_state?(state_name, active_states) when is_binary(state_name) do
    MapSet.member?(active_states, normalize_issue_state(state_name))
  end

  defp runner_kind_for_issue(%Issue{state: state_name}, %State{} = state) when is_binary(state_name) do
    settings = settings_for_state(state)

    Map.get(
      settings.runner.routes,
      normalize_issue_state(state_name),
      settings.runner.default
    )
  end

  defp runner_kind_for_issue(_issue, %State{} = state), do: settings_for_state(state).runner.default

  defp runner_kind_for_issue(issue), do: runner_kind_for_issue(issue, %State{})

  defp normalize_issue_state(state_name) when is_binary(state_name) do
    String.downcase(String.trim(state_name))
  end

  defp terminal_state_set(%State{} = state) do
    settings_for_state(state).tracker.terminal_states
    |> Enum.map(&normalize_issue_state/1)
    |> Enum.filter(&(&1 != ""))
    |> MapSet.new()
  end

  defp terminal_state_set, do: terminal_state_set(%State{})

  defp active_state_set(%State{} = state) do
    settings_for_state(state).tracker.active_states
    |> Enum.map(&normalize_issue_state/1)
    |> Enum.filter(&(&1 != ""))
    |> MapSet.new()
  end

  defp active_state_set, do: active_state_set(%State{})

  defp dispatch_issue(%State{} = state, issue, attempt \\ nil, preferred_worker_host \\ nil) do
    case revalidate_issue_for_dispatch(
           issue,
           fn ids -> Tracker.fetch_issue_states_by_ids(ids, state.project_context) end,
           state,
           terminal_state_set(state)
         ) do
      {:ok, %Issue{} = refreshed_issue} ->
        do_dispatch_issue(state, refreshed_issue, attempt, preferred_worker_host)

      {:skip, :missing} ->
        Logger.info("Skipping dispatch; issue no longer active or visible: #{issue_context(issue)}")

        state

      {:skip, %Issue{} = refreshed_issue} ->
        Logger.info("Skipping stale dispatch after issue refresh: #{issue_context(refreshed_issue)} state=#{inspect(refreshed_issue.state)} blocked_by=#{length(refreshed_issue.blocked_by)}")

        state

      {:error, reason} ->
        Logger.warning("Skipping dispatch; issue refresh failed for #{issue_context(issue)}: #{inspect(reason)}")

        state
    end
  end

  defp do_dispatch_issue(
         %State{} = state,
         issue,
         attempt,
         preferred_worker_host,
         pulse_kind \\ nil
       ) do
    recipient = self()

    runner_kind = runner_kind_for_issue(issue, state)

    case select_worker_host(state, preferred_worker_host, runner_kind) do
      :no_worker_capacity ->
        Logger.debug("No SSH worker slots available for #{issue_context(issue)} preferred_worker_host=#{inspect(preferred_worker_host)}")

        state

      worker_host ->
        spawn_issue_on_worker_host(state, issue, attempt, recipient, worker_host, pulse_kind)
    end
  end

  defp spawn_issue_on_worker_host(
         %State{} = state,
         issue,
         attempt,
         recipient,
         worker_host,
         pulse_kind
       ) do
    runner_kind = runner_kind_for_issue(issue, state)

    task_supervisor = state.task_supervisor || SymphonyElixir.TaskSupervisor

    case Task.Supervisor.start_child(task_supervisor, fn ->
           AgentRunner.run(issue, recipient,
             attempt: attempt,
             worker_host: worker_host,
             settings: settings_for_state(state),
             project_context: state.project_context
           )
         end) do
      {:ok, pid} ->
        ref = Process.monitor(pid)

        Logger.info("Dispatching issue to agent: #{issue_context(issue)} pid=#{inspect(pid)} attempt=#{inspect(attempt)} worker_host=#{worker_host || "local"}")

        running =
          Map.put(state.running, issue.id, %{
            pid: pid,
            ref: ref,
            task_supervisor: task_supervisor,
            identifier: issue.identifier,
            issue: issue,
            runner_kind: runner_kind,
            runner_owner: runner_kind,
            runner_phase: :starting,
            runner_command: nil,
            runner_project_root: nil,
            runner_attach_url: nil,
            runner_result_state: nil,
            runner_failure: nil,
            runner_outcome: nil,
            worker_host: worker_host,
            workspace_path: nil,
            session_id: nil,
            last_codex_message: nil,
            last_codex_timestamp: nil,
            last_codex_event: nil,
            codex_app_server_pid: nil,
            codex_input_tokens: 0,
            codex_output_tokens: 0,
            codex_total_tokens: 0,
            codex_last_reported_input_tokens: 0,
            codex_last_reported_output_tokens: 0,
            codex_last_reported_total_tokens: 0,
            turn_count: 0,
            pulse_kind: pulse_kind,
            retry_attempt: normalize_retry_attempt(attempt),
            started_at: DateTime.utc_now()
          })

        %{
          state
          | running: running,
            claimed: MapSet.put(state.claimed, issue.id),
            retry_attempts: Map.delete(state.retry_attempts, issue.id),
            active_project_milestone_id: state.active_project_milestone_id || issue_project_milestone_id(issue)
        }

      {:error, reason} ->
        Logger.error("Unable to spawn agent for #{issue_context(issue)}: #{inspect(reason)}")
        next_attempt = if is_integer(attempt), do: attempt + 1, else: nil

        schedule_issue_retry(state, issue.id, next_attempt, %{
          identifier: issue.identifier,
          issue: issue,
          error: "failed to spawn agent: #{inspect(reason)}",
          worker_host: worker_host
        })
    end
  end

  defp revalidate_issue_for_dispatch(%Issue{id: issue_id}, issue_fetcher, %State{} = state, terminal_states)
       when is_binary(issue_id) and is_function(issue_fetcher, 1) do
    case issue_fetcher.([issue_id]) do
      {:ok, [%Issue{} = refreshed_issue | _]} ->
        if retry_candidate_issue?(refreshed_issue, state, terminal_states) do
          {:ok, refreshed_issue}
        else
          {:skip, refreshed_issue}
        end

      {:ok, []} ->
        {:skip, :missing}

      {:error, reason} ->
        {:error, reason}
    end
  end

  defp revalidate_issue_for_dispatch(issue, _issue_fetcher, _state, _terminal_states), do: {:ok, issue}

  defp complete_issue(%State{} = state, issue_id) do
    %{
      state
      | completed: MapSet.put(state.completed, issue_id),
        retry_attempts: Map.delete(state.retry_attempts, issue_id)
    }
  end

  defp schedule_issue_retry(%State{} = state, issue_id, attempt, metadata)
       when is_binary(issue_id) and is_map(metadata) do
    metadata = Map.put_new(metadata, :project_context, state.project_context)
    previous_retry = Map.get(state.retry_attempts, issue_id, %{attempt: 0})
    next_attempt = if is_integer(attempt), do: attempt, else: previous_retry.attempt + 1
    delay_ms = retry_delay(next_attempt, metadata)
    old_timer = Map.get(previous_retry, :timer_ref)
    retry_token = make_ref()
    due_at_ms = System.monotonic_time(:millisecond) + delay_ms
    retry_metadata = retry_attempt_metadata(issue_id, previous_retry, metadata)

    if is_reference(old_timer) do
      Process.cancel_timer(old_timer)
    end

    timer_ref = Process.send_after(self(), {:retry_issue, issue_id, retry_token}, delay_ms)

    error_suffix = if is_binary(retry_metadata.error), do: " error=#{retry_metadata.error}", else: ""

    Logger.warning("Retrying issue_id=#{issue_id} issue_identifier=#{retry_metadata.identifier} in #{delay_ms}ms (attempt #{next_attempt})#{error_suffix}")

    %{
      state
      | retry_attempts:
          Map.put(
            state.retry_attempts,
            issue_id,
            Map.merge(retry_metadata, %{
              attempt: next_attempt,
              timer_ref: timer_ref,
              retry_token: retry_token,
              due_at_ms: due_at_ms
            })
          )
    }
  end

  defp retry_attempt_metadata(issue_id, previous_retry, metadata) do
    %{
      identifier: pick_retry_identifier(issue_id, previous_retry, metadata),
      issue: metadata[:issue] || Map.get(previous_retry, :issue),
      error: pick_retry_error(previous_retry, metadata),
      worker_host: pick_retry_worker_host(previous_retry, metadata),
      workspace_path: pick_retry_workspace_path(previous_retry, metadata),
      runner_kind: metadata[:runner_kind] || Map.get(previous_retry, :runner_kind),
      runner_owner: metadata[:runner_owner] || Map.get(previous_retry, :runner_owner),
      runner_phase: metadata[:runner_phase] || Map.get(previous_retry, :runner_phase),
      runner_project_root: metadata[:runner_project_root] || Map.get(previous_retry, :runner_project_root),
      runner_command: metadata[:runner_command] || Map.get(previous_retry, :runner_command),
      runner_result_state: metadata[:runner_result_state] || Map.get(previous_retry, :runner_result_state),
      runner_failure: metadata[:runner_failure] || Map.get(previous_retry, :runner_failure),
      project_context: metadata[:project_context] || Map.get(previous_retry, :project_context)
    }
  end

  defp pop_retry_attempt_state(%State{} = state, issue_id, retry_token)
       when is_reference(retry_token) do
    case Map.get(state.retry_attempts, issue_id) do
      %{attempt: attempt, retry_token: ^retry_token} = retry_entry ->
        metadata = %{
          identifier: Map.get(retry_entry, :identifier),
          issue: Map.get(retry_entry, :issue),
          error: Map.get(retry_entry, :error),
          worker_host: Map.get(retry_entry, :worker_host),
          workspace_path: Map.get(retry_entry, :workspace_path),
          runner_kind: Map.get(retry_entry, :runner_kind),
          runner_owner: Map.get(retry_entry, :runner_owner),
          runner_phase: Map.get(retry_entry, :runner_phase),
          runner_project_root: Map.get(retry_entry, :runner_project_root),
          runner_command: Map.get(retry_entry, :runner_command),
          runner_result_state: Map.get(retry_entry, :runner_result_state),
          runner_failure: Map.get(retry_entry, :runner_failure)
        }

        {:ok, attempt, metadata, %{state | retry_attempts: Map.delete(state.retry_attempts, issue_id)}}

      _ ->
        :missing
    end
  end

  defp handle_retry_issue(
         %State{} = state,
         issue_id,
         attempt,
         %{issue: %Issue{synthetic_kind: :project_milestone_planning} = issue} = metadata
       ) do
    if retry_candidate_issue?(issue, state, terminal_state_set(state)) and
         milestone_dispatch_allowed?(issue) and
         milestone_batch_allowed?(issue, state) and
         dispatch_slots_available?(issue, state) and
         worker_slots_available?(state, metadata[:worker_host], runner_kind_for_issue(issue, state)) do
      {:noreply, do_dispatch_issue(state, issue, attempt, metadata[:worker_host], :project_milestone_planning)}
    else
      {:noreply,
       schedule_issue_retry(
         state,
         issue_id,
         attempt + 1,
         Map.merge(metadata, %{error: "no available orchestrator slots"})
       )}
    end
  end

  defp handle_retry_issue(%State{} = state, issue_id, attempt, metadata) do
    case Tracker.fetch_candidate_issues(state.project_context) do
      {:ok, issues} ->
        issues
        |> find_issue_by_id(issue_id)
        |> handle_retry_issue_lookup(state, issue_id, attempt, metadata)

      {:error, reason} ->
        Logger.warning("Retry poll failed for issue_id=#{issue_id} issue_identifier=#{metadata[:identifier] || issue_id}: #{inspect(reason)}")

        {:noreply,
         schedule_issue_retry(
           state,
           issue_id,
           attempt + 1,
           Map.merge(metadata, %{error: "retry poll failed: #{inspect(reason)}"})
         )}
    end
  end

  defp handle_retry_issue_lookup(%Issue{} = issue, state, issue_id, attempt, metadata) do
    terminal_states = terminal_state_set(state)

    cond do
      terminal_issue_state?(issue.state, terminal_states) ->
        Logger.info("Issue state is terminal: issue_id=#{issue_id} issue_identifier=#{issue.identifier} state=#{issue.state}; removing associated workspace")

        cleanup_issue_workspace(state, issue.identifier, metadata[:worker_host])
        {:noreply, release_issue_claim(state, issue_id)}

      retry_candidate_issue?(issue, state, terminal_states) ->
        handle_active_retry(state, issue, attempt, metadata)

      true ->
        Logger.debug("Issue left active states, removing claim issue_id=#{issue_id} issue_identifier=#{issue.identifier}")

        {:noreply, release_issue_claim(state, issue_id)}
    end
  end

  defp handle_retry_issue_lookup(nil, state, issue_id, _attempt, _metadata) do
    Logger.debug("Issue no longer visible, removing claim issue_id=#{issue_id}")
    {:noreply, release_issue_claim(state, issue_id)}
  end

  defp cleanup_issue_workspace(state_or_identifier, identifier_or_worker_host \\ nil, worker_host \\ nil)

  defp cleanup_issue_workspace(%State{} = state, identifier, worker_host) when is_binary(identifier) do
    Workspace.remove_issue_workspaces(identifier, worker_host, settings_for_state(state))
  end

  defp cleanup_issue_workspace(identifier, worker_host, nil) when is_binary(identifier) do
    Workspace.remove_issue_workspaces(identifier, worker_host)
  end

  defp cleanup_issue_workspace(_state_or_identifier, _identifier_or_worker_host, _worker_host), do: :ok

  defp blocked_issue_worker_host(%State{} = state, issue_id) do
    state.blocked
    |> Map.get(issue_id, %{})
    |> Map.get(:worker_host)
  end

  defp run_terminal_workspace_cleanup(%State{} = state) do
    case Tracker.fetch_issues_by_states(settings_for_state(state).tracker.terminal_states, state.project_context) do
      {:ok, issues} ->
        issues
        |> Enum.each(fn
          %Issue{identifier: identifier} when is_binary(identifier) ->
            cleanup_issue_workspace(state, identifier, nil)

          _ ->
            :ok
        end)

      {:error, reason} ->
        Logger.warning("Skipping startup terminal workspace cleanup; failed to fetch terminal issues: #{inspect(reason)}")
    end
  end

  defp notify_dashboard do
    StatusDashboard.notify_update()
  end

  defp handle_active_retry(state, issue, attempt, metadata) do
    if retry_candidate_issue?(issue, state, terminal_state_set(state)) and
         milestone_dispatch_allowed?(issue) and
         milestone_batch_allowed?(issue, state) and
         dispatch_slots_available?(issue, state) and
         worker_slots_available?(state, metadata[:worker_host], runner_kind_for_issue(issue, state)) do
      {:noreply, dispatch_issue(state, issue, attempt, metadata[:worker_host])}
    else
      Logger.debug("No available slots for retrying #{issue_context(issue)}; retrying again")

      {:noreply,
       schedule_issue_retry(
         state,
         issue.id,
         attempt + 1,
         Map.merge(metadata, %{
           identifier: issue.identifier,
           error: "no available orchestrator slots"
         })
       )}
    end
  end

  defp release_issue_claim(%State{} = state, issue_id) do
    %{
      state
      | claimed: MapSet.delete(state.claimed, issue_id),
        blocked: Map.delete(state.blocked, issue_id),
        retry_attempts: Map.delete(state.retry_attempts, issue_id)
    }
  end

  defp retry_delay(attempt, metadata)
       when is_integer(attempt) and attempt > 0 and is_map(metadata) do
    if metadata[:delay_type] == :continuation and attempt == 1 do
      @continuation_retry_delay_ms
    else
      failure_retry_delay(attempt, metadata)
    end
  end

  defp failure_retry_delay(attempt, metadata) do
    max_delay_power = min(attempt - 1, 10)

    min(
      @failure_retry_base_ms * (1 <<< max_delay_power),
      Config.settings!(metadata[:project_context]).agent.max_retry_backoff_ms
    )
  end

  defp normalize_retry_attempt(attempt) when is_integer(attempt) and attempt > 0, do: attempt
  defp normalize_retry_attempt(_attempt), do: 0

  defp next_retry_attempt_from_running(running_entry) do
    case Map.get(running_entry, :retry_attempt) do
      attempt when is_integer(attempt) and attempt > 0 -> attempt + 1
      _ -> nil
    end
  end

  defp pick_retry_identifier(issue_id, previous_retry, metadata) do
    metadata[:identifier] || Map.get(previous_retry, :identifier) || issue_id
  end

  defp pick_retry_error(previous_retry, metadata) do
    metadata[:error] || Map.get(previous_retry, :error)
  end

  defp pick_retry_worker_host(previous_retry, metadata) do
    metadata[:worker_host] || Map.get(previous_retry, :worker_host)
  end

  defp pick_retry_workspace_path(previous_retry, metadata) do
    metadata[:workspace_path] || Map.get(previous_retry, :workspace_path)
  end

  defp maybe_put_runtime_value(running_entry, _key, nil), do: running_entry

  defp maybe_put_runtime_value(running_entry, key, value) when is_map(running_entry) do
    Map.put(running_entry, key, value)
  end

  defp select_worker_host(%State{} = state, preferred_worker_host, runner_kind) do
    if runner_supports_remote_worker_hosts?(runner_kind) do
      select_remote_worker_host(state, preferred_worker_host)
    else
      nil
    end
  end

  defp select_remote_worker_host(%State{} = state, preferred_worker_host) do
    case settings_for_state(state).worker.ssh_hosts do
      [] ->
        nil

      hosts ->
        available_hosts = Enum.filter(hosts, &worker_host_slots_available?(state, &1))

        cond do
          available_hosts == [] ->
            :no_worker_capacity

          preferred_worker_host_available?(preferred_worker_host, available_hosts) ->
            preferred_worker_host

          true ->
            least_loaded_worker_host(state, available_hosts)
        end
    end
  end

  defp preferred_worker_host_available?(preferred_worker_host, hosts)
       when is_binary(preferred_worker_host) and is_list(hosts) do
    preferred_worker_host != "" and preferred_worker_host in hosts
  end

  defp preferred_worker_host_available?(_preferred_worker_host, _hosts), do: false

  defp least_loaded_worker_host(%State{} = state, hosts) when is_list(hosts) do
    hosts
    |> Enum.with_index()
    |> Enum.min_by(fn {host, index} ->
      {running_worker_host_count(state.running, host), index}
    end)
    |> elem(0)
  end

  defp running_worker_host_count(running, worker_host)
       when is_map(running) and is_binary(worker_host) do
    Enum.count(running, fn
      {_issue_id, %{worker_host: ^worker_host}} -> true
      _ -> false
    end)
  end

  defp worker_slots_available?(%State{} = state, preferred_worker_host, runner_kind) do
    select_worker_host(state, preferred_worker_host, runner_kind) != :no_worker_capacity
  end

  defp runner_supports_remote_worker_hosts?("opencode"), do: OpenCodeDispatch.capabilities().remote_worker_hosts
  defp runner_supports_remote_worker_hosts?("codex"), do: CodexAdapter.capabilities().remote_worker_hosts
  defp runner_supports_remote_worker_hosts?(_runner_kind), do: false

  defp worker_host_slots_available?(%State{} = state, worker_host) when is_binary(worker_host) do
    case settings_for_state(state).worker.max_concurrent_agents_per_host do
      limit when is_integer(limit) and limit > 0 ->
        running_worker_host_count(state.running, worker_host) < limit

      _ ->
        true
    end
  end

  defp find_issue_by_id(issues, issue_id) when is_binary(issue_id) do
    Enum.find(issues, fn
      %Issue{id: ^issue_id} ->
        true

      _ ->
        false
    end)
  end

  defp find_issue_id_for_ref(running, ref) do
    running
    |> Enum.find_value(fn {issue_id, %{ref: running_ref}} ->
      if running_ref == ref, do: issue_id
    end)
  end

  defp running_entry_session_id(%{session_id: session_id}) when is_binary(session_id),
    do: session_id

  defp running_entry_session_id(_running_entry), do: "n/a"

  defp issue_context(%Issue{id: issue_id, identifier: identifier}) do
    "issue_id=#{issue_id} issue_identifier=#{identifier}"
  end

  defp available_slots(%State{} = state) do
    max(
      (state.max_concurrent_agents || settings_for_state(state).agent.max_concurrent_agents) -
        map_size(state.running),
      0
    )
  end

  @spec request_refresh() :: map() | :unavailable
  def request_refresh do
    request_refresh(__MODULE__)
  end

  @spec request_refresh(GenServer.server()) :: map() | :unavailable
  def request_refresh(server) do
    if server_available?(server) do
      GenServer.call(server, :request_refresh)
    else
      :unavailable
    end
  end

  @spec snapshot() :: map() | :timeout | :unavailable
  def snapshot, do: snapshot(__MODULE__, 15_000)

  @spec snapshot(GenServer.server(), timeout()) :: map() | :timeout | :unavailable
  def snapshot(server, timeout) do
    if server_available?(server) do
      try do
        GenServer.call(server, :snapshot, timeout)
      catch
        :exit, {:timeout, _} -> :timeout
        :exit, _ -> :unavailable
      end
    else
      :unavailable
    end
  end

  defp server_available?(pid) when is_pid(pid), do: Process.alive?(pid)
  defp server_available?(server), do: Process.whereis(server) != nil

  @impl true
  def handle_call(:snapshot, _from, state) do
    state = refresh_runtime_config(state)
    now = DateTime.utc_now()
    now_ms = System.monotonic_time(:millisecond)

    running =
      state.running
      |> Enum.map(fn {issue_id, metadata} ->
        %{
          issue_id: issue_id,
          identifier: metadata.identifier,
          state: metadata.issue.state,
          runner_kind: Map.get(metadata, :runner_kind),
          runner_owner: Map.get(metadata, :runner_owner),
          runner_phase: Map.get(metadata, :runner_phase),
          runner_command: Map.get(metadata, :runner_command),
          runner_project_root: Map.get(metadata, :runner_project_root),
          runner_attach_url: Map.get(metadata, :runner_attach_url),
          runner_result_state: Map.get(metadata, :runner_result_state),
          runner_failure: Map.get(metadata, :runner_failure),
          worker_host: Map.get(metadata, :worker_host),
          workspace_path: Map.get(metadata, :workspace_path),
          session_id: metadata.session_id,
          codex_app_server_pid: Map.get(metadata, :codex_app_server_pid),
          codex_input_tokens: Map.get(metadata, :codex_input_tokens, 0),
          codex_output_tokens: Map.get(metadata, :codex_output_tokens, 0),
          codex_total_tokens: Map.get(metadata, :codex_total_tokens, 0),
          turn_count: Map.get(metadata, :turn_count, 0),
          started_at: Map.get(metadata, :started_at),
          last_codex_timestamp: Map.get(metadata, :last_codex_timestamp),
          last_codex_message: Map.get(metadata, :last_codex_message),
          last_codex_event: Map.get(metadata, :last_codex_event),
          runtime_seconds: running_seconds(Map.get(metadata, :started_at), now)
        }
      end)

    retrying =
      state.retry_attempts
      |> Enum.map(fn {issue_id, %{attempt: attempt, due_at_ms: due_at_ms} = retry} ->
        %{
          issue_id: issue_id,
          attempt: attempt,
          due_in_ms: max(0, due_at_ms - now_ms),
          identifier: Map.get(retry, :identifier),
          error: Map.get(retry, :error),
          runner_kind: Map.get(retry, :runner_kind),
          runner_owner: Map.get(retry, :runner_owner),
          runner_phase: Map.get(retry, :runner_phase),
          runner_command: Map.get(retry, :runner_command),
          runner_project_root: Map.get(retry, :runner_project_root),
          runner_result_state: Map.get(retry, :runner_result_state),
          runner_failure: Map.get(retry, :runner_failure),
          worker_host: Map.get(retry, :worker_host),
          workspace_path: Map.get(retry, :workspace_path)
        }
      end)

    blocked =
      state.blocked
      |> Enum.map(fn {issue_id, metadata} ->
        %{
          issue_id: issue_id,
          identifier: Map.get(metadata, :identifier),
          state: blocked_issue_state(metadata),
          runner_kind: Map.get(metadata, :runner_kind),
          runner_owner: Map.get(metadata, :runner_owner),
          runner_phase: Map.get(metadata, :runner_phase),
          runner_command: Map.get(metadata, :runner_command),
          runner_project_root: Map.get(metadata, :runner_project_root),
          runner_attach_url: Map.get(metadata, :runner_attach_url),
          runner_result_state: Map.get(metadata, :runner_result_state),
          runner_failure: Map.get(metadata, :runner_failure),
          worker_host: Map.get(metadata, :worker_host),
          workspace_path: Map.get(metadata, :workspace_path),
          session_id: Map.get(metadata, :session_id),
          error: Map.get(metadata, :error),
          blocked_at: Map.get(metadata, :blocked_at),
          last_codex_timestamp: Map.get(metadata, :last_codex_timestamp),
          last_codex_message: Map.get(metadata, :last_codex_message),
          last_codex_event: Map.get(metadata, :last_codex_event)
        }
      end)

    {:reply,
     %{
       running: running,
       retrying: retrying,
       blocked: blocked,
       codex_totals: state.codex_totals,
       runner_runtime_totals: state.runner_runtime_totals || @empty_runner_runtime_totals,
       rate_limits: Map.get(state, :codex_rate_limits),
       polling: %{
         checking?: state.poll_check_in_progress == true,
         next_poll_in_ms: next_poll_in_ms(state.next_poll_due_at_ms, now_ms),
         poll_interval_ms: state.poll_interval_ms
       }
     }, state}
  end

  def handle_call(:request_refresh, _from, state) do
    now_ms = System.monotonic_time(:millisecond)
    already_due? = is_integer(state.next_poll_due_at_ms) and state.next_poll_due_at_ms <= now_ms
    coalesced = state.poll_check_in_progress == true or already_due?
    state = %{state | last_full_poll_at_ms: nil}
    state = if coalesced, do: state, else: schedule_tick(state, 0)

    {:reply,
     %{
       queued: true,
       coalesced: coalesced,
       requested_at: DateTime.utc_now(),
       operations: ["poll", "reconcile"]
     }, state}
  end

  defp blocked_issue_state(%{issue: %Issue{state: state}}), do: state
  defp blocked_issue_state(_metadata), do: nil

  defp integrate_runner_update(running_entry, %{event: event, timestamp: timestamp} = update) do
    running_entry
    |> Map.put(:runner_owner, Map.get(update, :runner_owner, Map.get(update, :runner_kind, Map.get(running_entry, :runner_owner))))
    |> maybe_put_runtime_value(:runner_kind, Map.get(update, :runner_kind))
    |> maybe_put_runtime_value(:runner_phase, Map.get(update, :phase))
    |> maybe_put_runtime_value(:runner_command, Map.get(update, :command))
    |> maybe_put_runtime_value(:runner_project_root, Map.get(update, :project_root))
    |> maybe_put_runtime_value(:runner_attach_url, Map.get(update, :attach_url))
    |> maybe_put_runtime_value(:runner_result_state, Map.get(update, :result_state))
    |> maybe_put_runtime_value(:runner_failure, Map.get(update, :failure))
    |> maybe_put_runtime_value(:runner_outcome, Map.get(update, :outcome))
    |> maybe_put_runtime_value(:session_id, Map.get(update, :session_id))
    |> Map.put(:last_codex_timestamp, timestamp)
    |> Map.put(:last_codex_event, event)
    |> Map.put(:last_codex_message, summarize_runner_update(update))
  end

  defp summarize_runner_update(update) when is_map(update) do
    %{
      event: Map.get(update, :event),
      runner_kind: Map.get(update, :runner_kind),
      phase: Map.get(update, :phase),
      command: Map.get(update, :command),
      project_root: Map.get(update, :project_root),
      session_id: Map.get(update, :session_id),
      result_state: Map.get(update, :result_state),
      failure: Map.get(update, :failure)
    }
  end

  defp runner_outcome_kind(%{runner_outcome: %Outcome{kind: kind}}), do: kind
  defp runner_outcome_kind(%{runner_outcome: %{kind: kind}}), do: kind
  defp runner_outcome_kind(%{runner_outcome: kind}) when is_atom(kind), do: kind
  defp runner_outcome_kind(_running_entry), do: nil

  defp runner_policy_block_error(%{runner_failure: %{reason: reason, detail: detail}})
       when not is_nil(detail) do
    "runner policy blocked: #{inspect(reason)} detail=#{inspect(detail)}"
  end

  defp runner_policy_block_error(%{runner_failure: %{reason: reason}}) do
    "runner policy blocked: #{inspect(reason)}"
  end

  defp runner_policy_block_error(_running_entry), do: "runner policy blocked"

  defp integrate_codex_update(running_entry, %{event: event, timestamp: timestamp} = update) do
    token_delta = extract_token_delta(running_entry, update)
    codex_input_tokens = Map.get(running_entry, :codex_input_tokens, 0)
    codex_output_tokens = Map.get(running_entry, :codex_output_tokens, 0)
    codex_total_tokens = Map.get(running_entry, :codex_total_tokens, 0)
    codex_app_server_pid = Map.get(running_entry, :codex_app_server_pid)
    last_reported_input = Map.get(running_entry, :codex_last_reported_input_tokens, 0)
    last_reported_output = Map.get(running_entry, :codex_last_reported_output_tokens, 0)
    last_reported_total = Map.get(running_entry, :codex_last_reported_total_tokens, 0)
    turn_count = Map.get(running_entry, :turn_count, 0)

    {
      Map.merge(running_entry, %{
        runner_kind: Map.get(update, :runner_kind, Map.get(running_entry, :runner_kind, "codex")),
        runner_owner: Map.get(update, :runner_owner, Map.get(running_entry, :runner_owner, "codex")),
        runner_phase: Map.get(update, :phase, codex_phase_for_event(event, Map.get(running_entry, :runner_phase))),
        runner_command: Map.get(update, :command, Map.get(running_entry, :runner_command)),
        runner_project_root: Map.get(update, :project_root, Map.get(running_entry, :runner_project_root)),
        runner_result_state: Map.get(update, :result_state, Map.get(running_entry, :runner_result_state)),
        runner_failure: Map.get(update, :failure, Map.get(running_entry, :runner_failure)),
        last_codex_timestamp: timestamp,
        last_codex_message: summarize_codex_update(update),
        session_id: session_id_for_update(running_entry.session_id, update),
        last_codex_event: event,
        codex_app_server_pid: codex_app_server_pid_for_update(codex_app_server_pid, update),
        codex_input_tokens: codex_input_tokens + token_delta.input_tokens,
        codex_output_tokens: codex_output_tokens + token_delta.output_tokens,
        codex_total_tokens: codex_total_tokens + token_delta.total_tokens,
        codex_last_reported_input_tokens: max(last_reported_input, token_delta.input_reported),
        codex_last_reported_output_tokens: max(last_reported_output, token_delta.output_reported),
        codex_last_reported_total_tokens: max(last_reported_total, token_delta.total_reported),
        turn_count: turn_count_for_update(turn_count, running_entry.session_id, update)
      }),
      token_delta
    }
  end

  defp codex_phase_for_event(:session_started, _current_phase), do: :session
  defp codex_phase_for_event(:turn_completed, _current_phase), do: :completed
  defp codex_phase_for_event(:failed, _current_phase), do: :failed
  defp codex_phase_for_event(_event, nil), do: :running
  defp codex_phase_for_event(_event, current_phase), do: current_phase

  defp codex_app_server_pid_for_update(_existing, %{codex_app_server_pid: pid})
       when is_binary(pid),
       do: pid

  defp codex_app_server_pid_for_update(_existing, %{codex_app_server_pid: pid})
       when is_integer(pid),
       do: Integer.to_string(pid)

  defp codex_app_server_pid_for_update(_existing, %{codex_app_server_pid: pid}) when is_list(pid),
    do: to_string(pid)

  defp codex_app_server_pid_for_update(existing, _update), do: existing

  defp session_id_for_update(_existing, %{session_id: session_id}) when is_binary(session_id),
    do: session_id

  defp session_id_for_update(existing, _update), do: existing

  defp turn_count_for_update(existing_count, existing_session_id, %{
         event: :session_started,
         session_id: session_id
       })
       when is_integer(existing_count) and is_binary(session_id) do
    if session_id == existing_session_id do
      existing_count
    else
      existing_count + 1
    end
  end

  defp turn_count_for_update(existing_count, _existing_session_id, _update)
       when is_integer(existing_count),
       do: existing_count

  defp turn_count_for_update(_existing_count, _existing_session_id, _update), do: 0

  defp summarize_codex_update(update) do
    %{
      event: update[:event],
      message: update[:payload] || update[:raw],
      timestamp: update[:timestamp]
    }
  end

  defp schedule_tick(%State{} = state, delay_ms) when is_integer(delay_ms) and delay_ms >= 0 do
    if is_reference(state.tick_timer_ref) do
      Process.cancel_timer(state.tick_timer_ref)
    end

    tick_token = make_ref()
    timer_ref = Process.send_after(self(), {:tick, tick_token}, delay_ms)

    %{
      state
      | tick_timer_ref: timer_ref,
        tick_token: tick_token,
        next_poll_due_at_ms: System.monotonic_time(:millisecond) + delay_ms
    }
  end

  defp schedule_poll_cycle_start do
    :timer.send_after(@poll_transition_render_delay_ms, self(), :run_poll_cycle)
    :ok
  end

  defp next_poll_in_ms(nil, _now_ms), do: nil

  defp next_poll_in_ms(next_poll_due_at_ms, now_ms) when is_integer(next_poll_due_at_ms) do
    max(0, next_poll_due_at_ms - now_ms)
  end

  defp pop_running_entry(state, issue_id) do
    {Map.get(state.running, issue_id), %{state | running: Map.delete(state.running, issue_id)}}
  end

  defp record_session_completion_totals(state, running_entry) when is_map(running_entry) do
    runtime_seconds = running_seconds(running_entry.started_at, DateTime.utc_now())

    runner_runtime_totals = apply_runtime_delta(state.runner_runtime_totals, runtime_seconds)

    state = %{state | runner_runtime_totals: runner_runtime_totals}

    if Map.get(running_entry, :runner_kind) != "codex" do
      state
    else
      codex_totals =
        apply_token_delta(
          state.codex_totals,
          %{
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            seconds_running: runtime_seconds
          }
        )

      %{state | codex_totals: codex_totals}
    end
  end

  defp record_session_completion_totals(state, _running_entry), do: state

  defp apply_runtime_delta(runtime_totals, seconds_running) do
    seconds = Map.get(runtime_totals || @empty_runner_runtime_totals, :seconds_running, 0) + seconds_running

    %{seconds_running: max(0, seconds)}
  end

  defp refresh_runtime_config(%State{} = state) do
    config = settings_for_state(state)

    %{
      state
      | poll_interval_ms: config.polling.interval_ms,
        full_poll_interval_ms: config.polling.full_interval_ms,
        fast_poll_states: config.polling.fast_states,
        max_concurrent_agents: config.agent.max_concurrent_agents
    }
  end

  defp settings_for_state(%State{project_context: project_context}), do: Config.settings!(project_context)

  defp validate_runtime_config(%State{project_context: nil}), do: Config.validate!()

  defp validate_runtime_config(%State{} = state) do
    with {:ok, settings} <- Config.settings(state.project_context) do
      validate_settings_semantics(settings)
    end
  end

  defp validate_settings_semantics(settings) do
    cond do
      is_nil(settings.tracker.kind) ->
        {:error, :missing_tracker_kind}

      settings.tracker.kind not in ["linear", "memory"] ->
        {:error, {:unsupported_tracker_kind, settings.tracker.kind}}

      settings.tracker.kind == "linear" and not is_binary(settings.tracker.api_key) ->
        {:error, :missing_linear_api_token}

      settings.tracker.kind == "linear" and not is_binary(settings.tracker.project_slug) ->
        {:error, :missing_linear_project_slug}

      true ->
        :ok
    end
  end

  defp task_supervisor_name(%ProjectContext{} = context) do
    ProjectRegistry.via_name(context.process_names.task_supervisor)
  end

  defp task_supervisor_name(_context), do: SymphonyElixir.TaskSupervisor

  defp retry_candidate_issue?(%Issue{} = issue, %State{} = state, terminal_states) do
    candidate_issue?(issue, active_state_set(state), terminal_states) and
      !todo_issue_blocked_by_non_terminal?(issue, terminal_states)
  end

  defp dispatch_slots_available?(%Issue{} = issue, %State{} = state) do
    available_slots(state) > 0 and state_slots_available?(issue, state)
  end

  defp apply_codex_token_delta(
         %{codex_totals: codex_totals} = state,
         %{input_tokens: input, output_tokens: output, total_tokens: total} = token_delta
       )
       when is_integer(input) and is_integer(output) and is_integer(total) do
    %{state | codex_totals: apply_token_delta(codex_totals, token_delta)}
  end

  defp apply_codex_token_delta(state, _token_delta), do: state

  defp apply_codex_rate_limits(%State{} = state, update) when is_map(update) do
    case extract_rate_limits(update) do
      %{} = rate_limits ->
        %{state | codex_rate_limits: rate_limits}

      _ ->
        state
    end
  end

  defp apply_codex_rate_limits(state, _update), do: state

  defp apply_token_delta(codex_totals, token_delta) do
    input_tokens = Map.get(codex_totals, :input_tokens, 0) + token_delta.input_tokens
    output_tokens = Map.get(codex_totals, :output_tokens, 0) + token_delta.output_tokens
    total_tokens = Map.get(codex_totals, :total_tokens, 0) + token_delta.total_tokens

    seconds_running =
      Map.get(codex_totals, :seconds_running, 0) + Map.get(token_delta, :seconds_running, 0)

    %{
      input_tokens: max(0, input_tokens),
      output_tokens: max(0, output_tokens),
      total_tokens: max(0, total_tokens),
      seconds_running: max(0, seconds_running)
    }
  end

  defp extract_token_delta(running_entry, %{event: _, timestamp: _} = update) do
    running_entry = running_entry || %{}
    usage = extract_token_usage(update)

    {
      compute_token_delta(
        running_entry,
        :input,
        usage,
        :codex_last_reported_input_tokens
      ),
      compute_token_delta(
        running_entry,
        :output,
        usage,
        :codex_last_reported_output_tokens
      ),
      compute_token_delta(
        running_entry,
        :total,
        usage,
        :codex_last_reported_total_tokens
      )
    }
    |> Tuple.to_list()
    |> then(fn [input, output, total] ->
      %{
        input_tokens: input.delta,
        output_tokens: output.delta,
        total_tokens: total.delta,
        input_reported: input.reported,
        output_reported: output.reported,
        total_reported: total.reported
      }
    end)
  end

  defp compute_token_delta(running_entry, token_key, usage, reported_key) do
    next_total = get_token_usage(usage, token_key)
    prev_reported = Map.get(running_entry, reported_key, 0)

    delta =
      if is_integer(next_total) and next_total >= prev_reported do
        next_total - prev_reported
      else
        0
      end

    %{
      delta: max(delta, 0),
      reported: if(is_integer(next_total), do: next_total, else: prev_reported)
    }
  end

  defp extract_token_usage(update) do
    payloads = [
      update[:usage],
      Map.get(update, "usage"),
      Map.get(update, :usage),
      update[:payload],
      Map.get(update, "payload"),
      update
    ]

    Enum.find_value(payloads, &absolute_token_usage_from_payload/1) ||
      Enum.find_value(payloads, &turn_completed_usage_from_payload/1) ||
      %{}
  end

  defp extract_rate_limits(update) do
    rate_limits_from_payload(update[:rate_limits]) ||
      rate_limits_from_payload(Map.get(update, "rate_limits")) ||
      rate_limits_from_payload(Map.get(update, :rate_limits)) ||
      rate_limits_from_payload(update[:payload]) ||
      rate_limits_from_payload(Map.get(update, "payload")) ||
      rate_limits_from_payload(update)
  end

  defp absolute_token_usage_from_payload(payload) when is_map(payload) do
    absolute_paths = [
      ["params", "msg", "payload", "info", "total_token_usage"],
      [:params, :msg, :payload, :info, :total_token_usage],
      ["params", "msg", "info", "total_token_usage"],
      [:params, :msg, :info, :total_token_usage],
      ["params", "tokenUsage", "total"],
      [:params, :tokenUsage, :total],
      ["tokenUsage", "total"],
      [:tokenUsage, :total]
    ]

    explicit_map_at_paths(payload, absolute_paths)
  end

  defp absolute_token_usage_from_payload(_payload), do: nil

  defp turn_completed_usage_from_payload(payload) when is_map(payload) do
    method = Map.get(payload, "method") || Map.get(payload, :method)

    if method in ["turn/completed", :turn_completed] do
      direct =
        Map.get(payload, "usage") ||
          Map.get(payload, :usage) ||
          map_at_path(payload, ["params", "usage"]) ||
          map_at_path(payload, [:params, :usage])

      if is_map(direct) and integer_token_map?(direct), do: direct
    end
  end

  defp turn_completed_usage_from_payload(_payload), do: nil

  defp rate_limits_from_payload(payload) when is_map(payload) do
    direct = Map.get(payload, "rate_limits") || Map.get(payload, :rate_limits)

    cond do
      rate_limits_map?(direct) ->
        direct

      rate_limits_map?(payload) ->
        payload

      true ->
        rate_limit_payloads(payload)
    end
  end

  defp rate_limits_from_payload(payload) when is_list(payload) do
    rate_limit_payloads(payload)
  end

  defp rate_limits_from_payload(_payload), do: nil

  defp rate_limit_payloads(payload) when is_map(payload) do
    Map.values(payload)
    |> Enum.reduce_while(nil, fn
      value, nil ->
        case rate_limits_from_payload(value) do
          nil -> {:cont, nil}
          rate_limits -> {:halt, rate_limits}
        end

      _value, result ->
        {:halt, result}
    end)
  end

  defp rate_limit_payloads(payload) when is_list(payload) do
    payload
    |> Enum.reduce_while(nil, fn
      value, nil ->
        case rate_limits_from_payload(value) do
          nil -> {:cont, nil}
          rate_limits -> {:halt, rate_limits}
        end

      _value, result ->
        {:halt, result}
    end)
  end

  defp rate_limits_map?(payload) when is_map(payload) do
    limit_id =
      Map.get(payload, "limit_id") ||
        Map.get(payload, :limit_id) ||
        Map.get(payload, "limit_name") ||
        Map.get(payload, :limit_name)

    has_buckets =
      Enum.any?(
        ["primary", :primary, "secondary", :secondary, "credits", :credits],
        &Map.has_key?(payload, &1)
      )

    !is_nil(limit_id) and has_buckets
  end

  defp rate_limits_map?(_payload), do: false

  defp explicit_map_at_paths(payload, paths) when is_map(payload) and is_list(paths) do
    Enum.find_value(paths, fn path ->
      value = map_at_path(payload, path)

      if is_map(value) and integer_token_map?(value), do: value
    end)
  end

  defp explicit_map_at_paths(_payload, _paths), do: nil

  defp map_at_path(payload, path) when is_map(payload) and is_list(path) do
    Enum.reduce_while(path, payload, fn key, acc ->
      if is_map(acc) and Map.has_key?(acc, key) do
        {:cont, Map.get(acc, key)}
      else
        {:halt, nil}
      end
    end)
  end

  defp map_at_path(_payload, _path), do: nil

  defp integer_token_map?(payload) do
    token_fields = [
      :input_tokens,
      :output_tokens,
      :total_tokens,
      :prompt_tokens,
      :completion_tokens,
      :inputTokens,
      :outputTokens,
      :totalTokens,
      :promptTokens,
      :completionTokens,
      "input_tokens",
      "output_tokens",
      "total_tokens",
      "prompt_tokens",
      "completion_tokens",
      "inputTokens",
      "outputTokens",
      "totalTokens",
      "promptTokens",
      "completionTokens"
    ]

    token_fields
    |> Enum.any?(fn field ->
      value = payload_get(payload, field)
      !is_nil(integer_like(value))
    end)
  end

  defp get_token_usage(usage, :input),
    do:
      payload_get(usage, [
        "input_tokens",
        "prompt_tokens",
        :input_tokens,
        :prompt_tokens,
        :input,
        "promptTokens",
        :promptTokens,
        "inputTokens",
        :inputTokens
      ])

  defp get_token_usage(usage, :output),
    do:
      payload_get(usage, [
        "output_tokens",
        "completion_tokens",
        :output_tokens,
        :completion_tokens,
        :output,
        :completion,
        "outputTokens",
        :outputTokens,
        "completionTokens",
        :completionTokens
      ])

  defp get_token_usage(usage, :total),
    do:
      payload_get(usage, [
        "total_tokens",
        "total",
        :total_tokens,
        :total,
        "totalTokens",
        :totalTokens
      ])

  defp payload_get(payload, fields) when is_list(fields) do
    Enum.find_value(fields, fn field -> map_integer_value(payload, field) end)
  end

  defp payload_get(payload, field), do: map_integer_value(payload, field)

  defp map_integer_value(payload, field) do
    if is_map(payload) do
      value = Map.get(payload, field)
      integer_like(value)
    else
      nil
    end
  end

  defp running_seconds(%DateTime{} = started_at, %DateTime{} = now) do
    max(0, DateTime.diff(now, started_at, :second))
  end

  defp running_seconds(_started_at, _now), do: 0

  defp integer_like(value) when is_integer(value) and value >= 0, do: value

  defp integer_like(value) when is_binary(value) do
    case Integer.parse(String.trim(value)) do
      {num, _} when num >= 0 -> num
      _ -> nil
    end
  end

  defp integer_like(_value), do: nil
end
