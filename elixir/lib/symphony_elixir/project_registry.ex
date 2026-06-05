defmodule SymphonyElixir.ProjectRegistry do
  @moduledoc """
  Registry name helpers for root-mode project worker processes.

  Keys may be any term accepted by `Registry`, though project code should prefer
  tuple keys such as `{:project_supervisor, project_id}` or
  `{:symphony_project, project_id, :component_name}` for consistency.
  """

  @doc """
  Returns the child spec for the unique project registry.

  Options are currently ignored because the registry name and key mode are fixed
  for root-mode project worker process naming.
  """
  @spec child_spec(keyword()) :: Supervisor.child_spec()
  def child_spec(_opts) do
    Registry.child_spec(keys: :unique, name: __MODULE__)
  end

  @doc """
  Builds the `:via` tuple used to register or address a process by key.
  """
  @spec via_name(term()) :: GenServer.name()
  def via_name(key), do: {:via, Registry, {__MODULE__, key}}

  @doc """
  Returns `true` when a process is currently registered under `key`.
  """
  @spec registered?(term()) :: boolean()
  def registered?(key), do: is_pid(whereis(key))

  @doc """
  Returns the pid registered under `key`, or `nil` when none is registered.

  Also returns `nil` when the project registry has not been started yet.
  """
  @spec whereis(term()) :: pid() | nil
  def whereis(key) do
    if Process.whereis(__MODULE__) do
      case Registry.lookup(__MODULE__, key) do
        [{pid, _value}] -> pid
        [] -> nil
      end
    end
  rescue
    ArgumentError -> nil
  end
end
