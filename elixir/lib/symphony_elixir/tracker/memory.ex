defmodule SymphonyElixir.Tracker.Memory do
  @moduledoc """
  In-memory tracker adapter used for tests and local development.
  """

  @behaviour SymphonyElixir.Tracker

  alias SymphonyElixir.Linear.Issue
  alias SymphonyElixir.OpenCode.TaskPrompt
  alias SymphonyElixir.ReviewDecision

  @spec fetch_candidate_issues() :: {:ok, [Issue.t()]} | {:error, term()}
  def fetch_candidate_issues do
    {:ok, issue_entries()}
  end

  @spec fetch_candidate_issues(term()) :: {:ok, [Issue.t()]} | {:error, term()}
  def fetch_candidate_issues(context) do
    {:ok, issue_entries(context)}
  end

  @spec fetch_issues_by_states([String.t()]) :: {:ok, [Issue.t()]} | {:error, term()}
  def fetch_issues_by_states(state_names) do
    fetch_issues_by_states(state_names, nil)
  end

  @spec fetch_issues_by_states([String.t()], term()) :: {:ok, [Issue.t()]} | {:error, term()}
  def fetch_issues_by_states(state_names, context) do
    normalized_states =
      state_names
      |> Enum.map(&normalize_state/1)
      |> MapSet.new()

    {:ok,
     Enum.filter(issue_entries(context), fn %Issue{state: state} ->
       MapSet.member?(normalized_states, normalize_state(state))
     end)}
  end

  @spec fetch_issue_states_by_ids([String.t()]) :: {:ok, [Issue.t()]} | {:error, term()}
  def fetch_issue_states_by_ids(issue_ids) do
    fetch_issue_states_by_ids(issue_ids, nil)
  end

  @spec fetch_issue_states_by_ids([String.t()], term()) :: {:ok, [Issue.t()]} | {:error, term()}
  def fetch_issue_states_by_ids(issue_ids, context) do
    wanted_ids = MapSet.new(issue_ids)

    {:ok,
     Enum.filter(issue_entries(context), fn %Issue{id: id} ->
       MapSet.member?(wanted_ids, id)
     end)}
  end

  @spec fetch_project_milestones() :: {:ok, [map()]}
  def fetch_project_milestones do
    {:ok, Application.get_env(:symphony_elixir, :memory_tracker_project_milestones, [])}
  end

  @spec fetch_project_milestones(term()) :: {:ok, [map()]}
  def fetch_project_milestones(context) do
    {:ok, project_env(:memory_tracker_project_milestones_by_project, context, :memory_tracker_project_milestones, [])}
  end

  @spec create_comment(String.t(), String.t()) :: :ok | {:error, term()}
  def create_comment(issue_id, body) do
    send_event({:memory_tracker_comment, issue_id, body})
    :ok
  end

  @spec create_comment(String.t(), String.t(), term()) :: :ok | {:error, term()}
  def create_comment(issue_id, body, context) do
    send_event({:memory_tracker_comment, issue_id, body}, context)
    :ok
  end

  @spec update_issue_state(String.t(), String.t()) :: :ok | {:error, term()}
  def update_issue_state(issue_id, state_name) do
    send_event({:memory_tracker_state_update, issue_id, state_name})
    :ok
  end

  @spec update_issue_state(String.t(), String.t(), term()) :: :ok | {:error, term()}
  def update_issue_state(issue_id, state_name, context) do
    send_event({:memory_tracker_state_update, issue_id, state_name}, context)
    :ok
  end

  @spec latest_opencode_task_prompt(String.t()) :: {:ok, String.t()} | {:error, term()}
  def latest_opencode_task_prompt(issue_id) when is_binary(issue_id) do
    latest_opencode_task_prompt(issue_id, nil)
  end

  @spec latest_opencode_task_prompt(String.t(), term()) :: {:ok, String.t()} | {:error, term()}
  def latest_opencode_task_prompt(issue_id, context) when is_binary(issue_id) do
    with {:ok, packet} <- latest_opencode_task_packet(issue_id, context) do
      {:ok, packet.prompt}
    end
  end

  @spec latest_opencode_task_packet(String.t()) :: {:ok, TaskPrompt.Packet.t()} | {:error, term()}
  def latest_opencode_task_packet(issue_id) when is_binary(issue_id) do
    latest_opencode_task_packet(issue_id, nil)
  end

  @spec latest_opencode_task_packet(String.t(), term()) :: {:ok, TaskPrompt.Packet.t()} | {:error, term()}
  def latest_opencode_task_packet(issue_id, context) when is_binary(issue_id) do
    comments =
      memory_comments(issue_id, context)

    comments
    |> latest_packet_from_comments()
    |> case do
      {:ok, packet} -> {:ok, packet}
      {:error, reason} -> {:error, reason}
      nil -> latest_packet_from_issue_description(issue_id, context)
    end
  end

  defp latest_packet_from_comments(comments) do
    comments
    |> List.wrap()
    |> Enum.reverse()
    |> Enum.find_value(&extract_opencode_packet_from_body/1)
  end

  defp extract_opencode_packet_from_body(body) when is_binary(body) do
    case TaskPrompt.extract_packet(body) do
      {:ok, packet} -> {:ok, packet}
      {:error, :opencode_task_prompt_not_found} -> nil
      {:error, reason} -> if TaskPrompt.marker_present?(body), do: {:error, reason}, else: nil
    end
  end

  defp extract_opencode_packet_from_body(_body), do: nil

  @spec review_decisions(String.t()) :: {:ok, [ReviewDecision.t()]} | {:error, term()}
  def review_decisions(issue_id) when is_binary(issue_id) do
    review_decisions(issue_id, nil)
  end

  @spec review_decisions(String.t(), term()) :: {:ok, [ReviewDecision.t()]} | {:error, term()}
  def review_decisions(issue_id, context) when is_binary(issue_id) do
    {:ok, ReviewDecision.extract_many(memory_comments(issue_id, context))}
  end

  defp latest_packet_from_issue_description(issue_id, context) do
    issue_entries(context)
    |> Enum.find(&(&1.id == issue_id))
    |> case do
      %Issue{description: description} -> TaskPrompt.extract_packet(description)
      _ -> {:error, :opencode_task_prompt_not_found}
    end
  end

  defp memory_comments(issue_id, nil) do
    :symphony_elixir
    |> Application.get_env(:memory_tracker_opencode_comments, %{})
    |> Map.get(issue_id, [])
    |> List.wrap()
  end

  defp memory_comments(issue_id, context) do
    comments_by_issue =
      project_env(
        :memory_tracker_project_opencode_comments,
        context,
        :memory_tracker_opencode_comments,
        %{}
      )

    comments_by_issue
    |> Map.get(issue_id, [])
    |> List.wrap()
  end

  defp configured_issues do
    Application.get_env(:symphony_elixir, :memory_tracker_issues, [])
  end

  defp configured_issues(nil), do: configured_issues()

  defp configured_issues(context) do
    project_env(:memory_tracker_project_issues, context, :memory_tracker_issues, [])
  end

  defp issue_entries(context \\ nil) do
    context
    |> configured_issues()
    |> Enum.filter(&match?(%Issue{}, &1))
  end

  defp send_event(message), do: send_event(message, nil)

  defp send_event(message, nil) do
    case Application.get_env(:symphony_elixir, :memory_tracker_recipient) do
      pid when is_pid(pid) -> send(pid, message)
      _ -> :ok
    end
  end

  defp send_event({event, issue_id, payload}, context) do
    case {Application.get_env(:symphony_elixir, :memory_tracker_recipient), project_context_id(context)} do
      {pid, project_id} when is_pid(pid) and is_binary(project_id) -> send(pid, {event, project_id, issue_id, payload})
      {pid, _project_id} when is_pid(pid) -> send(pid, {event, issue_id, payload})
      _recipient -> :ok
    end
  end

  defp project_env(project_key, context, fallback_key, default) do
    :symphony_elixir
    |> Application.get_env(project_key, %{})
    |> project_map_value(project_context_id(context))
    |> case do
      :missing -> Application.get_env(:symphony_elixir, fallback_key, default)
      value -> value
    end
  end

  defp project_map_value(map, project_id) when is_map(map) and is_binary(project_id) do
    atom_key = existing_atom_key(project_id)

    cond do
      Map.has_key?(map, project_id) -> Map.fetch!(map, project_id)
      not is_nil(atom_key) and Map.has_key?(map, atom_key) -> Map.fetch!(map, atom_key)
      true -> :missing
    end
  end

  defp project_map_value(_map, _project_id), do: :missing

  defp existing_atom_key(project_id) do
    String.to_existing_atom(project_id)
  rescue
    ArgumentError -> nil
  end

  defp project_context_id(%{project_id: project_id}) when is_binary(project_id), do: project_id
  defp project_context_id(%{id: id}) when is_binary(id), do: id
  defp project_context_id(%{"project_id" => project_id}) when is_binary(project_id), do: project_id
  defp project_context_id(%{"id" => id}) when is_binary(id), do: id
  defp project_context_id(project_id) when is_binary(project_id), do: project_id
  defp project_context_id(_context), do: nil

  defp normalize_state(state) when is_binary(state) do
    state
    |> String.trim()
    |> String.downcase()
  end

  defp normalize_state(_state), do: ""
end
