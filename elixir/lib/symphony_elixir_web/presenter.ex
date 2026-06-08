defmodule SymphonyElixirWeb.Presenter do
  @moduledoc """
  Shared projections for the observability API and dashboard.
  """

  alias SymphonyElixir.{Config, Orchestrator, StatusDashboard}

  @spec state_payload(GenServer.name(), timeout()) :: map()
  def state_payload(orchestrator, snapshot_timeout_ms) do
    generated_at = DateTime.utc_now() |> DateTime.truncate(:second) |> DateTime.to_iso8601()

    case Orchestrator.snapshot(orchestrator, snapshot_timeout_ms) do
      %{} = snapshot ->
        %{
          generated_at: generated_at,
          counts: %{
            running: length(snapshot.running),
            retrying: length(snapshot.retrying),
            blocked: length(Map.get(snapshot, :blocked, []))
          },
          running: Enum.map(snapshot.running, &running_entry_payload/1),
          retrying: Enum.map(snapshot.retrying, &retry_entry_payload/1),
          blocked: Enum.map(Map.get(snapshot, :blocked, []), &blocked_entry_payload/1),
          codex_totals: snapshot.codex_totals,
          runner_runtime_totals: runner_runtime_totals(snapshot),
          suppression_events: Map.get(snapshot, :suppression_events, []),
          suppression_counts: Map.get(snapshot, :suppression_counts, %{}),
          rate_limits: snapshot.rate_limits,
          polling: Map.get(snapshot, :polling),
          active_milestone: Map.get(snapshot, :active_milestone)
        }

      :timeout ->
        %{generated_at: generated_at, error: %{code: "snapshot_timeout", message: "Snapshot timed out"}}

      :unavailable ->
        %{generated_at: generated_at, error: %{code: "snapshot_unavailable", message: "Snapshot unavailable"}}
    end
  end

  defp runner_runtime_totals(snapshot) when is_map(snapshot) do
    Map.get(snapshot, :runner_runtime_totals) || %{seconds_running: Map.get(Map.get(snapshot, :codex_totals, %{}), :seconds_running, 0)}
  end

  @spec issue_payload(String.t(), GenServer.name(), timeout()) :: {:ok, map()} | {:error, :issue_not_found}
  def issue_payload(issue_identifier, orchestrator, snapshot_timeout_ms) when is_binary(issue_identifier) do
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
          {:ok, issue_payload_body(issue_identifier, running, retry, blocked, running_entries, retry_entries, blocked_entries)}
        end

      _ ->
        {:error, :issue_not_found}
    end
  end

  @spec refresh_payload(GenServer.name()) :: {:ok, map()} | {:error, :unavailable}
  def refresh_payload(orchestrator) do
    case Orchestrator.request_refresh(orchestrator) do
      :unavailable ->
        {:error, :unavailable}

      payload ->
        {:ok, Map.update!(payload, :requested_at, &DateTime.to_iso8601/1)}
    end
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
