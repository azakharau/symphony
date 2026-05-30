defmodule SymphonyElixir.Linear.Adapter do
  @moduledoc """
  Linear-backed tracker adapter.
  """

  @behaviour SymphonyElixir.Tracker

  alias SymphonyElixir.Linear.Client
  alias SymphonyElixir.ReviewDecision
  alias SymphonyElixir.OpenCode.TaskPrompt

  @create_comment_mutation """
  mutation SymphonyCreateComment($issueId: String!, $body: String!) {
    commentCreate(input: {issueId: $issueId, body: $body}) {
      success
    }
  }
  """

  @update_state_mutation """
  mutation SymphonyUpdateIssueState($issueId: String!, $stateId: String!) {
    issueUpdate(id: $issueId, input: {stateId: $stateId}) {
      success
    }
  }
  """

  @latest_comments_query """
  query SymphonyLatestOpenCodeTaskPromptComments($issueId: String!, $first: Int!) {
    issue(id: $issueId) {
      comments(first: $first) {
        nodes {
          body
          createdAt
        }
      }
    }
  }
  """

  @state_lookup_query """
  query SymphonyResolveStateId($issueId: String!, $stateName: String!) {
    issue(id: $issueId) {
      team {
        states(filter: {name: {eq: $stateName}}, first: 1) {
          nodes {
            id
          }
        }
      }
    }
  }
  """

  @spec fetch_candidate_issues() :: {:ok, [term()]} | {:error, term()}
  def fetch_candidate_issues, do: client_module().fetch_candidate_issues()

  @spec fetch_issues_by_states([String.t()]) :: {:ok, [term()]} | {:error, term()}
  def fetch_issues_by_states(states), do: client_module().fetch_issues_by_states(states)

  @spec fetch_issue_states_by_ids([String.t()]) :: {:ok, [term()]} | {:error, term()}
  def fetch_issue_states_by_ids(issue_ids), do: client_module().fetch_issue_states_by_ids(issue_ids)

  @spec fetch_project_milestones() :: {:ok, [term()]} | {:error, term()}
  def fetch_project_milestones, do: client_module().fetch_project_milestones()

  @spec create_comment(String.t(), String.t()) :: :ok | {:error, term()}
  def create_comment(issue_id, body) when is_binary(issue_id) and is_binary(body) do
    with {:ok, response} <- client_module().graphql(@create_comment_mutation, %{issueId: issue_id, body: body}),
         true <- get_in(response, ["data", "commentCreate", "success"]) == true do
      :ok
    else
      false -> {:error, :comment_create_failed}
      {:error, reason} -> {:error, reason}
      _ -> {:error, :comment_create_failed}
    end
  end

  @spec update_issue_state(String.t(), String.t()) :: :ok | {:error, term()}
  def update_issue_state(issue_id, state_name)
      when is_binary(issue_id) and is_binary(state_name) do
    with {:ok, state_id} <- resolve_state_id(issue_id, state_name),
         {:ok, response} <-
           client_module().graphql(@update_state_mutation, %{issueId: issue_id, stateId: state_id}),
         true <- get_in(response, ["data", "issueUpdate", "success"]) == true do
      :ok
    else
      false -> {:error, :issue_update_failed}
      {:error, reason} -> {:error, reason}
      _ -> {:error, :issue_update_failed}
    end
  end

  @spec latest_opencode_task_prompt(String.t()) :: {:ok, String.t()} | {:error, term()}
  def latest_opencode_task_prompt(issue_id) when is_binary(issue_id) do
    with {:ok, packet} <- latest_opencode_task_packet(issue_id) do
      {:ok, packet.prompt}
    end
  end

  @spec latest_opencode_task_packet(String.t()) :: {:ok, TaskPrompt.Packet.t()} | {:error, term()}
  def latest_opencode_task_packet(issue_id) when is_binary(issue_id) do
    with {:ok, response} <-
           client_module().graphql(@latest_comments_query, %{issueId: issue_id, first: 50}),
         comments when is_list(comments) <-
           get_in(response, ["data", "issue", "comments", "nodes"]) do
      comments
      |> Enum.sort_by(&Map.get(&1, "createdAt", ""), :desc)
      |> Enum.find_value(fn %{"body" => body} ->
        case TaskPrompt.extract_packet(body) do
          {:ok, packet} -> {:ok, packet}
          {:error, _reason} -> nil
        end
      end)
      |> case do
        {:ok, packet} -> {:ok, packet}
        nil -> {:error, :opencode_task_prompt_not_found}
      end
    else
      {:error, reason} -> {:error, reason}
      _ -> {:error, :opencode_task_prompt_not_found}
    end
  end

  @spec review_decisions(String.t()) :: {:ok, [ReviewDecision.t()]} | {:error, term()}
  def review_decisions(issue_id) when is_binary(issue_id) do
    with {:ok, response} <-
           client_module().graphql(@latest_comments_query, %{issueId: issue_id, first: 100}),
         comments when is_list(comments) <-
           get_in(response, ["data", "issue", "comments", "nodes"]) do
      bodies = Enum.map(comments, &Map.get(&1, "body", ""))
      {:ok, ReviewDecision.extract_many(bodies)}
    else
      {:error, reason} -> {:error, reason}
      _ -> {:error, :review_decisions_not_found}
    end
  end

  defp client_module do
    Application.get_env(:symphony_elixir, :linear_client_module, Client)
  end

  defp resolve_state_id(issue_id, state_name) do
    with {:ok, response} <-
           client_module().graphql(@state_lookup_query, %{issueId: issue_id, stateName: state_name}),
         state_id when is_binary(state_id) <-
           get_in(response, ["data", "issue", "team", "states", "nodes", Access.at(0), "id"]) do
      {:ok, state_id}
    else
      {:error, reason} -> {:error, reason}
      _ -> {:error, :state_not_found}
    end
  end
end
