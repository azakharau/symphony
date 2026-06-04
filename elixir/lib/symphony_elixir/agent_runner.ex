defmodule SymphonyElixir.AgentRunner do
  @moduledoc """
  Executes a single Linear issue in its workspace with the configured runner adapter.
  """

  require Logger

  alias SymphonyElixir.{Config, Linear.Issue, Workspace}
  alias SymphonyElixir.Runner.{CodexAdapter, OpenCodeDispatch, Outcome}

  @type worker_host :: String.t() | nil

  @spec run(map(), pid() | nil, keyword()) :: :ok | no_return()
  def run(issue, update_recipient \\ nil, opts \\ []) do
    project_context = Keyword.get(opts, :project_context)
    settings = Keyword.get(opts, :settings) || Config.settings!(project_context)

    # The orchestrator owns host retries so one worker lifetime never hops machines.
    case adapter_for_runner_kind(runner_kind_for_issue(issue, settings)) do
      {:ok, adapter} ->
        worker_host =
          selected_worker_host(
            Keyword.get(opts, :worker_host),
            settings.worker.ssh_hosts,
            adapter.capabilities()
          )

        Logger.info("Starting agent run for #{issue_context(issue)} worker_host=#{worker_host_for_log(worker_host)}")

        case run_on_worker_host(issue, update_recipient, opts, worker_host, settings, project_context) do
          success when success == :ok or is_struct(success, Outcome) ->
            :ok

          {:error, reason} ->
            Logger.error("Agent run failed for #{issue_context(issue)}: #{inspect(reason)}")
            raise RuntimeError, "Agent run failed for #{issue_context(issue)}: #{inspect(reason)}"
        end

      {:error, reason} ->
        Logger.error("Agent run failed for #{issue_context(issue)}: #{inspect(reason)}")
        raise RuntimeError, "Agent run failed for #{issue_context(issue)}: #{inspect(reason)}"
    end
  end

  defp run_on_worker_host(issue, update_recipient, opts, worker_host, settings, project_context) do
    Logger.info("Starting worker attempt for #{issue_context(issue)} worker_host=#{worker_host_for_log(worker_host)}")

    case Workspace.create_for_issue(issue, worker_host, settings) do
      {:ok, workspace} ->
        send_worker_runtime_info(update_recipient, issue, worker_host, workspace)

        try do
          with :ok <- Workspace.run_before_run_hook(workspace, issue, worker_host, settings) do
            run_issue_with_configured_runner(
              workspace,
              issue,
              update_recipient,
              opts,
              worker_host,
              settings,
              project_context
            )
          end
        after
          Workspace.run_after_run_hook(workspace, issue, worker_host, settings)
        end

      {:error, reason} ->
        {:error, reason}
    end
  end

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

  defp run_issue_with_configured_runner(workspace, issue, update_recipient, opts, worker_host, settings, project_context) do
    runner_kind = runner_kind_for_issue(issue, settings)

    case adapter_for_runner_kind(runner_kind) do
      {:ok, adapter} ->
        adapter.run(%{
          workspace: workspace,
          issue: issue,
          update_recipient: update_recipient,
          opts: opts,
          settings: settings,
          project_context: project_context,
          worker_host: worker_host,
          emit_update: runner_update_emitter(update_recipient, issue, runner_kind)
        })

      {:error, reason} ->
        {:error, reason}
    end
  end

  defp adapter_for_runner_kind("codex"), do: {:ok, CodexAdapter}
  defp adapter_for_runner_kind("opencode"), do: {:ok, OpenCodeDispatch}
  defp adapter_for_runner_kind(other), do: {:error, {:unsupported_runner_kind, other}}

  defp runner_update_emitter(recipient, %Issue{id: issue_id}, runner_kind)
       when is_pid(recipient) and is_binary(issue_id) do
    fn update ->
      update = Map.put(update, :runner_kind, runner_kind)
      send(recipient, {:runner_worker_update, issue_id, update})
      :ok
    end
  end

  defp runner_update_emitter(_recipient, _issue, _runner_kind), do: fn _update -> :ok end

  defp runner_kind_for_issue(%Issue{state: state_name}, settings) when is_binary(state_name) do
    Map.get(
      settings.runner.routes,
      normalize_issue_state(state_name),
      settings.runner.default
    )
  end

  defp runner_kind_for_issue(_issue, settings), do: settings.runner.default

  defp selected_worker_host(_preferred_host, _configured_hosts, %{remote_worker_hosts: false}), do: nil

  defp selected_worker_host(nil, [], _capabilities), do: nil

  defp selected_worker_host(preferred_host, configured_hosts, _capabilities) when is_list(configured_hosts) do
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
