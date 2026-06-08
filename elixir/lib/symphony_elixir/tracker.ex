defmodule SymphonyElixir.Tracker do
  @moduledoc """
  Adapter boundary for issue tracker reads and writes.
  """

  alias SymphonyElixir.Config

  @callback fetch_candidate_issues() :: {:ok, [term()]} | {:error, term()}
  @callback fetch_issues_by_states([String.t()]) :: {:ok, [term()]} | {:error, term()}
  @callback fetch_issue_states_by_ids([String.t()]) :: {:ok, [term()]} | {:error, term()}
  @callback fetch_project_milestones() :: {:ok, [term()]} | {:error, term()}
  @callback create_comment(String.t(), String.t()) :: :ok | {:error, term()}
  @callback update_issue_state(String.t(), String.t()) :: :ok | {:error, term()}
  @callback latest_opencode_task_prompt(String.t()) :: {:ok, String.t()} | {:error, term()}
  @callback latest_opencode_task_packet(String.t()) :: {:ok, term()} | {:error, term()}
  @callback review_decisions(String.t()) :: {:ok, [term()]} | {:error, term()}

  @spec fetch_candidate_issues() :: {:ok, [term()]} | {:error, term()}
  def fetch_candidate_issues do
    adapter().fetch_candidate_issues()
  end

  @spec fetch_candidate_issues(term()) :: {:ok, [term()]} | {:error, term()}
  def fetch_candidate_issues(context) do
    call_adapter(context, :fetch_candidate_issues, [])
  end

  @spec fetch_issues_by_states([String.t()]) :: {:ok, [term()]} | {:error, term()}
  def fetch_issues_by_states(states) do
    adapter().fetch_issues_by_states(states)
  end

  @spec fetch_issues_by_states([String.t()], term()) :: {:ok, [term()]} | {:error, term()}
  def fetch_issues_by_states(states, context) do
    call_adapter(context, :fetch_issues_by_states, [states])
  end

  @spec fetch_issue_states_by_ids([String.t()]) :: {:ok, [term()]} | {:error, term()}
  def fetch_issue_states_by_ids(issue_ids) do
    adapter().fetch_issue_states_by_ids(issue_ids)
  end

  @spec fetch_issue_states_by_ids([String.t()], term()) :: {:ok, [term()]} | {:error, term()}
  def fetch_issue_states_by_ids(issue_ids, context) do
    call_adapter(context, :fetch_issue_states_by_ids, [issue_ids])
  end

  @spec fetch_project_milestones() :: {:ok, [term()]} | {:error, term()}
  def fetch_project_milestones do
    adapter().fetch_project_milestones()
  end

  @spec fetch_project_milestones(term()) :: {:ok, [term()]} | {:error, term()}
  def fetch_project_milestones(context) do
    call_adapter(context, :fetch_project_milestones, [])
  end

  @spec create_comment(String.t(), String.t()) :: :ok | {:error, term()}
  def create_comment(issue_id, body) do
    adapter().create_comment(issue_id, body)
  end

  @spec create_comment(String.t(), String.t(), term()) :: :ok | {:error, term()}
  def create_comment(issue_id, body, context) do
    call_adapter(context, :create_comment, [issue_id, body])
  end

  @spec update_issue_state(String.t(), String.t()) :: :ok | {:error, term()}
  def update_issue_state(issue_id, state_name) do
    adapter().update_issue_state(issue_id, state_name)
  end

  @spec update_issue_state(String.t(), String.t(), term()) :: :ok | {:error, term()}
  def update_issue_state(issue_id, state_name, context) do
    call_adapter(context, :update_issue_state, [issue_id, state_name])
  end

  @spec latest_opencode_task_prompt(String.t()) :: {:ok, String.t()} | {:error, term()}
  def latest_opencode_task_prompt(issue_id) do
    adapter().latest_opencode_task_prompt(issue_id)
  end

  @spec latest_opencode_task_prompt(String.t(), term()) :: {:ok, String.t()} | {:error, term()}
  def latest_opencode_task_prompt(issue_id, context) do
    call_adapter(context, :latest_opencode_task_prompt, [issue_id])
  end

  @spec latest_opencode_task_packet(String.t()) :: {:ok, term()} | {:error, term()}
  def latest_opencode_task_packet(issue_id) do
    adapter().latest_opencode_task_packet(issue_id)
  end

  @spec latest_opencode_task_packet(String.t(), term()) :: {:ok, term()} | {:error, term()}
  def latest_opencode_task_packet(issue_id, context) do
    call_adapter(context, :latest_opencode_task_packet, [issue_id])
  end

  @spec review_decisions(String.t()) :: {:ok, [term()]} | {:error, term()}
  def review_decisions(issue_id) do
    adapter().review_decisions(issue_id)
  end

  @spec review_decisions(String.t(), term()) :: {:ok, [term()]} | {:error, term()}
  def review_decisions(issue_id, context) do
    call_adapter(context, :review_decisions, [issue_id])
  end

  @spec adapter() :: module()
  def adapter do
    case Config.settings!().tracker.kind do
      "memory" -> SymphonyElixir.Tracker.Memory
      _ -> SymphonyElixir.Linear.Adapter
    end
  end

  @spec adapter(term()) :: module()
  def adapter(nil), do: adapter()

  def adapter(context) do
    case Config.settings!(context).tracker.kind do
      "memory" -> SymphonyElixir.Tracker.Memory
      _ -> SymphonyElixir.Linear.Adapter
    end
  end

  defp call_adapter(nil, function, args), do: apply(adapter(), function, args)

  defp call_adapter(context, function, args) do
    module = adapter(context)

    Code.ensure_loaded(module)

    if function_exported?(module, function, length(args) + 1) do
      apply(module, function, args ++ [context])
    else
      apply(module, function, args)
    end
  end
end
