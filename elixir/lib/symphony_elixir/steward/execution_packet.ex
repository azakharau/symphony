defmodule SymphonyElixir.Steward.ExecutionPacket do
  @moduledoc """
  Builds the durable steward packet recorded before worker dispatch.

  The packet is intentionally plain data so it can move across runner boundaries
  without depending on runner internals or external Mnemesh APIs.
  """

  alias SymphonyElixir.Linear.Issue

  @forbidden_preambles [
    "You are the coding orchestrator",
    "You are the Machine Architect"
  ]

  @spec build(Issue.t(), term()) :: map()
  def build(%Issue{} = issue, project_context \\ nil) do
    milestone = issue.project_milestone || %{}

    %{
      "packet_version" => "symphony:execution-packet:v1",
      "created_at" => DateTime.utc_now() |> DateTime.truncate(:second) |> DateTime.to_iso8601(),
      "project" => project_payload(project_context),
      "active_milestone" => %{
        "id" => map_value(milestone, :id, "id"),
        "name" => map_value(milestone, :name, "name")
      },
      "issue" => %{
        "id" => issue.id,
        "identifier" => issue.identifier,
        "title" => issue.title,
        "state" => issue.state,
        "priority" => issue.priority,
        "url" => issue.url
      },
      "source_evidence_refs" => source_evidence_refs(issue),
      "acceptance_criteria" => acceptance_criteria(issue),
      "allowed_paths" => [],
      "validation_gates" => [
        "run the issue-specified targeted tests",
        "run formatter/static checks relevant to touched files",
        "record exact command results in the handoff"
      ],
      "stop_conditions" => [
        "missing durable context or task packet",
        "scope crosses milestone/product policy",
        "required validation cannot run"
      ],
      "handoff_requirements" => [
        "return changed files with line anchors",
        "return exact validation commands and outcomes",
        "name unresolved risks before the issue can close"
      ],
      "documentation_requirement" => "update operator or workflow docs when changed behavior makes existing docs stale"
    }
  end

  @spec prompt(map()) :: {:ok, String.t()} | {:error, :forbidden_role_preamble}
  def prompt(packet) when is_map(packet) do
    prompt =
      """
      Symphony steward packet

      <mission>
      Keep Codex as the architect/reviewer. OpenCode writes application code.
      Be concise, act only on the current Linear state, and stop as soon as the state contract is satisfied.
      </mission>

      <state_contract>
      Todo:
      - This state is the queued work backlog for the active milestone.
      - Do not run Codex stewardship while the issue is still in Todo.

      Preparing:
      - Verify the issue is in the active milestone and is not blocked.
      - If implementation is needed, post exactly one `symphony:opencode-task-prompt:v1` Linear comment.
      - The OpenCode prompt must include objective, scope, allowed paths, forbidden actions, acceptance criteria, validation commands, stop conditions, and handoff requirements.
      - Move the same issue to `In Progress`, then stop.
      - Do not edit repo files, run implementation validation, commit, push, or open a PR.

      In Progress:
      - This state belongs to OpenCode. Do not process it with Codex.

      In Review:
      - Inspect the OpenCode handoff, diff, and validation evidence.
      - Post one `symphony:review-decision:v1` comment.
      - Accept and close only after direct evidence; otherwise reject, request owner input, or route to RCA.

      RCA Required:
      - Identify root cause first.
      - If code repair is needed, post a redesigned OpenCode prompt with a new slice_id and move the issue to `In Progress`.
      - Do not implement the repair in Codex.

      Need Owner Input:
      - Read the latest owner-visible comment.
      - Apply the owner decision if present; otherwise leave the issue parked.
      - Do not edit repo files.
      </state_contract>

      <hard_stops>
      - Never write application code in Codex for `Todo`, `Preparing`, `In Progress`, `RCA Required`, or `Need Owner Input`.
      - Never replace OpenCode implementation with a Codex implementation.
      - Never continue after posting the required handoff or review decision.
      - Ask one concise owner question only when the packet lacks the information needed to choose the next state.
      </hard_stops>

      <packet_json>

      #{Jason.encode!(packet, pretty: true)}
      </packet_json>
      """
      |> String.trim()

    if forbidden_preamble?(prompt), do: {:error, :forbidden_role_preamble}, else: {:ok, prompt}
  end

  @spec forbidden_preamble?(String.t()) :: boolean()
  def forbidden_preamble?(prompt) when is_binary(prompt) do
    trimmed = String.trim_leading(prompt)

    Enum.any?(@forbidden_preambles, &String.starts_with?(trimmed, &1))
  end

  def forbidden_preamble?(_prompt), do: false

  defp project_payload(%{id: id, name: name}) do
    %{"id" => id, "name" => name}
  end

  defp project_payload(%{"id" => id, "name" => name}) do
    %{"id" => id, "name" => name}
  end

  defp project_payload(_project_context), do: %{"id" => nil, "name" => nil}

  defp source_evidence_refs(%Issue{url: url}) when is_binary(url) and url != "", do: [url]
  defp source_evidence_refs(_issue), do: []

  defp acceptance_criteria(%Issue{description: description}) when is_binary(description) and description != "" do
    description
    |> String.split("\n")
    |> Enum.filter(&(String.contains?(String.downcase(&1), "accept") or String.starts_with?(String.trim(&1), "-")))
    |> Enum.take(10)
  end

  defp acceptance_criteria(_issue), do: []

  defp map_value(map, atom_key, string_key) when is_map(map), do: Map.get(map, atom_key) || Map.get(map, string_key)
  defp map_value(_map, _atom_key, _string_key), do: nil
end
