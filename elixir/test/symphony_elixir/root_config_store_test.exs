defmodule SymphonyElixir.RootConfigStoreTest do
  use ExUnit.Case

  import ExUnit.CaptureLog

  alias SymphonyElixir.{Orchestrator, ProjectRegistry, RootConfigStore}

  setup do
    root = Path.join(System.tmp_dir!(), "symphony-root-config-store-#{System.unique_integer([:positive])}")
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

  test "start_link reports a descriptive initial load error", %{root: root} do
    missing_path = Path.join(root, "missing-projects.yml")
    previous_trap_exit = Process.flag(:trap_exit, true)

    log =
      capture_log(fn ->
        assert {:error, {:missing_root_config_file, expanded_path, :enoent}} =
                 RootConfigStore.start_link(path: missing_path)

        assert expanded_path == Path.expand(missing_path)
      end)

    Process.flag(:trap_exit, previous_trap_exit)

    assert log =~ "Root config store failed to load initial root config"
    assert log =~ missing_path
    assert log =~ "missing_root_config_file"
  end

  test "reload returns an error when the root config file is deleted and keeps old project state", %{
    root: root,
    config_path: config_path
  } do
    workflow = write_project_workflow!(root, "alpha")
    write_projects_config!(config_path, [%{id: "alpha", enabled: true, workflow_path: workflow}])

    start_supervised!({RootConfigStore, path: config_path})
    alpha_pid = project_supervisor_pid("alpha")

    File.rm!(config_path)

    log =
      capture_log(fn ->
        assert {:error, {:missing_root_config_file, expanded_path, :enoent}} = RootConfigStore.reload()
        assert expanded_path == Path.expand(config_path)
      end)

    assert log =~ "Root config store reload failed for"
    assert log =~ "keeping previous project state"
    assert project_supervisor_pid("alpha") == alpha_pid
    assert %{status: :running, pid: ^alpha_pid} = RootConfigStore.project_states()["alpha"]
  end

  test "reload keeps old project state when the new root config is broken", %{
    root: root,
    config_path: config_path
  } do
    workflow = write_project_workflow!(root, "alpha")
    write_projects_config!(config_path, [%{id: "alpha", enabled: true, workflow_path: workflow}])

    start_supervised!({RootConfigStore, path: config_path})
    alpha_pid = project_supervisor_pid("alpha")

    File.write!(config_path, "projects: [\n")

    assert {:error, {:root_config_parse_error, _reason}} = RootConfigStore.reload()
    assert project_supervisor_pid("alpha") == alpha_pid
    assert %{status: :running, pid: ^alpha_pid} = RootConfigStore.project_states()["alpha"]
  end

  test "reload transitions a project from error to running when config is fixed", %{
    root: root,
    config_path: config_path
  } do
    missing_workflow = Path.join(root, "alpha/WORKFLOW.md")
    write_projects_config!(config_path, [%{id: "alpha", enabled: true, workflow_path: missing_workflow}])

    start_supervised!({RootConfigStore, path: config_path})
    assert %{status: :error, error: {:missing_workflow_file, ^missing_workflow}, pid: nil} = project_state("alpha")
    refute project_supervisor_pid("alpha")

    write_project_workflow!(root, "alpha")

    assert {:ok, _config} = RootConfigStore.reload()
    assert %{status: :running, pid: pid, error: nil} = project_state("alpha")
    assert is_pid(pid)
    assert project_supervisor_pid("alpha") == pid
    assert_paused_project_orchestrator("alpha")
  end

  test "reload transitions a project from running to error when config becomes broken", %{
    root: root,
    config_path: config_path
  } do
    workflow = write_project_workflow!(root, "alpha")
    write_projects_config!(config_path, [%{id: "alpha", enabled: true, workflow_path: workflow}])

    start_supervised!({RootConfigStore, path: config_path})
    old_pid = project_supervisor_pid("alpha")

    File.write!(workflow, "---\nrunner:\n  default: bad-runner\n---\ninvalid")

    assert {:ok, _config} = RootConfigStore.reload()

    assert %{status: :error, error: {:invalid_project, [_ | _]}, pid: nil} = project_state("alpha")
    refute project_supervisor_pid("alpha")
    refute Process.alive?(old_pid)
  end

  test "reload gracefully handles a project going from invalid to valid", %{
    root: root,
    config_path: config_path
  } do
    workflow = write_project_workflow!(root, "alpha")

    write_projects_config!(config_path, [
      %{id: "alpha", enabled: "yes", workflow_path: workflow}
    ])

    start_supervised!({RootConfigStore, path: config_path})
    assert %{status: :error, error: {:invalid_project, [_ | _]}, pid: nil} = project_state("alpha")

    write_projects_config!(config_path, [
      %{id: "alpha", enabled: true, workflow_path: workflow}
    ])

    assert {:ok, _config} = RootConfigStore.reload()
    assert %{status: :running, pid: pid, error: nil} = project_state("alpha")
    assert is_pid(pid)
  end

  test "project_states returns the expected state map", %{root: root, config_path: config_path} do
    alpha_workflow = write_project_workflow!(root, "alpha")
    beta_workflow = write_project_workflow!(root, "beta")
    missing_workflow = Path.join(root, "gamma/WORKFLOW.md")

    write_projects_config!(config_path, [
      %{id: "alpha", enabled: true, workflow_path: alpha_workflow},
      %{id: "beta", enabled: false, workflow_path: beta_workflow},
      %{id: "gamma", enabled: true, workflow_path: missing_workflow}
    ])

    start_supervised!({RootConfigStore, path: config_path})

    assert %{
             "alpha" => %{status: :running, context: %{project_id: "alpha"}, pid: alpha_pid, error: nil},
             "beta" => %{status: :disabled, context: %{project_id: "beta"}, pid: nil, error: nil},
             "gamma" => %{
               status: :error,
               context: %{project_id: "gamma"},
               pid: nil,
               error: {:missing_workflow_file, ^missing_workflow}
             }
           } = RootConfigStore.project_states()

    assert is_pid(alpha_pid)
  end

  test "start_link uses the configured application root config path", %{root: root, config_path: config_path} do
    workflow = write_project_workflow!(root, "alpha")
    write_projects_config!(config_path, [%{id: "alpha", enabled: true, workflow_path: workflow}])

    Application.put_env(:symphony_elixir, :root_config_path, config_path)

    assert {:ok, pid} = RootConfigStore.start_link()
    assert %{status: :running} = RootConfigStore.project_states()["alpha"]

    GenServer.stop(pid)
  after
    Application.delete_env(:symphony_elixir, :root_config_path)
  end

  test "start_project logs unexpected project supervisor start errors", %{
    root: root,
    config_path: config_path
  } do
    workflow = write_project_workflow!(root, "alpha")
    write_projects_config!(config_path, [%{id: "alpha", enabled: true, workflow_path: workflow}])

    {:ok, conflict_pid} =
      Agent.start_link(fn -> :conflict end,
        name: ProjectRegistry.via_name({:symphony_project, "alpha", :orchestrator})
      )

    log =
      capture_log(fn ->
        start_supervised!({RootConfigStore, path: config_path})
      end)

    assert log =~ "Root config store failed to start project supervisor"
    assert log =~ "alpha"
    assert log =~ "failed_to_start_child"

    assert %{status: :error, error: {:shutdown, {:failed_to_start_child, Orchestrator, _reason}}, pid: nil} =
             project_state("alpha")

    Agent.stop(conflict_pid)
  end

  defp project_state(project_id), do: RootConfigStore.project_states()[project_id]

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
end
