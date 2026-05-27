defmodule SymphonyElixir.AgentRunner do
  @moduledoc """
  Executes a single Linear issue in its workspace with Codex.
  """

  require Logger
  alias SymphonyElixir.Codex.AppServer
  alias SymphonyElixir.OpenCode.Runner, as: OpenCodeRunner
  alias SymphonyElixir.{Config, Linear.Issue, ProcessPolicy, PromptBuilder, Tracker, Workspace}

  @type worker_host :: String.t() | nil

  @spec run(map(), pid() | nil, keyword()) :: :ok | no_return()
  def run(issue, codex_update_recipient \\ nil, opts \\ []) do
    # The orchestrator owns host retries so one worker lifetime never hops machines.
    worker_host = selected_worker_host(Keyword.get(opts, :worker_host), Config.settings!().worker.ssh_hosts)

    Logger.info("Starting agent run for #{issue_context(issue)} worker_host=#{worker_host_for_log(worker_host)}")

    case run_on_worker_host(issue, codex_update_recipient, opts, worker_host) do
      :ok ->
        :ok

      {:error, reason} ->
        Logger.error("Agent run failed for #{issue_context(issue)}: #{inspect(reason)}")
        raise RuntimeError, "Agent run failed for #{issue_context(issue)}: #{inspect(reason)}"
    end
  end

  defp run_on_worker_host(issue, codex_update_recipient, opts, worker_host) do
    Logger.info("Starting worker attempt for #{issue_context(issue)} worker_host=#{worker_host_for_log(worker_host)}")

    case Workspace.create_for_issue(issue, worker_host) do
      {:ok, workspace} ->
        send_worker_runtime_info(codex_update_recipient, issue, worker_host, workspace)

        try do
          with :ok <- Workspace.run_before_run_hook(workspace, issue, worker_host) do
            run_issue_with_configured_runner(workspace, issue, codex_update_recipient, opts, worker_host)
          end
        after
          Workspace.run_after_run_hook(workspace, issue, worker_host)
        end

      {:error, reason} ->
        {:error, reason}
    end
  end

  defp codex_message_handler(recipient, issue) do
    fn message ->
      send_codex_update(recipient, issue, message)
    end
  end

  defp send_codex_update(recipient, %Issue{id: issue_id}, message)
       when is_binary(issue_id) and is_pid(recipient) do
    send(recipient, {:codex_worker_update, issue_id, message})
    :ok
  end

  defp send_codex_update(_recipient, _issue, _message), do: :ok

  defp send_worker_runtime_info(recipient, %Issue{id: issue_id}, worker_host, workspace)
       when is_binary(issue_id) and is_pid(recipient) and is_binary(workspace) do
    send(
      recipient,
      {:worker_runtime_info, issue_id,
       %{
         worker_host: worker_host,
         workspace_path: workspace
       }}
    )

    :ok
  end

  defp send_worker_runtime_info(_recipient, _issue, _worker_host, _workspace), do: :ok

  defp run_issue_with_configured_runner(workspace, issue, codex_update_recipient, opts, worker_host) do
    case runner_kind_for_issue(issue) do
      "codex" ->
        run_codex_turns(workspace, issue, codex_update_recipient, opts, worker_host)

      "opencode" ->
        run_opencode_once(workspace, issue, opts)

      other ->
        {:error, {:unsupported_runner_kind, other}}
    end
  end

  defp run_opencode_once(workspace, issue, _opts) do
    with {:ok, packet} <- Tracker.latest_opencode_task_packet(issue.id),
         {:ok, decisions} <- Tracker.review_decisions(issue.id) do
      case ProcessPolicy.opencode_dispatch_decision(packet, decisions) do
        :allow ->
          with {:ok, result} <- OpenCodeRunner.run(workspace, issue, packet.prompt),
               :ok <- Tracker.create_comment(issue.id, OpenCodeRunner.handoff_comment(issue, result)),
               :ok <- Tracker.update_issue_state(issue.id, Config.settings!().opencode.result_state) do
            :ok
          end

        {:block, block} ->
          Logger.warning("OpenCode dispatch blocked for #{issue_context(issue)} reason=#{inspect(block[:reason])} slice_id=#{block[:slice_id]} rejection_count=#{block[:rejection_count]}")

          with :ok <- Tracker.create_comment(issue.id, ProcessPolicy.loop_breaker_comment(block)),
               :ok <- Tracker.update_issue_state(issue.id, block.rca_required_state) do
            :ok
          end
      end
    end
  end

  defp run_codex_turns(workspace, issue, codex_update_recipient, opts, worker_host) do
    max_turns = Keyword.get(opts, :max_turns, Config.settings!().agent.max_turns)
    issue_state_fetcher = Keyword.get(opts, :issue_state_fetcher, &Tracker.fetch_issue_states_by_ids/1)
    codex_workspace = codex_project_root(workspace)

    with {:ok, session} <- AppServer.start_session(codex_workspace, worker_host: worker_host) do
      try do
        do_run_codex_turns(session, codex_workspace, issue, codex_update_recipient, opts, issue_state_fetcher, 1, max_turns)
      after
        AppServer.stop_session(session)
      end
    end
  end

  defp codex_project_root(default_workspace) do
    case Config.settings!().codex.project_root do
      project_root when is_binary(project_root) and project_root != "" -> project_root
      _ -> default_workspace
    end
  end

  defp do_run_codex_turns(app_session, workspace, issue, codex_update_recipient, opts, issue_state_fetcher, turn_number, max_turns) do
    prompt = build_turn_prompt(issue, opts, turn_number, max_turns)

    with {:ok, turn_session} <-
           AppServer.run_turn(
             app_session,
             prompt,
             issue,
             on_message: codex_message_handler(codex_update_recipient, issue)
           ) do
      Logger.info("Completed agent run for #{issue_context(issue)} session_id=#{turn_session[:session_id]} workspace=#{workspace} turn=#{turn_number}/#{max_turns}")

      case continue_with_issue?(issue, issue_state_fetcher) do
        {:continue, refreshed_issue} when turn_number < max_turns ->
          Logger.info("Continuing agent run for #{issue_context(refreshed_issue)} after normal turn completion turn=#{turn_number}/#{max_turns}")

          do_run_codex_turns(
            app_session,
            workspace,
            refreshed_issue,
            codex_update_recipient,
            opts,
            issue_state_fetcher,
            turn_number + 1,
            max_turns
          )

        {:continue, refreshed_issue} ->
          Logger.info("Reached agent.max_turns for #{issue_context(refreshed_issue)} with issue still active; returning control to orchestrator")

          :ok

        {:handoff, refreshed_issue, runner_kind} ->
          Logger.info("Stopping Codex run for #{issue_context(refreshed_issue)} because refreshed issue now routes to #{runner_kind}")

          :ok

        {:done, _refreshed_issue} ->
          :ok

        {:error, reason} ->
          {:error, reason}
      end
    end
  end

  defp build_turn_prompt(issue, opts, 1, _max_turns), do: PromptBuilder.build_prompt(issue, opts)

  defp build_turn_prompt(%Issue{state: "Need Owner Input"} = issue, opts, turn_number, max_turns) do
    """
    Owner input refresh:

    - The previous Codex turn completed, but the Linear issue is still in Need Owner Input.
    - This is continuation turn ##{turn_number} of #{max_turns} for the current agent run.
    - Re-read the refreshed Linear issue below, including latest owner comments; comments may have arrived after the previous turn started.
    - If the owner answer resolves the blocker, update this same issue with the OpenCode handoff and move it to In Progress.
    - If it does not resolve the blocker, ask exactly one sharper follow-up question on this same issue.

    #{PromptBuilder.build_prompt(issue, opts)}
    """
  end

  defp build_turn_prompt(_issue, _opts, turn_number, max_turns) do
    """
    Continuation guidance:

    - The previous Codex turn completed normally, but the Linear issue is still in an active state.
    - This is continuation turn ##{turn_number} of #{max_turns} for the current agent run.
    - Resume from the current workspace and workpad state instead of restarting from scratch.
    - The original task instructions and prior turn context are already present in this thread, so do not restate them before acting.
    - Focus on the remaining ticket work and do not end the turn while the issue stays active unless you are truly blocked.
    """
  end

  defp continue_with_issue?(%Issue{id: issue_id} = issue, issue_state_fetcher) when is_binary(issue_id) do
    case issue_state_fetcher.([issue_id]) do
      {:ok, [%Issue{} = refreshed_issue | _]} ->
        cond do
          not active_issue_state?(refreshed_issue.state) ->
            {:done, refreshed_issue}

          runner_kind_for_issue(refreshed_issue) == "codex" ->
            {:continue, refreshed_issue}

          true ->
            {:handoff, refreshed_issue, runner_kind_for_issue(refreshed_issue)}
        end

      {:ok, []} ->
        {:done, issue}

      {:error, reason} ->
        {:error, {:issue_state_refresh_failed, reason}}
    end
  end

  defp continue_with_issue?(issue, _issue_state_fetcher), do: {:done, issue}

  defp active_issue_state?(state_name) when is_binary(state_name) do
    normalized_state = normalize_issue_state(state_name)

    Config.settings!().tracker.active_states
    |> Enum.any?(fn active_state -> normalize_issue_state(active_state) == normalized_state end)
  end

  defp active_issue_state?(_state_name), do: false

  defp runner_kind_for_issue(%Issue{state: state_name}) when is_binary(state_name) do
    settings = Config.settings!()

    Map.get(
      settings.runner.routes,
      normalize_issue_state(state_name),
      settings.runner.default
    )
  end

  defp runner_kind_for_issue(_issue), do: Config.settings!().runner.default

  defp selected_worker_host(nil, []), do: nil

  defp selected_worker_host(preferred_host, configured_hosts) when is_list(configured_hosts) do
    hosts =
      configured_hosts
      |> Enum.map(&String.trim/1)
      |> Enum.reject(&(&1 == ""))
      |> Enum.uniq()

    case preferred_host do
      host when is_binary(host) and host != "" -> host
      _ when hosts == [] -> nil
      _ -> List.first(hosts)
    end
  end

  defp worker_host_for_log(nil), do: "local"
  defp worker_host_for_log(worker_host), do: worker_host

  defp normalize_issue_state(state_name) when is_binary(state_name) do
    state_name
    |> String.trim()
    |> String.downcase()
  end

  defp issue_context(%Issue{id: issue_id, identifier: identifier}) do
    "issue_id=#{issue_id} issue_identifier=#{identifier}"
  end
end
