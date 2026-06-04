defmodule SymphonyElixir.Linear.Adapter do
  @moduledoc """
  Linear-backed tracker adapter.
  """

  @behaviour SymphonyElixir.Tracker

  alias SymphonyElixir.Linear.Client
  alias SymphonyElixir.OpenCode.TaskPrompt
  alias SymphonyElixir.ReviewDecision

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
  query SymphonyLatestOpenCodeTaskPromptComments($issueId: String!, $first: Int!, $after: String) {
    issue(id: $issueId) {
      comments(first: $first, after: $after, orderBy: createdAt) {
        nodes {
          body
          createdAt
        }
        pageInfo {
          hasNextPage
          endCursor
        }
      }
    }
  }
  """

  @review_decisions_comments_query """
  query SymphonyReviewDecisionComments($issueId: String!, $first: Int!, $after: String) {
    issue(id: $issueId) {
      comments(first: $first, after: $after, orderBy: createdAt) {
        nodes {
          body
          createdAt
        }
        pageInfo {
          hasNextPage
          endCursor
        }
      }
    }
  }
  """

  @review_decision_page_size 100
  @max_review_decision_pages 20
  @opencode_task_prompt_page_size 50
  @max_opencode_task_prompt_pages 20

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

  @spec fetch_candidate_issues(term()) :: {:ok, [term()]} | {:error, term()}
  def fetch_candidate_issues(context), do: client_module().fetch_candidate_issues(context)

  @spec fetch_issues_by_states([String.t()]) :: {:ok, [term()]} | {:error, term()}
  def fetch_issues_by_states(states), do: client_module().fetch_issues_by_states(states)

  @spec fetch_issues_by_states([String.t()], term()) :: {:ok, [term()]} | {:error, term()}
  def fetch_issues_by_states(states, context), do: client_module().fetch_issues_by_states(states, context)

  @spec fetch_issue_states_by_ids([String.t()]) :: {:ok, [term()]} | {:error, term()}
  def fetch_issue_states_by_ids(issue_ids), do: client_module().fetch_issue_states_by_ids(issue_ids)

  @spec fetch_issue_states_by_ids([String.t()], term()) :: {:ok, [term()]} | {:error, term()}
  def fetch_issue_states_by_ids(issue_ids, context), do: client_module().fetch_issue_states_by_ids(issue_ids, context)

  @spec fetch_project_milestones() :: {:ok, [term()]} | {:error, term()}
  def fetch_project_milestones, do: client_module().fetch_project_milestones()

  @spec fetch_project_milestones(term()) :: {:ok, [term()]} | {:error, term()}
  def fetch_project_milestones(context), do: client_module().fetch_project_milestones(context)

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

  @spec create_comment(String.t(), String.t(), term()) :: :ok | {:error, term()}
  def create_comment(issue_id, body, context) when is_binary(issue_id) and is_binary(body) do
    with {:ok, response} <-
           client_module().graphql(@create_comment_mutation, %{issueId: issue_id, body: body}, context: context),
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

  @spec update_issue_state(String.t(), String.t(), term()) :: :ok | {:error, term()}
  def update_issue_state(issue_id, state_name, context)
      when is_binary(issue_id) and is_binary(state_name) do
    with {:ok, state_id} <- resolve_state_id(issue_id, state_name, context),
         {:ok, response} <-
           client_module().graphql(@update_state_mutation, %{issueId: issue_id, stateId: state_id}, context: context),
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

  @spec latest_opencode_task_prompt(String.t(), term()) :: {:ok, String.t()} | {:error, term()}
  def latest_opencode_task_prompt(issue_id, context) when is_binary(issue_id) do
    with {:ok, packet} <- latest_opencode_task_packet(issue_id, context) do
      {:ok, packet.prompt}
    end
  end

  @spec latest_opencode_task_packet(String.t()) :: {:ok, TaskPrompt.Packet.t()} | {:error, term()}
  def latest_opencode_task_packet(issue_id) when is_binary(issue_id) do
    case fetch_opencode_task_prompt_comments(issue_id) do
      {:ok, comments} ->
        comments
        |> Enum.sort_by(&Map.get(&1, "createdAt", ""), :desc)
        |> find_latest_opencode_packet()

      {:error, reason} ->
        {:error, reason}
    end
  end

  @spec latest_opencode_task_packet(String.t(), term()) :: {:ok, TaskPrompt.Packet.t()} | {:error, term()}
  def latest_opencode_task_packet(issue_id, context) when is_binary(issue_id) do
    case fetch_opencode_task_prompt_comments(issue_id, context) do
      {:ok, comments} ->
        comments
        |> Enum.sort_by(&Map.get(&1, "createdAt", ""), :desc)
        |> find_latest_opencode_packet()

      {:error, reason} ->
        {:error, reason}
    end
  end

  @spec review_decisions(String.t()) :: {:ok, [ReviewDecision.t()]} | {:error, term()}
  def review_decisions(issue_id) when is_binary(issue_id) do
    case fetch_review_decision_comments(issue_id) do
      {:ok, comments} ->
        bodies = comments |> Enum.sort_by(&Map.get(&1, "createdAt", ""), :desc) |> Enum.map(&Map.get(&1, "body", ""))

        {:ok, ReviewDecision.extract_many(bodies)}

      {:error, reason} ->
        {:error, reason}
    end
  end

  @spec review_decisions(String.t(), term()) :: {:ok, [ReviewDecision.t()]} | {:error, term()}
  def review_decisions(issue_id, context) when is_binary(issue_id) do
    case fetch_review_decision_comments(issue_id, context) do
      {:ok, comments} ->
        bodies = comments |> Enum.sort_by(&Map.get(&1, "createdAt", ""), :desc) |> Enum.map(&Map.get(&1, "body", ""))

        {:ok, ReviewDecision.extract_many(bodies)}

      {:error, reason} ->
        {:error, reason}
    end
  end

  defp find_latest_opencode_packet(comments) when is_list(comments) do
    comments
    |> Enum.find_value(&extract_opencode_packet_from_comment/1)
    |> case do
      {:ok, packet} -> {:ok, packet}
      {:error, reason} -> {:error, reason}
      nil -> {:error, :opencode_task_prompt_not_found}
    end
  end

  defp extract_opencode_packet_from_comment(%{"body" => body}) do
    case TaskPrompt.extract_packet(body) do
      {:ok, packet} -> {:ok, packet}
      {:error, :opencode_task_prompt_not_found} -> nil
      {:error, reason} -> if TaskPrompt.marker_present?(body), do: {:error, reason}, else: nil
    end
  end

  defp extract_opencode_packet_from_comment(_comment), do: nil

  defp fetch_opencode_task_prompt_comments(issue_id) do
    fetch_opencode_task_prompt_comments(issue_id, nil, [], 0)
  end

  defp fetch_opencode_task_prompt_comments(issue_id, context) do
    fetch_opencode_task_prompt_comments(issue_id, nil, [], 0, context)
  end

  defp fetch_opencode_task_prompt_comments(_issue_id, _after, _comments, pages_seen)
       when pages_seen >= @max_opencode_task_prompt_pages do
    {:error, :opencode_task_prompt_comment_page_limit_exceeded}
  end

  defp fetch_opencode_task_prompt_comments(issue_id, after_cursor, comments_acc, pages_seen) do
    variables = %{issueId: issue_id, first: @opencode_task_prompt_page_size, after: after_cursor}

    with {:ok, response} <- client_module().graphql(@latest_comments_query, variables),
         %{} = comments_payload <- get_in(response, ["data", "issue", "comments"]),
         nodes when is_list(nodes) <- Map.get(comments_payload, "nodes"),
         %{} = page_info <- Map.get(comments_payload, "pageInfo", %{}) do
      comments = comments_acc ++ nodes

      case {Map.get(page_info, "hasNextPage"), Map.get(page_info, "endCursor")} do
        {true, cursor} when is_binary(cursor) and cursor != "" ->
          fetch_opencode_task_prompt_comments(issue_id, cursor, comments, pages_seen + 1)

        {true, _cursor} ->
          {:error, :opencode_task_prompt_missing_end_cursor}

        _ ->
          {:ok, comments}
      end
    else
      {:error, reason} -> {:error, reason}
      _ -> {:error, :opencode_task_prompt_not_found}
    end
  end

  defp fetch_opencode_task_prompt_comments(_issue_id, _after, _comments, pages_seen, _context)
       when pages_seen >= @max_opencode_task_prompt_pages do
    {:error, :opencode_task_prompt_comment_page_limit_exceeded}
  end

  defp fetch_opencode_task_prompt_comments(issue_id, after_cursor, comments_acc, pages_seen, context) do
    variables = %{issueId: issue_id, first: @opencode_task_prompt_page_size, after: after_cursor}

    with {:ok, response} <- client_module().graphql(@latest_comments_query, variables, context: context),
         %{} = comments_payload <- get_in(response, ["data", "issue", "comments"]),
         nodes when is_list(nodes) <- Map.get(comments_payload, "nodes"),
         %{} = page_info <- Map.get(comments_payload, "pageInfo", %{}) do
      comments = comments_acc ++ nodes

      case {Map.get(page_info, "hasNextPage"), Map.get(page_info, "endCursor")} do
        {true, cursor} when is_binary(cursor) and cursor != "" ->
          fetch_opencode_task_prompt_comments(issue_id, cursor, comments, pages_seen + 1, context)

        {true, _cursor} ->
          {:error, :opencode_task_prompt_missing_end_cursor}

        _ ->
          {:ok, comments}
      end
    else
      {:error, reason} -> {:error, reason}
      _ -> {:error, :opencode_task_prompt_not_found}
    end
  end

  defp fetch_review_decision_comments(issue_id) do
    fetch_review_decision_comments(issue_id, nil, [], 0)
  end

  defp fetch_review_decision_comments(issue_id, context) do
    fetch_review_decision_comments(issue_id, nil, [], 0, context)
  end

  defp fetch_review_decision_comments(_issue_id, _after, _comments, pages_seen)
       when pages_seen >= @max_review_decision_pages do
    {:error, :review_decisions_comment_page_limit_exceeded}
  end

  defp fetch_review_decision_comments(issue_id, after_cursor, comments_acc, pages_seen) do
    variables = %{issueId: issue_id, first: @review_decision_page_size, after: after_cursor}

    with {:ok, response} <- client_module().graphql(@review_decisions_comments_query, variables),
         %{} = comments_payload <- get_in(response, ["data", "issue", "comments"]),
         nodes when is_list(nodes) <- Map.get(comments_payload, "nodes"),
         %{} = page_info <- Map.get(comments_payload, "pageInfo", %{}) do
      comments = comments_acc ++ nodes

      case {Map.get(page_info, "hasNextPage"), Map.get(page_info, "endCursor")} do
        {true, cursor} when is_binary(cursor) and cursor != "" ->
          fetch_review_decision_comments(issue_id, cursor, comments, pages_seen + 1)

        {true, _cursor} ->
          {:error, :review_decisions_missing_end_cursor}

        _ ->
          {:ok, comments}
      end
    else
      {:error, reason} -> {:error, reason}
      _ -> {:error, :review_decisions_not_found}
    end
  end

  defp fetch_review_decision_comments(_issue_id, _after, _comments, pages_seen, _context)
       when pages_seen >= @max_review_decision_pages do
    {:error, :review_decisions_comment_page_limit_exceeded}
  end

  defp fetch_review_decision_comments(issue_id, after_cursor, comments_acc, pages_seen, context) do
    variables = %{issueId: issue_id, first: @review_decision_page_size, after: after_cursor}

    with {:ok, response} <- client_module().graphql(@review_decisions_comments_query, variables, context: context),
         %{} = comments_payload <- get_in(response, ["data", "issue", "comments"]),
         nodes when is_list(nodes) <- Map.get(comments_payload, "nodes"),
         %{} = page_info <- Map.get(comments_payload, "pageInfo", %{}) do
      comments = comments_acc ++ nodes

      case {Map.get(page_info, "hasNextPage"), Map.get(page_info, "endCursor")} do
        {true, cursor} when is_binary(cursor) and cursor != "" ->
          fetch_review_decision_comments(issue_id, cursor, comments, pages_seen + 1, context)

        {true, _cursor} ->
          {:error, :review_decisions_missing_end_cursor}

        _ ->
          {:ok, comments}
      end
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

  defp resolve_state_id(issue_id, state_name, context) do
    with {:ok, response} <-
           client_module().graphql(@state_lookup_query, %{issueId: issue_id, stateName: state_name}, context: context),
         state_id when is_binary(state_id) <-
           get_in(response, ["data", "issue", "team", "states", "nodes", Access.at(0), "id"]) do
      {:ok, state_id}
    else
      {:error, reason} -> {:error, reason}
      _ -> {:error, :state_not_found}
    end
  end
end
