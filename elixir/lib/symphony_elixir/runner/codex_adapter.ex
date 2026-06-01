defmodule SymphonyElixir.Runner.CodexAdapter do
  @moduledoc """
  Codex runner adapter.

  Owns Codex AppServer turn/session mechanics while AgentRunner remains the
  workspace lifecycle owner.
  """

  @behaviour SymphonyElixir.Runner.Adapter

  require Logger

  alias SymphonyElixir.Codex.AppServer
  alias SymphonyElixir.{Config, Linear.Issue, PromptBuilder, Tracker}

  @impl true
  def capabilities, do: %{remote_worker_hosts: true}

  @impl true
  def run(
        %{
          workspace: workspace,
          issue: issue,
          update_recipient: update_recipient,
          opts: opts,
          worker_host: worker_host,
          emit_update: _emit_update
        } = context
      ) do
    project_context = Map.get(context, :project_context)
    settings = Map.get(context, :settings) || Config.settings!(project_context)

    run_codex_turns(workspace, issue, update_recipient, opts, worker_host, settings, project_context)
  end

  defp codex_message_handler(recipient, issue) do
    fn message -> send_codex_update(recipient, issue, message) end
  end

  defp send_codex_update(recipient, %Issue{id: issue_id}, message)
       when is_binary(issue_id) and is_pid(recipient) do
    send(recipient, {:codex_worker_update, issue_id, message})
    :ok
  end

  defp send_codex_update(_recipient, _issue, _message), do: :ok

  defp run_codex_turns(workspace, issue, codex_update_recipient, opts, worker_host, settings, project_context) do
    max_turns = Keyword.get(opts, :max_turns, settings.agent.max_turns)
    issue_state_fetcher = Keyword.get(opts, :issue_state_fetcher, fn ids -> Tracker.fetch_issue_states_by_ids(ids, project_context) end)
    codex_workspace = codex_project_root(workspace, settings)

    with {:ok, session} <- AppServer.start_session(codex_workspace, worker_host: worker_host) do
      try do
        do_run_codex_turns(
          session,
          codex_workspace,
          issue,
          codex_update_recipient,
          opts,
          issue_state_fetcher,
          1,
          max_turns,
          settings
        )
      after
        AppServer.stop_session(session)
      end
    end
  end

  defp codex_project_root(default_workspace, settings) do
    case settings.codex.project_root do
      project_root when is_binary(project_root) and project_root != "" -> project_root
      _ -> default_workspace
    end
  end

  defp do_run_codex_turns(app_session, workspace, issue, codex_update_recipient, opts, issue_state_fetcher, turn_number, max_turns, settings) do
    prompt = build_turn_prompt(issue, opts, turn_number, max_turns)

    with {:ok, turn_session} <-
           AppServer.run_turn(
             app_session,
             prompt,
             issue,
             on_message: codex_message_handler(codex_update_recipient, issue)
           ) do
      Logger.info("Completed agent run for #{issue_context(issue)} session_id=#{turn_session[:session_id]} workspace=#{workspace} turn=#{turn_number}/#{max_turns}")

      case continue_with_issue?(issue, issue_state_fetcher, settings) do
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
            max_turns,
            settings
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

  defp continue_with_issue?(%Issue{id: issue_id} = issue, issue_state_fetcher, settings) when is_binary(issue_id) do
    case issue_state_fetcher.([issue_id]) do
      {:ok, [%Issue{} = refreshed_issue | _]} ->
        cond do
          not active_issue_state?(refreshed_issue.state, settings) -> {:done, refreshed_issue}
          owner_input_issue_state?(refreshed_issue.state) -> {:done, refreshed_issue}
          runner_kind_for_issue(refreshed_issue, settings) == "codex" -> {:continue, refreshed_issue}
          true -> {:handoff, refreshed_issue, runner_kind_for_issue(refreshed_issue, settings)}
        end

      {:ok, []} ->
        {:done, issue}

      {:error, reason} ->
        {:error, {:issue_state_refresh_failed, reason}}
    end
  end

  defp continue_with_issue?(issue, _issue_state_fetcher, _settings), do: {:done, issue}

  defp active_issue_state?(state_name, settings) when is_binary(state_name) do
    normalized_state = normalize_issue_state(state_name)

    settings.tracker.active_states
    |> Enum.any?(fn active_state -> normalize_issue_state(active_state) == normalized_state end)
  end

  defp active_issue_state?(_state_name, _settings), do: false

  defp owner_input_issue_state?(state_name) when is_binary(state_name) do
    normalize_issue_state(state_name) == "need owner input"
  end

  defp runner_kind_for_issue(%Issue{state: state_name}, settings) when is_binary(state_name) do
    Map.get(settings.runner.routes, normalize_issue_state(state_name), settings.runner.default)
  end

  defp normalize_issue_state(state_name) when is_binary(state_name) do
    state_name |> String.trim() |> String.downcase()
  end

  defp issue_context(%Issue{id: issue_id, identifier: identifier}) do
    "issue_id=#{issue_id} issue_identifier=#{identifier}"
  end
end
