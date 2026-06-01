defmodule SymphonyElixir.ProjectSupervisorTest do
  use ExUnit.Case

  alias SymphonyElixir.{Orchestrator, ProjectRegistry, RootConfigStore, WorkflowStore}

  setup do
    root = Path.join(System.tmp_dir!(), "symphony-project-supervisor-#{System.unique_integer([:positive])}")
    File.mkdir_p!(root)

    start_supervised!(SymphonyElixir.ProjectRegistry)

    dynamic_supervisor_spec = {
      DynamicSupervisor,
      name: SymphonyElixir.ProjectSupervisor.DynamicSupervisor, strategy: :one_for_one
    }

    start_supervised!(dynamic_supervisor_spec)

    on_exit(fn -> File.rm_rf(root) end)

    %{root: root, config_path: Path.join(root, "projects.yml")}
  end

  test "root reload starts a newly enabled project", %{root: root, config_path: config_path} do
    workflow_a = write_project_workflow!(root, "alpha")
    workflow_b = write_project_workflow!(root, "beta")

    write_projects_config!(config_path, [
      %{id: "alpha", enabled: true, workflow_path: workflow_a},
      %{id: "beta", enabled: false, workflow_path: workflow_b}
    ])

    start_supervised!({RootConfigStore, path: config_path})
    assert project_supervisor_pid("alpha")
    assert_paused_project_orchestrator("alpha")
    refute project_supervisor_pid("beta")
    refute project_orchestrator_pid("beta")

    write_projects_config!(config_path, [
      %{id: "alpha", enabled: true, workflow_path: workflow_a},
      %{id: "beta", enabled: true, workflow_path: workflow_b}
    ])

    assert {:ok, _config} = RootConfigStore.reload()
    assert project_supervisor_pid("alpha")
    assert_paused_project_orchestrator("alpha")
    assert project_supervisor_pid("beta")
    assert_paused_project_orchestrator("beta")
  end

  test "root reload stops a disabled project without stopping other projects", %{root: root, config_path: config_path} do
    workflow_a = write_project_workflow!(root, "alpha")
    workflow_b = write_project_workflow!(root, "beta")

    write_projects_config!(config_path, [
      %{id: "alpha", enabled: true, workflow_path: workflow_a},
      %{id: "beta", enabled: true, workflow_path: workflow_b}
    ])

    start_supervised!({RootConfigStore, path: config_path})
    alpha_pid = project_supervisor_pid("alpha")
    alpha_orchestrator_pid = assert_paused_project_orchestrator("alpha")
    assert project_supervisor_pid("beta")
    beta_orchestrator_pid = assert_paused_project_orchestrator("beta")

    write_projects_config!(config_path, [
      %{id: "alpha", enabled: true, workflow_path: workflow_a},
      %{id: "beta", enabled: false, workflow_path: workflow_b}
    ])

    assert {:ok, _config} = RootConfigStore.reload()
    assert project_supervisor_pid("alpha") == alpha_pid
    assert project_orchestrator_pid("alpha") == alpha_orchestrator_pid
    refute project_supervisor_pid("beta")
    refute project_orchestrator_pid("beta")
    refute Process.alive?(beta_orchestrator_pid)
  end

  test "root supervisor survives one project child crash", %{root: root, config_path: config_path} do
    workflow_a = write_project_workflow!(root, "alpha")
    workflow_b = write_project_workflow!(root, "beta")

    write_projects_config!(config_path, [
      %{id: "alpha", enabled: true, workflow_path: workflow_a},
      %{id: "beta", enabled: true, workflow_path: workflow_b}
    ])

    start_supervised!({RootConfigStore, path: config_path})
    alpha_pid = project_supervisor_pid("alpha")
    alpha_store_pid = ProjectRegistry.whereis({:symphony_project, "alpha", :workflow_store})
    beta_pid = project_supervisor_pid("beta")
    assert_paused_project_orchestrator("alpha")
    assert_paused_project_orchestrator("beta")

    Process.exit(alpha_store_pid, :kill)

    assert eventually(fn ->
             new_alpha_store_pid = ProjectRegistry.whereis({:symphony_project, "alpha", :workflow_store})
             is_pid(new_alpha_store_pid) and new_alpha_store_pid != alpha_store_pid
           end)

    assert Process.alive?(alpha_pid)
    assert Process.alive?(beta_pid)
    assert project_supervisor_pid("beta") == beta_pid
  end

  test "invalid workflow projects become errored project states without crashing root", %{
    root: root,
    config_path: config_path
  } do
    valid_workflow = write_project_workflow!(root, "valid")
    invalid_workflow = Path.join([root, "invalid", "WORKFLOW.md"])
    File.mkdir_p!(Path.dirname(invalid_workflow))
    File.write!(invalid_workflow, "---\nrunner:\n  default: bad-runner\n---\ninvalid")

    write_projects_config!(config_path, [
      %{id: "valid", enabled: true, workflow_path: valid_workflow},
      %{id: "invalid", enabled: true, workflow_path: invalid_workflow}
    ])

    start_supervised!({RootConfigStore, path: config_path})

    assert project_supervisor_pid("valid")
    assert_paused_project_orchestrator("valid")
    refute project_supervisor_pid("invalid")
    refute project_orchestrator_pid("invalid")
    assert %{status: :error, error: {:invalid_project, [_ | _]}} = RootConfigStore.project_states()["invalid"]
  end

  test "root config store reports initial load and reload errors", %{root: root, config_path: config_path} do
    missing_path = Path.join(root, "missing-projects.yml")

    previous_trap_exit = Process.flag(:trap_exit, true)

    assert {:error, {:missing_root_config_file, ^missing_path, :enoent}} =
             RootConfigStore.start_link(path: missing_path)

    Process.flag(:trap_exit, previous_trap_exit)

    workflow = write_project_workflow!(root, "alpha")
    write_projects_config!(config_path, [%{id: "alpha", enabled: true, workflow_path: workflow}])

    start_supervised!({RootConfigStore, path: config_path})
    File.write!(config_path, "projects: [\n")

    assert {:error, {:root_config_parse_error, _reason}} = RootConfigStore.reload()
    assert %{status: :running} = RootConfigStore.project_states()["alpha"]
  end

  test "root config store marks missing workflow projects as errors", %{root: root, config_path: config_path} do
    missing_workflow = Path.join(root, "missing/WORKFLOW.md")

    write_projects_config!(config_path, [
      %{id: "missing", enabled: true, workflow_path: missing_workflow}
    ])

    start_supervised!({RootConfigStore, path: config_path})

    assert %{status: :error, error: {:missing_workflow_file, ^missing_workflow}, pid: nil} =
             RootConfigStore.project_states()["missing"]
  end

  test "root config store can start from configured application path", %{root: root, config_path: config_path} do
    workflow = write_project_workflow!(root, "alpha")
    write_projects_config!(config_path, [%{id: "alpha", enabled: true, workflow_path: workflow}])

    Application.put_env(:symphony_elixir, :root_config_path, config_path)

    assert {:ok, pid} = RootConfigStore.start_link()
    assert %{status: :running} = RootConfigStore.project_states()["alpha"]

    GenServer.stop(pid)
  after
    Application.delete_env(:symphony_elixir, :root_config_path)
  end

  test "root config store records project supervisor start errors", %{root: root, config_path: config_path} do
    workflow = write_project_workflow!(root, "alpha")
    write_projects_config!(config_path, [%{id: "alpha", enabled: true, workflow_path: workflow}])

    {:ok, conflict_pid} =
      Agent.start_link(fn -> :conflict end,
        name: ProjectRegistry.via_name({:symphony_project, "alpha", :orchestrator})
      )

    start_supervised!({RootConfigStore, path: config_path})

    assert %{status: :error, error: {:shutdown, {:failed_to_start_child, Orchestrator, _reason}}, pid: nil} =
             RootConfigStore.project_states()["alpha"]

    Agent.stop(conflict_pid)
  end

  test "project workflow store uses its configured workflow path", %{root: root, config_path: config_path} do
    workflow_a = write_project_workflow!(root, "alpha", "alpha prompt")
    workflow_b = write_project_workflow!(root, "beta", "beta prompt")

    write_projects_config!(config_path, [
      %{id: "alpha", enabled: true, workflow_path: workflow_a},
      %{id: "beta", enabled: true, workflow_path: workflow_b}
    ])

    start_supervised!({RootConfigStore, path: config_path})

    assert {:ok, %{prompt: "alpha prompt"}} =
             WorkflowStore.current(ProjectRegistry.via_name({:symphony_project, "alpha", :workflow_store}))

    assert {:ok, %{prompt: "beta prompt"}} =
             WorkflowStore.current(ProjectRegistry.via_name({:symphony_project, "beta", :workflow_store}))
  end

  test "root reload restarts only projects with changed context", %{root: root, config_path: config_path} do
    workflow_a = write_project_workflow!(root, "alpha", "alpha original")
    workflow_a_changed = write_project_workflow!(root, "alpha-changed", "alpha changed")
    workflow_b = write_project_workflow!(root, "beta", "beta stable")

    write_projects_config!(config_path, [
      %{id: "alpha", enabled: true, workflow_path: workflow_a},
      %{id: "beta", enabled: true, workflow_path: workflow_b}
    ])

    start_supervised!({RootConfigStore, path: config_path})
    alpha_pid = project_supervisor_pid("alpha")
    alpha_orchestrator_pid = assert_paused_project_orchestrator("alpha")
    beta_pid = project_supervisor_pid("beta")
    beta_orchestrator_pid = assert_paused_project_orchestrator("beta")

    assert {:ok, %{prompt: "alpha original"}} =
             WorkflowStore.current(ProjectRegistry.via_name({:symphony_project, "alpha", :workflow_store}))

    write_projects_config!(config_path, [
      %{id: "alpha", enabled: true, workflow_path: workflow_a_changed},
      %{id: "beta", enabled: true, workflow_path: workflow_b}
    ])

    assert {:ok, _config} = RootConfigStore.reload()

    assert project_supervisor_pid("alpha") != alpha_pid
    assert project_orchestrator_pid("alpha") != alpha_orchestrator_pid
    assert_paused_project_orchestrator("alpha")
    assert project_supervisor_pid("beta") == beta_pid
    assert project_orchestrator_pid("beta") == beta_orchestrator_pid

    assert {:ok, %{prompt: "alpha changed"}} =
             WorkflowStore.current(ProjectRegistry.via_name({:symphony_project, "alpha", :workflow_store}))

    assert {:ok, %{prompt: "beta stable"}} =
             WorkflowStore.current(ProjectRegistry.via_name({:symphony_project, "beta", :workflow_store}))
  end

  test "project supervisor starts a paused project orchestrator", %{root: root, config_path: config_path} do
    workflow_a = write_project_workflow!(root, "alpha")

    write_projects_config!(config_path, [
      %{id: "alpha", enabled: true, workflow_path: workflow_a}
    ])

    start_supervised!({RootConfigStore, path: config_path})

    assert_paused_project_orchestrator("alpha")
  end

  test "paused orchestrator init does not read cwd WORKFLOW.md", %{root: root} do
    cwd = File.cwd!()
    empty_root = Path.join(root, "empty-cwd")
    File.mkdir_p!(empty_root)

    try do
      File.cd!(empty_root)
      assert {:ok, %Orchestrator.State{} = state} = Orchestrator.init(dispatch_paused?: true)
      assert state.dispatch_paused? == true
      assert state.tick_timer_ref == nil
      assert state.next_poll_due_at_ms == nil
    after
      File.cd!(cwd)
    end
  end

  test "default orchestrator startup remains unpaused and schedules polling" do
    assert {:ok, %Orchestrator.State{} = state} = Orchestrator.init([])

    assert state.dispatch_paused? == false
    assert is_reference(state.tick_timer_ref)
    assert is_reference(state.tick_token)
    assert is_integer(state.next_poll_due_at_ms)

    Process.cancel_timer(state.tick_timer_ref)
    flush_tick_messages()
  end

  defp project_supervisor_pid(project_id), do: ProjectRegistry.whereis({:project_supervisor, project_id})

  defp project_orchestrator_pid(project_id), do: ProjectRegistry.whereis({:symphony_project, project_id, :orchestrator})

  defp assert_paused_project_orchestrator(project_id) do
    assert orchestrator_pid = project_orchestrator_pid(project_id)

    assert %Orchestrator.State{
             dispatch_paused?: true,
             next_poll_due_at_ms: nil,
             poll_check_in_progress: false,
             tick_timer_ref: nil,
             tick_token: nil
           } = :sys.get_state(orchestrator_pid)

    orchestrator_pid
  end

  defp write_project_workflow!(root, project_id, prompt \\ nil) do
    path = Path.join([root, project_id, "WORKFLOW.md"])
    File.mkdir_p!(Path.dirname(path))
    File.write!(path, "---\ntracker:\n  kind: memory\n---\n#{prompt || project_id}")
    path
  end

  defp write_projects_config!(path, projects) do
    lines =
      Enum.map_join(projects, "\n", fn project ->
        """
          - id: #{project.id}
            enabled: #{project.enabled}
            workflow_path: #{project.workflow_path}
        """
      end)

    File.write!(path, "projects:\n#{lines}")
  end

  defp eventually(fun, attempts \\ 20)
  defp eventually(_fun, 0), do: false

  defp eventually(fun, attempts) do
    if fun.() do
      true
    else
      Process.sleep(25)
      eventually(fun, attempts - 1)
    end
  end

  defp flush_tick_messages do
    receive do
      {:tick, _tick_token} -> flush_tick_messages()
      :tick -> flush_tick_messages()
    after
      0 -> :ok
    end
  end
end
