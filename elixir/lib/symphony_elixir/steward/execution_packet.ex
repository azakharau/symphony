defmodule SymphonyElixir.Steward.ExecutionPacket do
  @moduledoc """
  Builds the durable steward packet recorded before worker dispatch.

  The packet is intentionally plain data so PulseLedger can persist it without
  depending on runner internals or external Mnemesh APIs.
  """

  alias SymphonyElixir.Linear.Issue

  @forbidden_preambles [
    "You are the coding orchestrator",
    "You are the Machine Architect",
    "You are the OpenCode build orchestrator"
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
      Symphony execution packet

      #{Jason.encode!(packet, pretty: true)}
      """
      |> String.trim()

    if forbidden_preamble?(prompt), do: {:error, :forbidden_role_preamble}, else: {:ok, prompt}
  end

  @spec forbidden_preamble?(String.t()) :: boolean()
  def forbidden_preamble?(prompt) when is_binary(prompt) do
    trimmed = String.trim_leading(prompt)

    String.starts_with?(trimmed, "You are ") or
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
