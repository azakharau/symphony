defmodule SymphonyElixir.ProjectSupervisorTest do
  use ExUnit.Case

  alias SymphonyElixir.{
    Config,
    Orchestrator,
    ProjectRegistry,
    PromptBuilder,
    RootConfigStore,
    Tracker,
    WorkflowStore,
    Workspace
  }

  alias SymphonyElixir.Linear.Issue

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

  test "enabled projects keep workflow config, prompts, workspaces, runners, and orchestration local", %{
    root: root,
    config_path: config_path
  } do
    alpha_workspace_root = Path.join(root, "alpha-workspaces")
    beta_workspace_root = Path.join(root, "beta-workspaces")

    alpha_workflow =
      write_project_runtime_workflow!(root, "alpha",
        tracker_project_slug: "alpha-slug",
        workspace_root: alpha_workspace_root,
        max_concurrent_agents: 1,
        runner_default: "opencode",
        codex_project_root: Path.join(root, "alpha-codex"),
        opencode_project_root: Path.join(root, "alpha-opencode"),
        prompt: "alpha prompt {{ issue.identifier }}"
      )

    beta_workflow =
      write_project_runtime_workflow!(root, "beta",
        tracker_project_slug: "beta-slug",
        workspace_root: beta_workspace_root,
        max_concurrent_agents: 2,
        runner_default: "codex",
        codex_project_root: Path.join(root, "beta-codex"),
        opencode_project_root: Path.join(root, "beta-opencode"),
        prompt: "beta prompt {{ issue.identifier }}"
      )

    write_projects_config!(config_path, [
      %{id: "alpha", enabled: true, workflow_path: alpha_workflow},
      %{id: "beta", enabled: true, workflow_path: beta_workflow}
    ])

    start_supervised!({RootConfigStore, path: config_path})

    %{alpha: alpha_context, beta: beta_context} = project_contexts_by_id()

    assert_paused_project_orchestrator("alpha")
    assert_paused_project_orchestrator("beta")
    alpha_state = paused_project_orchestrator_state("alpha")
    beta_state = paused_project_orchestrator_state("beta")

    assert alpha_state.project_context.project_id == "alpha"
    assert beta_state.project_context.project_id == "beta"
    assert alpha_state.task_supervisor == ProjectRegistry.via_name(alpha_context.process_names.task_supervisor)
    assert beta_state.task_supervisor == ProjectRegistry.via_name(beta_context.process_names.task_supervisor)
    assert alpha_state.max_concurrent_agents == 0
    assert beta_state.max_concurrent_agents == 0

    alpha_settings = Config.settings!(alpha_context)
    beta_settings = Config.settings!(beta_context)

    assert alpha_settings.tracker.project_slug == "alpha-slug"
    assert beta_settings.tracker.project_slug == "beta-slug"
    assert alpha_settings.workspace.root == alpha_workspace_root
    assert beta_settings.workspace.root == beta_workspace_root
    assert alpha_settings.runner.default == "opencode"
    assert beta_settings.runner.default == "codex"
    assert alpha_settings.agent.max_concurrent_agents == 1
    assert beta_settings.agent.max_concurrent_agents == 2
    assert alpha_settings.codex.project_root == Path.join(root, "alpha-codex")
    assert beta_settings.codex.project_root == Path.join(root, "beta-codex")
    assert alpha_settings.opencode.project_root == Path.join(root, "alpha-opencode")
    assert beta_settings.opencode.project_root == Path.join(root, "beta-opencode")
    assert Tracker.adapter(alpha_context) == SymphonyElixir.Tracker.Memory
    assert Tracker.adapter(beta_context) == SymphonyElixir.Tracker.Memory

    issue = %Issue{id: "issue-1", identifier: "SYM-4", title: "Runtime locality", state: "Todo"}

    assert PromptBuilder.build_prompt(issue, project_context: alpha_context) == "alpha prompt SYM-4"
    assert PromptBuilder.build_prompt(issue, project_context: beta_context) == "beta prompt SYM-4"

    assert {:ok, alpha_workspace} = Workspace.create_for_issue(issue, nil, alpha_settings)
    assert {:ok, beta_workspace} = Workspace.create_for_issue(issue, nil, beta_settings)

    assert String.starts_with?(alpha_workspace, alpha_workspace_root)
    assert String.starts_with?(beta_workspace, beta_workspace_root)
    refute String.starts_with?(alpha_workspace, beta_workspace_root)
    refute String.starts_with?(beta_workspace, alpha_workspace_root)
  end

  test "enabled projects keep memory tracker reads and writes local", %{
    root: root,
    config_path: config_path
  } do
    alpha_workflow =
      write_project_runtime_workflow!(root, "alpha",
        tracker_project_slug: "alpha-slug",
        workspace_root: Path.join(root, "alpha-workspaces"),
        max_concurrent_agents: 1,
        runner_default: "codex",
        codex_project_root: Path.join(root, "alpha-codex"),
        opencode_project_root: Path.join(root, "alpha-opencode"),
        prompt: "alpha prompt {{ issue.identifier }}"
      )

    beta_workflow =
      write_project_runtime_workflow!(root, "beta",
        tracker_project_slug: "beta-slug",
        workspace_root: Path.join(root, "beta-workspaces"),
        max_concurrent_agents: 1,
        runner_default: "codex",
        codex_project_root: Path.join(root, "beta-codex"),
        opencode_project_root: Path.join(root, "beta-opencode"),
        prompt: "beta prompt {{ issue.identifier }}"
      )

    write_projects_config!(config_path, [
      %{id: "alpha", enabled: true, workflow_path: alpha_workflow},
      %{id: "beta", enabled: true, workflow_path: beta_workflow}
    ])

    start_supervised!({RootConfigStore, path: config_path})

    %{alpha: alpha_context, beta: beta_context} = project_contexts_by_id()

    assert alpha_context.project_id == "alpha"
    assert beta_context.project_id == "beta"

    alpha_issue = %Issue{id: "issue-alpha", identifier: "ALPHA-1", title: "Alpha work", state: "Todo"}
    beta_issue = %Issue{id: "issue-beta", identifier: "BETA-1", title: "Beta work", state: "Todo"}

    Application.put_env(:symphony_elixir, :memory_tracker_project_issues, %{
      "alpha" => [alpha_issue],
      "beta" => [beta_issue]
    })

    Application.put_env(:symphony_elixir, :memory_tracker_recipient, self())

    assert {:ok, [%Issue{identifier: "ALPHA-1"}]} = Tracker.fetch_issues_by_states(["Todo"], alpha_context)
    assert {:ok, [%Issue{identifier: "BETA-1"}]} = Tracker.fetch_issues_by_states(["Todo"], beta_context)
    assert {:ok, [%Issue{identifier: "ALPHA-1"}]} = Tracker.fetch_issue_states_by_ids(["issue-alpha"], alpha_context)
    assert {:ok, []} = Tracker.fetch_issue_states_by_ids(["issue-alpha"], beta_context)

    assert :ok = Tracker.create_comment("issue-alpha", "alpha comment", alpha_context)
    assert :ok = Tracker.update_issue_state("issue-beta", "In Progress", beta_context)

    assert_receive {:memory_tracker_comment, "alpha", "issue-alpha", "alpha comment"}
    assert_receive {:memory_tracker_state_update, "beta", "issue-beta", "In Progress"}
  after
    Application.delete_env(:symphony_elixir, :memory_tracker_project_issues)
    Application.delete_env(:symphony_elixir, :memory_tracker_recipient)
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

  defp paused_project_orchestrator_state(project_id) do
    project_id
    |> project_orchestrator_pid()
    |> :sys.get_state()
  end

  defp project_contexts_by_id do
    RootConfigStore.project_states()
    |> Map.new(fn {project_id, %{context: context}} -> {String.to_atom(project_id), context} end)
  end

  defp write_project_workflow!(root, project_id, prompt \\ nil) do
    path = Path.join([root, project_id, "WORKFLOW.md"])
    File.mkdir_p!(Path.dirname(path))
    File.write!(path, "---\ntracker:\n  kind: memory\n---\n#{prompt || project_id}")
    path
  end

  defp write_project_runtime_workflow!(root, project_id, opts) do
    path = Path.join([root, project_id, "WORKFLOW.md"])
    File.mkdir_p!(Path.dirname(path))

    File.write!(path, """
    ---
    tracker:
      kind: memory
      project_slug: "#{opts[:tracker_project_slug]}"
      active_states: [Todo, In Progress]
      terminal_states: [Closed, Done]
    workspace:
      root: "#{opts[:workspace_root]}"
    agent:
      max_concurrent_agents: #{opts[:max_concurrent_agents]}
      max_turns: 20
      max_retry_backoff_ms: 300000
      max_concurrent_agents_by_state: {}
    runner:
      default: "#{opts[:runner_default]}"
      routes: {"RCA Required": "codex", "In Review": "codex"}
    codex:
      command: "codex app-server"
      project_root: "#{opts[:codex_project_root]}"
      thread_sandbox: "workspace-write"
      turn_timeout_ms: 3600000
      read_timeout_ms: 5000
      stall_timeout_ms: 300000
    opencode:
      command: opencode
      project_root: "#{opts[:opencode_project_root]}"
      agent: "build"
      format: "json"
      result_state: "In Review"
      timeout_ms: 3600000
    process_policy:
      rca_required_state: "RCA Required"
      max_rejections_per_slice: 2
    polling:
      interval_ms: 30000
      full_interval_ms: 60000
      fast_states: [Todo]
    hooks:
      timeout_ms: 60000
    observability:
      dashboard_enabled: true
      refresh_ms: 1000
      render_interval_ms: 16
    ---
    #{opts[:prompt]}
    """)

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
