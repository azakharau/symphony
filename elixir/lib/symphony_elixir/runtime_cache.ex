defmodule SymphonyElixir.RuntimeCache do
  @moduledoc """
  Process-local runtime cache for loop suppression data.

  This cache is intentionally non-durable. Linear state, workflow config, and
  issue/runtime structs remain the durable authority.
  """

  use GenServer

  alias SymphonyElixir.{Linear.Issue, ProjectContext}

  @type state :: %{handoff_fingerprints: MapSet.t()}

  @spec start_link(keyword()) :: GenServer.on_start()
  def start_link(opts \\ []) do
    GenServer.start_link(__MODULE__, %{}, name: Keyword.get(opts, :name, __MODULE__))
  end

  @impl true
  def init(_opts) do
    {:ok, %{handoff_fingerprints: MapSet.new()}}
  end

  @spec handoff_fingerprint_seen?(ProjectContext.t() | nil, Issue.t() | String.t(), String.t()) :: boolean()
  def handoff_fingerprint_seen?(project_context, issue_or_id, fingerprint) when is_binary(fingerprint) do
    GenServer.call(server(), {:handoff_fingerprint_seen?, project_key(project_context), issue_id(issue_or_id), fingerprint})
  end

  @spec record_handoff_fingerprint(ProjectContext.t() | nil, Issue.t() | String.t(), String.t()) :: :ok
  def record_handoff_fingerprint(project_context, issue_or_id, fingerprint) when is_binary(fingerprint) do
    GenServer.call(server(), {:record_handoff_fingerprint, project_key(project_context), issue_id(issue_or_id), fingerprint})
  end

  @spec clear_issue(ProjectContext.t() | nil, Issue.t() | String.t()) :: :ok
  def clear_issue(project_context, issue_or_id) do
    GenServer.call(server(), {:clear_issue, project_key(project_context), issue_id(issue_or_id)})
  end

  @impl true
  def handle_call({:handoff_fingerprint_seen?, project_key, issue_id, fingerprint}, _from, state) do
    {:reply, MapSet.member?(state.handoff_fingerprints, {project_key, issue_id, fingerprint}), state}
  end

  def handle_call({:record_handoff_fingerprint, project_key, issue_id, fingerprint}, _from, state) do
    fingerprints = MapSet.put(state.handoff_fingerprints, {project_key, issue_id, fingerprint})
    {:reply, :ok, %{state | handoff_fingerprints: fingerprints}}
  end

  def handle_call({:clear_issue, nil, issue_id}, _from, state) do
    fingerprints =
      MapSet.filter(state.handoff_fingerprints, fn
        {_project_key, ^issue_id, _fingerprint} -> false
        _entry -> true
      end)

    {:reply, :ok, %{state | handoff_fingerprints: fingerprints}}
  end

  def handle_call({:clear_issue, project_key, issue_id}, _from, state) do
    fingerprints =
      MapSet.filter(state.handoff_fingerprints, fn
        {^project_key, ^issue_id, _fingerprint} -> false
        _entry -> true
      end)

    {:reply, :ok, %{state | handoff_fingerprints: fingerprints}}
  end

  defp server do
    case Process.whereis(__MODULE__) do
      nil ->
        case GenServer.start(__MODULE__, %{}, name: __MODULE__) do
          {:ok, _pid} -> __MODULE__
          {:error, {:already_started, _pid}} -> __MODULE__
        end

      _pid ->
        __MODULE__
    end
  end

  defp project_key(%ProjectContext{id: id}) when is_binary(id) and id != "", do: id
  defp project_key(_project_context), do: nil

  defp issue_id(%Issue{id: id}), do: id
  defp issue_id(id) when is_binary(id), do: id
end
