defmodule SymphonyElixir.RootConfigStore do
  @moduledoc """
  Loads root project configuration and reconciles enabled project supervisors.
  """

  use GenServer

  alias SymphonyElixir.{ProjectContext, ProjectSupervisor, RootConfig}

  defmodule State do
    @moduledoc false

    defstruct [:path, :config, project_states: %{}]
  end

  @spec start_link(keyword()) :: GenServer.on_start()
  def start_link(opts \\ []) do
    GenServer.start_link(__MODULE__, opts, name: __MODULE__)
  end

  @spec reload() :: {:ok, RootConfig.t()} | {:error, term()}
  def reload, do: GenServer.call(__MODULE__, :reload)

  @spec project_states() :: map()
  def project_states, do: GenServer.call(__MODULE__, :project_states)

  @impl true
  def init(opts) do
    path = Keyword.get(opts, :path) || Application.fetch_env!(:symphony_elixir, :root_config_path)

    case load_and_reconcile(path, %{}) do
      {:ok, config, project_states} -> {:ok, %State{path: path, config: config, project_states: project_states}}
      {:error, reason} -> {:stop, reason}
    end
  end

  @impl true
  def handle_call(:reload, _from, %State{} = state) do
    case load_and_reconcile(state.path, state.project_states) do
      {:ok, config, project_states} ->
        {:reply, {:ok, config}, %{state | config: config, project_states: project_states}}

      {:error, reason} ->
        {:reply, {:error, reason}, state}
    end
  end

  def handle_call(:project_states, _from, %State{} = state) do
    {:reply, state.project_states, state}
  end

  defp load_and_reconcile(path, previous_states) do
    with {:ok, config} <- RootConfig.load(path) do
      project_states = reconcile_projects(config.projects, previous_states)
      {:ok, config, project_states}
    end
  end

  defp reconcile_projects(projects, previous_states) do
    contexts_by_id = Map.new(projects, &{&1.project_id, &1})

    previous_states
    |> Map.keys()
    |> Enum.reject(&Map.has_key?(contexts_by_id, &1))
    |> Enum.each(&stop_project/1)

    Map.new(projects, fn %ProjectContext{} = context ->
      {context.project_id, reconcile_project(context, Map.get(previous_states, context.project_id))}
    end)
  end

  defp reconcile_project(%ProjectContext{status: :invalid, errors: errors} = context, _previous_state) do
    stop_project(context.project_id)
    %{status: :error, context: context, pid: nil, error: {:invalid_project, errors}}
  end

  defp reconcile_project(%ProjectContext{enabled: false} = context, _previous_state) do
    stop_project(context.project_id)
    %{status: :disabled, context: context, pid: nil, error: nil}
  end

  defp reconcile_project(%ProjectContext{} = context, previous_state) do
    case ProjectContext.dispatch_blocker(context) do
      nil ->
        maybe_restart_changed_project(context, previous_state)

      blocker ->
        stop_project(context.project_id)
        %{status: :error, context: context, pid: nil, error: blocker}
    end
  end

  defp maybe_restart_changed_project(%ProjectContext{} = context, %{status: :running, context: context}) do
    start_project(context)
  end

  defp maybe_restart_changed_project(%ProjectContext{} = context, %{status: :running}) do
    stop_project(context.project_id)
    start_project(context)
  end

  defp maybe_restart_changed_project(%ProjectContext{} = context, _previous_state) do
    start_project(context)
  end

  defp start_project(%ProjectContext{} = context) do
    spec = ProjectSupervisor.child_spec(context)

    case DynamicSupervisor.start_child(SymphonyElixir.ProjectSupervisor.DynamicSupervisor, spec) do
      {:ok, pid} -> %{status: :running, context: context, pid: pid, error: nil}
      {:error, {:already_started, pid}} -> %{status: :running, context: context, pid: pid, error: nil}
      {:error, reason} -> %{status: :error, context: context, pid: nil, error: reason}
    end
  end

  defp stop_project(project_id) do
    case SymphonyElixir.ProjectRegistry.whereis({:project_supervisor, project_id}) do
      pid when is_pid(pid) ->
        with :ok <- DynamicSupervisor.terminate_child(SymphonyElixir.ProjectSupervisor.DynamicSupervisor, pid) do
          await_project_stopped(project_id, 50)
        end

      nil ->
        :ok
    end
  end

  defp await_project_stopped(_project_id, 0), do: :ok

  defp await_project_stopped(project_id, attempts_left) do
    case SymphonyElixir.ProjectRegistry.whereis({:project_supervisor, project_id}) do
      nil ->
        :ok

      _pid ->
        Process.sleep(1)
        await_project_stopped(project_id, attempts_left - 1)
    end
  end
end
