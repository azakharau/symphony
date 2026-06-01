defmodule SymphonyElixir.RootConfigTest do
  use ExUnit.Case, async: true

  alias SymphonyElixir.ProjectContext
  alias SymphonyElixir.RootConfig

  test "loads valid multi-project config with disabled projects visible" do
    root = tmp_root("valid")
    config_path = Path.join(root, "projects.yml")
    workflow_a = Path.join(root, "alpha/WORKFLOW.md")
    workflow_b = Path.join(root, "beta/WORKFLOW.md")
    logs_root = Path.join(root, "logs/beta")

    File.mkdir_p!(Path.dirname(workflow_a))
    File.mkdir_p!(Path.dirname(workflow_b))
    File.write!(workflow_a, "---\ntracker:\n  kind: memory\n---\nalpha")
    File.write!(workflow_b, "---\ntracker:\n  kind: memory\n---\nbeta")

    File.write!(config_path, """
    server:
      host: 0.0.0.0
      port: 4040
    projects:
      - id: alpha-project
        name: Alpha Project
        enabled: true
        status: ignored
        repo_root: ./alpha
        app_root: ./alpha/app
        workflow_path: alpha/WORKFLOW.md
        dashboard_order: 10
        linear:
          team:
            key: ENG
            name: Engineering
          project:
            id: linear-project-id
            slug: alpha-slug
            name: Alpha Linear Project
          milestone:
            id: milestone-id
            name: Milestone One
        mnemesh:
          workspace_id: workspace-1
          task_id: task-1
          subtask_id: subtask-1
          handoff_cursor: cursor-1
        runner:
          owner: opencode
          default: codex
        execution:
          enabled: true
          max_concurrent_runs: 2
        gates:
          dispatch_enabled: true
          requires_review: true
      - id: beta_project
        workflow_path: beta/WORKFLOW.md
        logs_root: logs/beta
    """)

    assert {:ok, %RootConfig{} = config} = RootConfig.load(config_path)
    assert config.server == %{host: "0.0.0.0", port: 4040}
    assert [alpha, beta] = config.projects

    assert %ProjectContext{
             project_id: "alpha-project",
             name: "Alpha Project",
             enabled: true,
             status: :valid,
             repo_root: _,
             app_root: _,
             workflow_path: ^workflow_a,
             dashboard_order: 10,
             logs_root: nil,
             linear: _,
             mnemesh: _,
             runner: _,
             execution: _,
             gates: _,
             errors: []
           } = alpha

    assert alpha.id == "alpha-project"
    assert alpha.repo_root == Path.join(root, "alpha")
    assert alpha.app_root == Path.join(root, "alpha/app")
    assert alpha.linear["team"] == %{"key" => "ENG", "name" => "Engineering"}
    assert alpha.linear["project"] == %{"id" => "linear-project-id", "slug" => "alpha-slug", "name" => "Alpha Linear Project"}
    assert alpha.linear["milestone"] == %{"id" => "milestone-id", "name" => "Milestone One"}
    assert alpha.mnemesh == %{"workspace_id" => "workspace-1", "task_id" => "task-1", "subtask_id" => "subtask-1", "handoff_cursor" => "cursor-1"}
    assert alpha.runner == %{"owner" => "opencode", "default" => "codex"}
    assert alpha.execution == %{"enabled" => true, "max_concurrent_runs" => 2}
    assert alpha.gates == %{"dispatch_enabled" => true, "requires_review" => true}
    assert alpha.process_names.workflow_store == {:symphony_project, "alpha-project", :workflow_store}
    assert ProjectContext.dispatchable?(alpha)

    assert %ProjectContext{
             project_id: "beta_project",
             name: "beta_project",
             enabled: false,
             status: :disabled,
             workflow_path: ^workflow_b,
             dashboard_order: nil,
             logs_root: ^logs_root,
             errors: []
           } = beta

    refute ProjectContext.dispatchable?(beta)
    assert ProjectContext.dispatch_blocker(beta) == :disabled
    assert RootConfig.enabled_projects(config) == [alpha]
    assert RootConfig.project_by_id(config, "beta_project") == beta
  after
    File.rm_rf(tmp_root("valid"))
  end

  test "rejects duplicate project ids" do
    assert {:error, {:invalid_root_config, message}} =
             RootConfig.parse(%{
               "projects" => [
                 %{"id" => "same", "workflow_path" => "WORKFLOW.md"},
                 %{"id" => "same", "workflow_path" => "OTHER.md"}
               ]
             })

    assert message =~ "must be unique"
  end

  test "rejects invalid YAML before project validation" do
    root = tmp_root("invalid-yaml")
    config_path = Path.join(root, "projects.yml")
    File.mkdir_p!(root)
    File.write!(config_path, "projects: [\n")

    assert {:error, {:root_config_parse_error, _reason}} = RootConfig.load(config_path)
  after
    File.rm_rf(tmp_root("invalid-yaml"))
  end

  test "rejects non-list projects" do
    assert {:error, {:invalid_root_config, message}} = RootConfig.parse(%{"projects" => %{}})
    assert message =~ "projects must be a list"
  end

  test "rejects missing workflow paths" do
    assert {:error, {:invalid_root_config, message}} =
             RootConfig.parse(%{"projects" => [%{"id" => "missing-workflow"}]})

    assert message =~ "workflow_path is required"
  end

  test "rejects project ids that are not lower-case URL-safe identifiers" do
    assert {:error, {:invalid_root_config, message}} =
             RootConfig.parse(%{"projects" => [%{"id" => "Bad ID", "workflow_path" => "WORKFLOW.md"}]})

    assert message =~ "lower-case URL-safe"
  end

  test "uses default server host and allows absent port" do
    assert {:ok, %RootConfig{server: %{host: "127.0.0.1", port: nil}}} =
             RootConfig.parse(%{"projects" => []})
  end

  test "isolates workflow config failures to the invalid project context" do
    root = tmp_root("isolation")
    valid_workflow = Path.join(root, "valid/WORKFLOW.md")
    invalid_workflow = Path.join(root, "invalid/WORKFLOW.md")

    File.mkdir_p!(Path.dirname(valid_workflow))
    File.mkdir_p!(Path.dirname(invalid_workflow))
    File.write!(valid_workflow, "---\ntracker:\n  kind: memory\n---\nvalid")
    File.write!(invalid_workflow, "---\nrunner:\n  default: bad-runner\n---\ninvalid")

    assert {:ok, %RootConfig{projects: [valid, invalid]}} =
             RootConfig.parse(
               %{
                 "projects" => [
                   %{"id" => "valid", "enabled" => true, "workflow_path" => "valid/WORKFLOW.md"},
                   %{"id" => "invalid", "enabled" => true, "workflow_path" => "invalid/WORKFLOW.md"}
                 ]
               },
               root
             )

    assert valid.status == :valid
    assert valid.errors == []
    assert ProjectContext.dispatchable?(valid)

    assert invalid.status == :invalid
    assert [{:invalid_workflow_config, {:invalid_workflow_config, message}}] = invalid.errors
    assert message =~ "runner.default"
    assert ProjectContext.dispatch_blocker(invalid) == {:invalid_project, invalid.errors}
  after
    File.rm_rf(tmp_root("isolation"))
  end

  test "dispatch blockers cover missing workflow and execution gates" do
    root = tmp_root("dispatch")
    workflow = Path.join(root, "enabled/WORKFLOW.md")
    File.mkdir_p!(Path.dirname(workflow))
    File.write!(workflow, "---\ntracker:\n  kind: memory\n---\nenabled")

    assert {:ok, %RootConfig{projects: [missing, execution_disabled, gate_disabled]}} =
             RootConfig.parse(
               %{
                 "projects" => [
                   %{"id" => "missing", "enabled" => true, "workflow_path" => "missing/WORKFLOW.md"},
                   %{"id" => "exec-off", "enabled" => true, "workflow_path" => "enabled/WORKFLOW.md", "execution" => %{"enabled" => false}},
                   %{"id" => "gate-off", "enabled" => true, "workflow_path" => "enabled/WORKFLOW.md", "gates" => %{"dispatch_enabled" => false}}
                 ]
               },
               root
             )

    assert ProjectContext.dispatch_blocker(missing) == {:missing_workflow_file, Path.join(root, "missing/WORKFLOW.md")}
    assert ProjectContext.dispatch_blocker(execution_disabled) == :execution_disabled
    assert ProjectContext.dispatch_blocker(gate_disabled) == :gate_disabled
  after
    File.rm_rf(tmp_root("dispatch"))
  end

  defp tmp_root(name) do
    Path.join(System.tmp_dir!(), "symphony-root-config-#{name}")
  end
end
