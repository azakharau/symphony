defmodule SymphonyElixir.Orchestrator.OwnerInputPulse do
  @moduledoc false

  alias SymphonyElixir.Linear.Issue

  @machine_generated_comment_markers [
    "<!-- symphony:",
    "## opencode handoff",
    "## opencode session attached",
    "## symphony stop rule",
    "## benchmark",
    "## validation",
    "## changed files",
    "```text\nstatus:",
    "symphony stop rule",
    "opencode handoff",
    "opencode session attached",
    "changed files",
    "validation results",
    "owner-review update",
    "status after codex continuation",
    "owner decision needed",
    "validation/context checked",
    "is not executable from this packet",
    "no repo files were changed",
    "need owner input stays parked"
  ]

  @spec activity_sort_key(Issue.t()) :: integer()
  def activity_sort_key(%Issue{} = issue) do
    issue
    |> activity_at()
    |> datetime_sort_key()
  end

  @spec fingerprint(Issue.t()) :: String.t()
  def fingerprint(%Issue{id: id} = issue) when is_binary(id) do
    case activity_at(issue) do
      %DateTime{} = activity_at -> id <> ":" <> DateTime.to_iso8601(activity_at)
      _ -> id <> ":unknown"
    end
  end

  @spec activity_at(Issue.t()) :: DateTime.t() | nil
  def activity_at(%Issue{comments: comments}) when is_list(comments) do
    comments
    |> Enum.sort_by(&comment_activity_sort_key/1)
    |> List.last()
    |> case do
      %{created_at: %DateTime{} = created_at} = comment ->
        if owner_answer_comment?(comment), do: created_at

      _ ->
        nil
    end
  end

  def activity_at(%Issue{}), do: nil

  defp owner_answer_comment?(%{body: body, parent_id: parent_id})
       when is_binary(body) and is_binary(parent_id) and parent_id != "" do
    owner_answer_body?(body)
  end

  defp owner_answer_comment?(%{body: body, parent_id: parent_id})
       when is_binary(body) and (is_nil(parent_id) or parent_id == "") do
    owner_answer_body?(body)
  end

  defp owner_answer_comment?(_comment), do: false

  defp owner_answer_body?(body) when is_binary(body) do
    normalized =
      body
      |> String.trim()
      |> String.downcase()

    normalized != "" and
      not machine_generated_comment?(normalized) and
      not long_question_comment?(normalized)
  end

  defp machine_generated_comment?(body) when is_binary(body) do
    Enum.any?(@machine_generated_comment_markers, &String.contains?(body, &1))
  end

  defp long_question_comment?(body) when is_binary(body) do
    String.length(body) > 80 and String.contains?(body, "?")
  end

  defp comment_activity_sort_key(%{created_at: %DateTime{} = created_at}), do: DateTime.to_unix(created_at, :microsecond)
  defp comment_activity_sort_key(_comment), do: 0

  defp datetime_sort_key(%DateTime{} = datetime), do: DateTime.to_unix(datetime, :microsecond)
  defp datetime_sort_key(_datetime), do: 0
end
