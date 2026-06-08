defmodule SymphonyElixir.Runner.Outcome do
  @moduledoc """
  Shared runner completion contract used by adapters and the orchestrator.

  Adapters own runner-specific policy/process mechanics. The orchestrator only
  needs the normalized outcome kind to decide whether to continue, retry,
  reroute, or hold a durable policy block.
  """

  @enforce_keys [:kind]
  defstruct [:kind, :reason, :detail, :result_state, failure: %{}]

  @type kind :: :completed | :rerouted | :policy_blocked
  @type t :: %__MODULE__{
          kind: kind(),
          reason: atom() | nil,
          detail: atom() | String.t() | nil,
          result_state: String.t() | nil,
          failure: map()
        }

  @spec completed(keyword()) :: t()
  def completed(opts \\ []), do: build(:completed, opts)

  @spec rerouted(keyword()) :: t()
  def rerouted(opts \\ []), do: build(:rerouted, opts)

  @spec policy_blocked(keyword()) :: t()
  def policy_blocked(opts \\ []), do: build(:policy_blocked, opts)

  defp build(kind, opts) do
    reason = Keyword.get(opts, :reason)
    detail = Keyword.get(opts, :detail)

    failure =
      opts
      |> Keyword.get(:failure, %{})
      |> Map.new()
      |> maybe_put(:reason, reason)
      |> maybe_put(:detail, detail)

    %__MODULE__{
      kind: kind,
      reason: reason,
      detail: detail,
      result_state: Keyword.get(opts, :result_state),
      failure: failure
    }
  end

  defp maybe_put(map, _key, nil), do: map
  defp maybe_put(map, key, value), do: Map.put(map, key, value)
end
