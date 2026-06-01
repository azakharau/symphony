defmodule SymphonyElixir.Runner.OpenCodeDispatch do
  @moduledoc """
  Shared OpenCode issue dispatch workflow.

  This module owns task-packet lookup, policy checks, Linear comment/state
  transitions, and policy/result events. The OpenCode adapter remains focused on
  runner process/session mechanics.
  """

  require Logger

  alias SymphonyElixir.{Config, Linear.Issue, ProcessPolicy, Tracker}
  alias SymphonyElixir.OpenCode.Runner, as: OpenCodeRunner
  alias SymphonyElixir.Runner.{OpenCodeAdapter, Outcome}

  @behaviour SymphonyElixir.Runner.Adapter

  @impl true
  def capabilities, do: %{remote_worker_hosts: false}

  @impl true
  @spec run(SymphonyElixir.Runner.Adapter.context()) :: Outcome.t() | {:error, term()}
  def run(%{workspace: workspace, issue: issue, opts: opts, emit_update: emit_update} = context) do
    case latest_opencode_task_packet_or_reroute(issue, emit_update) do
      {:ok, packet} ->
        with {:ok, decisions} <- Tracker.review_decisions(issue.id) do
          dispatch_opencode_packet(context, workspace, issue, packet, decisions, opts, emit_update)
        end

      %Outcome{} = outcome ->
        outcome

      {:error, reason} ->
        {:error, reason}
    end
  end

  defp dispatch_opencode_packet(context, workspace, issue, packet, decisions, opts, emit_update) do
    case ProcessPolicy.opencode_dispatch_decision(packet, decisions) do
      :allow ->
        emit_update.(%{event: :dispatch_allowed, phase: :dispatch, timestamp: DateTime.utc_now()})

        runner_context = Map.merge(context, %{workspace: workspace, issue: issue, task_packet: packet, opts: opts})

        case OpenCodeAdapter.run(runner_context) do
          {:ok, result} ->
            record_opencode_handoff(issue, result, emit_update)

          {:error, {:opencode_remote_worker_host_unsupported, details}} ->
            block_remote_opencode_worker_host(issue, details, emit_update)

          {:error, reason} ->
            {:error, reason}
        end

      {:block, block} ->
        {:ok, rca_required_state} = ProcessPolicy.codex_owned_rca_required_state()
        block_loop_breaker_dispatch(issue, block, rca_required_state, emit_update)
    end
  end

  defp latest_opencode_task_packet_or_reroute(%Issue{id: issue_id} = issue, emit_update)
       when is_binary(issue_id) do
    case Tracker.latest_opencode_task_packet(issue_id) do
      {:ok, packet} ->
        {:ok, packet}

      {:error, :opencode_task_prompt_not_found} ->
        reroute_missing_opencode_task_prompt(issue, emit_update)

      {:error, reason}
      when reason in [
             :opencode_task_prompt_missing_slice_id,
             :opencode_task_prompt_empty,
             :opencode_task_prompt_malformed_fence
           ] ->
        block_malformed_opencode_task_prompt(issue, reason, emit_update)

      {:error, reason} ->
        {:error, reason}
    end
  end

  defp latest_opencode_task_packet_or_reroute(_issue, _emit_update), do: {:error, :opencode_task_prompt_not_found}

  defp block_malformed_opencode_task_prompt(%Issue{id: issue_id} = issue, reason, emit_update)
       when is_binary(issue_id) do
    Logger.warning("OpenCode task prompt malformed for #{issue_context(issue)} reason=#{inspect(reason)}")

    {:ok, rca_required_state} = ProcessPolicy.codex_owned_rca_required_state()
    block_malformed_with_rca_state(issue, issue_id, reason, rca_required_state, emit_update)
  end

  defp block_loop_breaker_dispatch(issue, block, rca_required_state, emit_update) do
    block = Map.put(block, :rca_required_state, rca_required_state)

    Logger.warning(
      "OpenCode dispatch blocked for #{issue_context(issue)} reason=#{inspect(block[:reason])} " <>
        "slice_id=#{block[:slice_id]} rejection_count=#{block[:rejection_count]}"
    )

    emit_update.(%{
      event: :loop_breaker_blocked,
      phase: :policy_blocked,
      outcome: :rerouted,
      timestamp: DateTime.utc_now(),
      result_state: rca_required_state,
      failure: %{reason: block[:reason], slice_id: block[:slice_id], rejection_count: block[:rejection_count]}
    })

    with :ok <- Tracker.create_comment(issue.id, ProcessPolicy.loop_breaker_comment(block)),
         :ok <- Tracker.update_issue_state(issue.id, rca_required_state) do
      Outcome.rerouted(reason: block[:reason], result_state: rca_required_state, failure: block)
    end
  end

  defp block_malformed_with_rca_state(issue, issue_id, reason, rca_required_state, emit_update) do
    emit_update.(%{
      event: :malformed_task_prompt_blocked,
      phase: :policy_blocked,
      outcome: :rerouted,
      timestamp: DateTime.utc_now(),
      result_state: rca_required_state,
      failure: %{reason: reason}
    })

    comment = malformed_opencode_task_prompt_comment(issue, reason, rca_required_state)

    with :ok <- Tracker.create_comment(issue_id, comment),
         :ok <- Tracker.update_issue_state(issue_id, rca_required_state) do
      Outcome.rerouted(reason: reason, result_state: rca_required_state, failure: %{reason: reason})
    end
  end

  defp record_opencode_handoff(issue, result, emit_update) do
    result_state = Config.settings!().opencode.result_state

    with :ok <- Tracker.create_comment(issue.id, OpenCodeRunner.handoff_comment(issue, result)),
         :ok <- Tracker.update_issue_state(issue.id, result_state) do
      emit_update.(%{
        event: :handoff_recorded,
        phase: :completed,
        outcome: :completed,
        timestamp: DateTime.utc_now(),
        result_state: result_state,
        command: result.command,
        runner_owner: Map.get(result, :runner_owner),
        session_id: Map.get(result, :session_id),
        project_root: Map.get(result, :project_root),
        attach_url: Map.get(result, :attach_url)
      })

      Outcome.completed(result_state: result_state)
    end
  end

  defp reroute_missing_opencode_task_prompt(%Issue{id: issue_id} = issue, emit_update)
       when is_binary(issue_id) do
    target_state = codex_reroute_state()

    Logger.warning("OpenCode task prompt missing for #{issue_context(issue)}; rerouting to #{target_state}")

    emit_update.(%{
      event: :missing_task_prompt_rerouted,
      phase: :policy_reroute,
      outcome: :rerouted,
      timestamp: DateTime.utc_now(),
      result_state: target_state,
      failure: %{reason: :opencode_task_prompt_not_found}
    })

    with :ok <- Tracker.create_comment(issue_id, missing_opencode_task_prompt_comment(issue, target_state)),
         :ok <- Tracker.update_issue_state(issue_id, target_state) do
      Outcome.rerouted(reason: :opencode_task_prompt_not_found, result_state: target_state)
    end
  end

  defp codex_reroute_state do
    settings = Config.settings!()
    active_states = settings.tracker.active_states
    route_preferences = ["Todo", "In Review", "Need Owner Input", "RCA Required"]

    (route_preferences ++ active_states)
    |> Enum.uniq()
    |> Enum.find(fn state_name ->
      is_binary(state_name) and runner_kind_for_state(state_name, settings) == "codex"
    end)
  end

  defp runner_kind_for_state(state_name, settings) when is_binary(state_name) do
    Map.get(settings.runner.routes, normalize_issue_state(state_name), settings.runner.default)
  end

  defp missing_opencode_task_prompt_comment(%Issue{} = issue, target_state) do
    """
    ## Symphony Routing Diagnostic

    OpenCode task prompt missing for `#{issue.identifier}`.

    This issue is currently in `#{issue.state}`, which routes to OpenCode, but Symphony did not find an architect-authored comment marker:

    `<!-- symphony:opencode-task-prompt:v1 ... -->`

    OpenCode was not started. Symphony is moving this issue back to `#{target_state}` so Codex/Machine Architect can create the required task packet or move the issue to the correct workflow state.
    """
  end

  defp malformed_opencode_task_prompt_comment(%Issue{} = issue, reason, rca_required_state) do
    """
    ## Symphony Routing Diagnostic

    OpenCode task prompt malformed for `#{issue.identifier}`.

    Symphony found the `symphony:opencode-task-prompt:v1` marker, but the packet is not runnable: `#{inspect(reason)}`.

    OpenCode was not started. Symphony is moving this issue to `#{rca_required_state}` so Codex/Machine Architect can replace the task packet with a valid fenced prompt and a non-empty `slice_id` before this issue can run in OpenCode.
    """
  end

  defp block_remote_opencode_worker_host(%Issue{id: issue_id} = issue, details, emit_update)
       when is_binary(issue_id) and is_map(details) do
    Logger.warning(
      "OpenCode remote worker_host blocked for #{issue_context(issue)} worker_host=#{inspect(details[:worker_host])} workspace=#{inspect(details[:workspace])} project_root=#{inspect(details[:project_root])}"
    )

    {:ok, rca_required_state} = ProcessPolicy.codex_owned_rca_required_state()

    emit_update.(%{
      event: :remote_worker_host_blocked,
      phase: :policy_blocked,
      outcome: :rerouted,
      timestamp: DateTime.utc_now(),
      result_state: rca_required_state,
      worker_host: details[:worker_host],
      project_root: details[:project_root],
      failure: Map.put(details, :reason, :opencode_remote_worker_host_unsupported)
    })

    with :ok <-
           Tracker.create_comment(
             issue_id,
             remote_worker_host_blocked_comment(issue, details, rca_required_state)
           ),
         :ok <- Tracker.update_issue_state(issue_id, rca_required_state) do
      Outcome.rerouted(
        reason: :opencode_remote_worker_host_unsupported,
        detail: details[:worker_host],
        result_state: rca_required_state,
        failure: Map.put(details, :reason, :opencode_remote_worker_host_unsupported)
      )
    end
  end

  defp remote_worker_host_blocked_comment(%Issue{} = issue, details, rca_required_state) do
    """
    ## Symphony Routing Diagnostic

    OpenCode remote worker host is not supported for `#{issue.identifier}`.

    worker_host: #{details[:worker_host]}
    workspace: #{details[:workspace]}
    project_root: #{details[:project_root]}

    OpenCode was not started locally. Symphony is moving this issue to `#{rca_required_state}` so Codex can repair the runner scheduling contract before this issue can run in OpenCode.
    """
  end

  defp normalize_issue_state(state_name) when is_binary(state_name) do
    state_name |> String.trim() |> String.downcase()
  end

  defp issue_context(%Issue{id: issue_id, identifier: identifier}) do
    "issue_id=#{issue_id} issue_identifier=#{identifier}"
  end
end
