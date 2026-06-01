defmodule SymphonyElixir.ProjectRegistry do
  @moduledoc """
  Registry name helpers for root-mode project worker processes.
  """

  @spec child_spec(keyword()) :: Supervisor.child_spec()
  def child_spec(_opts) do
    Registry.child_spec(keys: :unique, name: __MODULE__)
  end

  @spec via_name(term()) :: GenServer.name()
  def via_name(key), do: {:via, Registry, {__MODULE__, key}}

  @spec whereis(term()) :: pid() | nil
  def whereis(key) do
    case Registry.lookup(__MODULE__, key) do
      [{pid, _value}] -> pid
      [] -> nil
    end
  end
end
