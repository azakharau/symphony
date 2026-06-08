defmodule SymphonyElixir.Runner.OpenCodeDispatch do
  @moduledoc """
  Shared OpenCode issue dispatch workflow.

  This module owns task-packet lookup, policy checks, Linear comment/state
  transitions, and policy/result events. The OpenCode adapter remains focused on
  runner process/session mechanics.
  """

  require Logger

  alias SymphonyElixir.{Config, Linear.Issue, OpenCode.TaskPrompt, ProcessPolicy, RuntimeCache, Tracker}
  alias SymphonyElixir.OpenCode.Runner, as: OpenCodeRunner
  alias SymphonyElixir.Runner.{OpenCodeAdapter, Outcome}

  @behaviour SymphonyElixir.Runner.Adapter

  @impl true
  def capabilities, do: %{remote_worker_hosts: false}

  @impl true
  @spec run(SymphonyElixir.Runner.Adapter.context()) :: Outcome.t() | {:error, term()}
  def run(%{workspace: workspace, issue: issue, opts: opts, emit_update: emit_update} = context) do
    project_context = Map.get(context, :project_context)

    case steward_task_packet(context) || latest_opencode_task_packet_or_reroute(issue, project_context, emit_update) do
      {:ok, packet} ->
        with {:ok, decisions} <- Tracker.review_decisions(issue.id, project_context) do
          dispatch_opencode_packet(context, workspace, issue, packet, decisions, opts, emit_update, project_context)
        end

      %Outcome{} = outcome ->
        outcome

      {:error, reason} ->
        {:error, reason}
    end
  end

  defp dispatch_opencode_packet(context, workspace, issue, packet, decisions, opts, emit_update, project_context) do
    case ProcessPolicy.opencode_dispatch_decision(packet, decisions, project_context) do
      :allow ->
        emit_update.(%{event: :dispatch_allowed, phase: :dispatch, timestamp: DateTime.utc_now()})

        runner_context = Map.merge(context, %{workspace: workspace, issue: issue, task_packet: packet, opts: opts})

        case OpenCodeAdapter.run(runner_context) do
          {:ok, result} ->
            record_opencode_handoff(issue, packet, result, emit_update, project_context)

          {:error, {:opencode_remote_worker_host_unsupported, details}} ->
            block_remote_opencode_worker_host(issue, details, emit_update, project_context)

          {:error, {:need_owner_input, {:opencode_acp_session_attached, session_id}}} ->
            record_opencode_attached_session(issue, session_id, emit_update, project_context)

          {:error, {:need_owner_input, reason}} ->
            record_opencode_owner_input(issue, reason, emit_update, project_context)

          {:error, {:opencode_acp_stalled, timeout_ms}} ->
            record_opencode_runner_failure_rca(
              issue,
              {:opencode_acp_stalled, timeout_ms},
              emit_update,
              project_context
            )

          {:error, reason} ->
            {:error, reason}
        end

      {:block, block} ->
        {:ok, rca_required_state} = ProcessPolicy.codex_owned_rca_required_state(project_context)
        block_loop_breaker_dispatch(issue, block, rca_required_state, emit_update, project_context)
    end
  end

  defp latest_opencode_task_packet_or_reroute(%Issue{id: issue_id} = issue, project_context, emit_update)
       when is_binary(issue_id) do
    case Tracker.latest_opencode_task_packet(issue_id, project_context) do
      {:ok, packet} ->
        {:ok, packet}

      {:error, :opencode_task_prompt_not_found} ->
        reroute_missing_opencode_task_prompt(issue, project_context, emit_update)

      {:error, reason}
      when reason in [
             :opencode_task_prompt_missing_slice_id,
             :opencode_task_prompt_empty,
             :opencode_task_prompt_malformed_fence,
             :opencode_task_prompt_forbidden_role_preamble
           ] ->
        block_malformed_opencode_task_prompt(issue, reason, project_context, emit_update)

      {:error, reason} ->
        {:error, reason}
    end
  end

  defp latest_opencode_task_packet_or_reroute(_issue, _project_context, _emit_update), do: {:error, :opencode_task_prompt_not_found}

  defp steward_task_packet(%{task_packet: %TaskPrompt.Packet{} = packet}), do: {:ok, packet}

  defp steward_task_packet(%{opts: opts}) when is_list(opts) do
    with prompt when is_binary(prompt) <- Keyword.get(opts, :steward_execution_prompt),
         packet when is_map(packet) <- Keyword.get(opts, :steward_execution_packet),
         {:ok, built} <- steward_task_packet_from_prompt(packet, prompt) do
      {:ok, built}
    else
      _ -> nil
    end
  end

  defp steward_task_packet(_context), do: nil

  defp steward_task_packet_from_prompt(%{"issue" => %{"id" => issue_id}}, prompt) when is_binary(issue_id) do
    {:ok,
     %TaskPrompt.Packet{
       prompt: prompt,
       slice_id: issue_id,
       fingerprint: packet_fingerprint(prompt)
     }}
  end

  defp steward_task_packet_from_prompt(_packet, _prompt), do: {:error, :missing_issue_id}

  defp block_malformed_opencode_task_prompt(%Issue{id: issue_id} = issue, reason, project_context, emit_update)
       when is_binary(issue_id) do
    Logger.warning("OpenCode task prompt malformed for #{issue_context(issue)} reason=#{inspect(reason)}")

    {:ok, rca_required_state} = ProcessPolicy.codex_owned_rca_required_state(project_context)
    block_malformed_with_rca_state(issue, issue_id, reason, rca_required_state, project_context, emit_update)
  end

  defp block_loop_breaker_dispatch(issue, block, rca_required_state, emit_update, project_context) do
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

    with :ok <- Tracker.create_comment(issue.id, ProcessPolicy.loop_breaker_comment(block), project_context),
         :ok <- Tracker.update_issue_state(issue.id, rca_required_state, project_context) do
      Outcome.rerouted(reason: block[:reason], result_state: rca_required_state, failure: block)
    end
  end

  defp block_malformed_with_rca_state(issue, issue_id, reason, rca_required_state, project_context, emit_update) do
    emit_update.(%{
      event: :malformed_task_prompt_blocked,
      phase: :policy_blocked,
      outcome: :rerouted,
      timestamp: DateTime.utc_now(),
      result_state: rca_required_state,
      failure: %{reason: reason}
    })

    comment = malformed_opencode_task_prompt_comment(issue, reason, rca_required_state)

    with :ok <- Tracker.create_comment(issue_id, comment, project_context),
         :ok <- Tracker.update_issue_state(issue_id, rca_required_state, project_context) do
      Outcome.rerouted(reason: reason, result_state: rca_required_state, failure: %{reason: reason})
    end
  end

  defp record_opencode_handoff(issue, packet, result, emit_update, project_context) do
    result_state = Config.settings!(project_context).opencode.result_state
    fingerprint = handoff_fingerprint(packet, result)

    if RuntimeCache.handoff_fingerprint_seen?(project_context, issue.id, fingerprint) do
      suppress_opencode_handoff(result, emit_update, result_state, "handoff_unchanged", "handoff unchanged")
    else
      create_opencode_handoff(issue, result, emit_update, project_context, result_state, fingerprint)
    end
  end

  defp create_opencode_handoff(issue, result, emit_update, project_context, result_state, fingerprint) do
    with :ok <- Tracker.create_comment(issue.id, OpenCodeRunner.handoff_comment(issue, result), project_context),
         :ok <- Tracker.update_issue_state(issue.id, result_state, project_context) do
      RuntimeCache.record_handoff_fingerprint(project_context, issue.id, fingerprint)

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

  defp suppress_opencode_handoff(result, emit_update, result_state, kind, reason) do
    emit_update.(%{
      event: :handoff_suppressed,
      phase: :completed,
      outcome: :completed,
      timestamp: DateTime.utc_now(),
      result_state: result_state,
      command: result.command,
      runner_owner: Map.get(result, :runner_owner),
      session_id: Map.get(result, :session_id),
      project_root: Map.get(result, :project_root),
      attach_url: Map.get(result, :attach_url),
      suppression_kind: kind,
      suppression_reason: reason
    })

    Outcome.completed(result_state: result_state)
  end

  defp packet_fingerprint(prompt) do
    :crypto.hash(:sha256, prompt)
    |> Base.encode16(case: :lower)
  end

  defp handoff_fingerprint(_packet, result) do
    :crypto.hash(
      :sha256,
      :erlang.term_to_binary({
        Map.get(result, :session_id),
        Map.get(result, :command),
        Map.get(result, :project_root),
        Map.get(result, :handoff),
        Map.get(result, :output)
      })
    )
    |> Base.encode16(case: :lower)
  end

  defp record_opencode_owner_input(issue, reason, emit_update, project_context) do
    emit_update.(%{
      event: :owner_input_requested,
      phase: :blocked,
      outcome: :rerouted,
      timestamp: DateTime.utc_now(),
      result_state: "Need Owner Input",
      failure: %{reason: :need_owner_input}
    })

    with :ok <- Tracker.create_comment(issue.id, opencode_owner_input_comment(issue, reason), project_context),
         :ok <- Tracker.update_issue_state(issue.id, "Need Owner Input", project_context) do
      Outcome.rerouted(reason: :need_owner_input, result_state: "Need Owner Input", failure: %{reason: reason})
    end
  end

  defp record_opencode_attached_session(issue, session_id, emit_update, project_context) do
    emit_update.(%{
      event: :session_attached,
      phase: :blocked,
      outcome: :rerouted,
      timestamp: DateTime.utc_now(),
      result_state: "Need Owner Input",
      session_id: session_id,
      failure: %{reason: :opencode_acp_session_attached}
    })

    with :ok <- Tracker.create_comment(issue.id, opencode_attached_session_comment(issue, session_id), project_context),
         :ok <- Tracker.update_issue_state(issue.id, "Need Owner Input", project_context) do
      Outcome.rerouted(
        reason: :opencode_acp_session_attached,
        result_state: "Need Owner Input",
        failure: %{reason: :opencode_acp_session_attached, session_id: session_id}
      )
    end
  end

  defp record_opencode_runner_failure_rca(issue, reason, emit_update, project_context) do
    {:ok, rca_required_state} = ProcessPolicy.codex_owned_rca_required_state(project_context)

    emit_update.(%{
      event: :runner_failure_rerouted,
      phase: :policy_reroute,
      outcome: :rerouted,
      timestamp: DateTime.utc_now(),
      result_state: rca_required_state,
      failure: runner_failure_payload(reason)
    })

    with :ok <- Tracker.create_comment(issue.id, opencode_runner_failure_comment(issue, reason, rca_required_state), project_context),
         :ok <- Tracker.update_issue_state(issue.id, rca_required_state, project_context) do
      Outcome.rerouted(reason: runner_failure_reason(reason), result_state: rca_required_state, failure: runner_failure_payload(reason))
    end
  end

  defp reroute_missing_opencode_task_prompt(%Issue{id: issue_id} = issue, project_context, emit_update)
       when is_binary(issue_id) do
    target_state = codex_reroute_state(project_context)

    Logger.warning("OpenCode task prompt missing for #{issue_context(issue)}; rerouting to #{target_state}")

    emit_update.(%{
      event: :missing_task_prompt_rerouted,
      phase: :policy_reroute,
      outcome: :rerouted,
      timestamp: DateTime.utc_now(),
      result_state: target_state,
      failure: %{reason: :opencode_task_prompt_not_found}
    })

    missing_prompt_comment = missing_opencode_task_prompt_comment(issue, target_state)

    with :ok <- Tracker.create_comment(issue_id, missing_prompt_comment, project_context),
         :ok <- Tracker.update_issue_state(issue_id, target_state, project_context) do
      Outcome.rerouted(reason: :opencode_task_prompt_not_found, result_state: target_state)
    end
  end

  defp codex_reroute_state(project_context) do
    settings = Config.settings!(project_context)
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

  defp opencode_runner_failure_comment(%Issue{} = issue, reason, target_state) do
    """
    ## Symphony OpenCode Runner Diagnostic

    OpenCode runner failed for #{issue_context(issue)} and Symphony moved the issue to `#{target_state}` for Codex Architect RCA.

    Failure:
    - #{runner_failure_human(reason)}

    Required Codex action:
    - Inspect the OpenCode session and repository state.
    - Decide whether to issue a corrected implementation prompt, request owner input, or reject/escalate the slice.
    - Do not leave this issue in `In Progress` without an active OpenCode session.
    """
    |> String.trim()
  end

  defp runner_failure_payload({:opencode_acp_stalled, timeout_ms}) do
    %{reason: :opencode_acp_stalled, timeout_ms: timeout_ms}
  end

  defp runner_failure_payload(reason), do: %{reason: runner_failure_reason(reason)}

  defp runner_failure_reason({reason, _detail}) when is_atom(reason), do: reason
  defp runner_failure_reason(reason) when is_atom(reason), do: reason
  defp runner_failure_reason(_reason), do: :opencode_runner_failed

  defp runner_failure_human({:opencode_acp_stalled, timeout_ms}) do
    "OpenCode ACP session produced no runner events for #{timeout_ms}ms."
  end

  defp runner_failure_human(reason), do: inspect(reason)

  defp malformed_opencode_task_prompt_comment(%Issue{} = issue, reason, rca_required_state) do
    """
    ## Symphony Routing Diagnostic

    OpenCode task prompt malformed for `#{issue.identifier}`.

    Symphony found the `symphony:opencode-task-prompt:v1` marker, but the packet is not runnable: `#{inspect(reason)}`.

    OpenCode was not started. Symphony is moving this issue to `#{rca_required_state}` so Codex/Machine Architect can replace the task packet with a valid fenced prompt and a non-empty `slice_id` before this issue can run in OpenCode.
    """
  end

  defp opencode_owner_input_comment(%Issue{} = issue, reason) do
    """
    ## OpenCode Handoff

    Issue: #{issue.identifier}
    Runner: OpenCode
    Status: need owner input

    OpenCode requested owner input or permission before it could complete.

    Reason:

    ```text
    #{inspect(reason)}
    ```
    """
  end

  defp opencode_attached_session_comment(%Issue{} = issue, session_id) do
    """
    ## OpenCode Session Attached

    Issue: #{issue.identifier}
    Runner: OpenCode ACP
    Status: session attached
    Session ID: `#{session_id}`

    Symphony found an existing persisted OpenCode ACP session for this issue and did not resend the task prompt. This is a session attachment guard, not a completed OpenCode handoff and not an owner-input request emitted by OpenCode.

    Inspect or continue the session in OpenCode, then move the issue back to `In Progress` when it is ready to run again, or provide owner direction in this issue.
    """
  end

  defp block_remote_opencode_worker_host(%Issue{id: issue_id} = issue, details, emit_update, project_context)
       when is_binary(issue_id) and is_map(details) do
    Logger.warning(
      "OpenCode remote worker_host blocked for #{issue_context(issue)} worker_host=#{inspect(details[:worker_host])} workspace=#{inspect(details[:workspace])} project_root=#{inspect(details[:project_root])}"
    )

    {:ok, rca_required_state} = ProcessPolicy.codex_owned_rca_required_state(project_context)

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
             remote_worker_host_blocked_comment(issue, details, rca_required_state),
             project_context
           ),
         :ok <- Tracker.update_issue_state(issue_id, rca_required_state, project_context) do
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
