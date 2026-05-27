defmodule SymphonyElixir.ReviewDecision do
  @moduledoc """
  Parses Codex review decision packets from tracker comments.
  """

  defstruct [:status, :slice_id, :reason]

  @type t :: %__MODULE__{status: String.t() | nil, slice_id: String.t() | nil, reason: String.t() | nil}

  @marker "symphony:review-decision:v1"

  @spec extract_many([String.t()] | String.t()) :: [t()]
  def extract_many(bodies) when is_list(bodies) do
    bodies
    |> Enum.flat_map(&extract_from_body/1)
  end

  def extract_many(body) when is_binary(body), do: extract_from_body(body)
  def extract_many(_body), do: []

  defp extract_from_body(body) when is_binary(body) do
    ~r/<!--\s*#{Regex.escape(@marker)}[^>]*-->\s*```(?:text|yaml)?\s*(?<payload>.*?)```/s
    |> Regex.scan(body, capture: :all_names)
    |> Enum.map(fn [payload] -> parse_payload(payload) end)
    |> Enum.reject(&is_nil/1)
  end

  defp extract_from_body(_body), do: []

  defp parse_payload(payload) when is_binary(payload) do
    fields =
      payload
      |> String.split("\n")
      |> Enum.reduce(%{}, fn line, acc ->
        case String.split(line, ":", parts: 2) do
          [key, value] -> Map.put(acc, normalize_key(key), String.trim(value))
          _ -> acc
        end
      end)

    case Map.get(fields, "status") do
      nil -> nil
      status -> %__MODULE__{status: normalize_status(status), slice_id: Map.get(fields, "slice_id"), reason: Map.get(fields, "reason")}
    end
  end

  defp normalize_key(key) do
    key
    |> String.trim()
    |> String.downcase()
  end

  defp normalize_status(status) do
    status
    |> String.trim()
    |> String.downcase()
  end
end
