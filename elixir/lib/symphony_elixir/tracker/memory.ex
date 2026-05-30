defmodule SymphonyElixir.Tracker.Memory do
  @moduledoc """
  In-memory tracker adapter used for tests and local development.
  """

  @behaviour SymphonyElixir.Tracker

  alias SymphonyElixir.Linear.Issue
  alias SymphonyElixir.ReviewDecision
  alias SymphonyElixir.OpenCode.TaskPrompt

  @spec fetch_candidate_issues() :: {:ok, [Issue.t()]} | {:error, term()}
  def fetch_candidate_issues do
    {:ok, issue_entries()}
  end

  @spec fetch_issues_by_states([String.t()]) :: {:ok, [Issue.t()]} | {:error, term()}
  def fetch_issues_by_states(state_names) do
    normalized_states =
      state_names
      |> Enum.map(&normalize_state/1)
      |> MapSet.new()

    {:ok,
     Enum.filter(issue_entries(), fn %Issue{state: state} ->
       MapSet.member?(normalized_states, normalize_state(state))
     end)}
  end

  @spec fetch_issue_states_by_ids([String.t()]) :: {:ok, [Issue.t()]} | {:error, term()}
  def fetch_issue_states_by_ids(issue_ids) do
    wanted_ids = MapSet.new(issue_ids)

    {:ok,
     Enum.filter(issue_entries(), fn %Issue{id: id} ->
       MapSet.member?(wanted_ids, id)
     end)}
  end

  @spec fetch_project_milestones() :: {:ok, [map()]}
  def fetch_project_milestones do
    {:ok, Application.get_env(:symphony_elixir, :memory_tracker_project_milestones, [])}
  end

  @spec create_comment(String.t(), String.t()) :: :ok | {:error, term()}
  def create_comment(issue_id, body) do
    send_event({:memory_tracker_comment, issue_id, body})
    :ok
  end

  @spec update_issue_state(String.t(), String.t()) :: :ok | {:error, term()}
  def update_issue_state(issue_id, state_name) do
    send_event({:memory_tracker_state_update, issue_id, state_name})
    :ok
  end

  @spec latest_opencode_task_prompt(String.t()) :: {:ok, String.t()} | {:error, term()}
  def latest_opencode_task_prompt(issue_id) when is_binary(issue_id) do
    with {:ok, packet} <- latest_opencode_task_packet(issue_id) do
      {:ok, packet.prompt}
    end
  end

  @spec latest_opencode_task_packet(String.t()) :: {:ok, TaskPrompt.Packet.t()} | {:error, term()}
  def latest_opencode_task_packet(issue_id) when is_binary(issue_id) do
    comments =
      memory_comments(issue_id)

    comments
    |> List.wrap()
    |> Enum.reverse()
    |> Enum.find_value(fn body ->
      case TaskPrompt.extract_packet(body) do
        {:ok, packet} -> {:ok, packet}
        {:error, _reason} -> nil
      end
    end)
    |> case do
      {:ok, packet} -> {:ok, packet}
      nil -> latest_packet_from_issue_description(issue_id)
    end
  end

  @spec review_decisions(String.t()) :: {:ok, [ReviewDecision.t()]} | {:error, term()}
  def review_decisions(issue_id) when is_binary(issue_id) do
    {:ok, ReviewDecision.extract_many(memory_comments(issue_id))}
  end

  defp latest_packet_from_issue_description(issue_id) do
    issue_entries()
    |> Enum.find(&(&1.id == issue_id))
    |> case do
      %Issue{description: description} -> TaskPrompt.extract_packet(description)
      _ -> {:error, :opencode_task_prompt_not_found}
    end
  end

  defp memory_comments(issue_id) do
    :symphony_elixir
    |> Application.get_env(:memory_tracker_opencode_comments, %{})
    |> Map.get(issue_id, [])
    |> List.wrap()
  end

  defp configured_issues do
    Application.get_env(:symphony_elixir, :memory_tracker_issues, [])
  end

  defp issue_entries do
    Enum.filter(configured_issues(), &match?(%Issue{}, &1))
  end

  defp send_event(message) do
    case Application.get_env(:symphony_elixir, :memory_tracker_recipient) do
      pid when is_pid(pid) -> send(pid, message)
      _ -> :ok
    end
  end

  defp normalize_state(state) when is_binary(state) do
    state
    |> String.trim()
    |> String.downcase()
  end

  defp normalize_state(_state), do: ""
end
