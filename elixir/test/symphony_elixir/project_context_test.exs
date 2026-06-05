defmodule SymphonyElixir.ProjectContextTest do
  use ExUnit.Case, async: true

  alias SymphonyElixir.ProjectContext

  describe "new/1" do
    test "builds a context with all fields provided" do
      workflow_path = "/tmp/full/WORKFLOW.md"
      process_names = ProjectContext.process_names("custom")

      context =
        ProjectContext.new(%{
          id: "full-project",
          project_id: "ignored-project-id",
          name: "Full Project",
          enabled: true,
          status: :valid,
          repo_root: "/tmp/full",
          app_root: "/tmp/full/app",
          workflow_path: workflow_path,
          dashboard_order: 42,
          logs_root: "/tmp/full/logs",
          linear: %{"team" => %{"key" => "ENG"}},
          mnemesh: %{"task_id" => "task-1"},
          runner: %{"default" => "opencode"},
          execution: %{"enabled" => true, "max_concurrent_runs" => 2},
          gates: %{"dispatch_enabled" => true, "requires_review" => true},
          errors: [],
          process_names: process_names
        })

      assert %ProjectContext{
               id: "full-project",
               project_id: "full-project",
               name: "Full Project",
               enabled: true,
               status: :valid,
               repo_root: "/tmp/full",
               app_root: "/tmp/full/app",
               workflow_path: ^workflow_path,
               dashboard_order: 42,
               logs_root: "/tmp/full/logs",
               linear: %{"team" => %{"key" => "ENG"}},
               mnemesh: %{"task_id" => "task-1"},
               runner: %{"default" => "opencode"},
               execution: %{"enabled" => true, "max_concurrent_runs" => 2},
               gates: %{"dispatch_enabled" => true, "requires_review" => true},
               errors: [],
               process_names: ^process_names
             } = context
    end

    test "builds a disabled context with only id and workflow_path" do
      context = ProjectContext.new(%{id: "minimal", workflow_path: "/tmp/minimal/WORKFLOW.md"})

      assert context.id == "minimal"
      assert context.project_id == "minimal"
      assert context.name == "minimal"
      assert context.enabled == false
      assert context.status == :disabled
      assert context.workflow_path == "/tmp/minimal/WORKFLOW.md"
      assert context.linear == %{}
      assert context.mnemesh == %{}
      assert context.runner == %{}
      assert context.execution == %{"enabled" => true}
      assert context.gates == %{"dispatch_enabled" => true}
      assert context.errors == []
      assert context.process_names == ProjectContext.process_names("minimal")
    end

    test "normalizes string keys" do
      context =
        ProjectContext.new(%{
          "id" => "string-keyed",
          "name" => "String Keyed",
          "enabled" => true,
          "workflow_path" => "/tmp/string-keyed/WORKFLOW.md",
          "execution" => %{"enabled" => true},
          "gates" => %{"dispatch_enabled" => true}
        })

      assert context.id == "string-keyed"
      assert context.project_id == "string-keyed"
      assert context.name == "String Keyed"
      assert context.enabled == true
      assert context.status == :valid
      assert context.workflow_path == "/tmp/string-keyed/WORKFLOW.md"
    end

    test "raises a clear error when id and project_id are missing" do
      assert_raise ArgumentError, ~r/ProjectContext requires :id or :project_id/, fn ->
        ProjectContext.new(%{workflow_path: "/tmp/missing-id/WORKFLOW.md"})
      end
    end

    test "honors explicit status override" do
      context = ProjectContext.new(%{id: "override", enabled: true, status: :disabled, workflow_path: "/tmp/override/WORKFLOW.md"})

      assert context.status == :disabled
    end

    test "infers invalid status when enabled context has errors" do
      context = ProjectContext.new(%{id: "invalid", enabled: true, workflow_path: "/tmp/invalid/WORKFLOW.md", errors: [:bad]})

      assert context.status == :invalid
      assert context.errors == [:bad]
    end
  end

  describe "dispatchable?/1" do
    test "returns true when enabled, valid, workflow exists, execution is enabled, and gate is enabled" do
      workflow_path = write_workflow!("dispatchable")
      context = context("dispatchable", workflow_path: workflow_path)

      assert ProjectContext.dispatchable?(context)
    end

    test "returns false due to invalid status" do
      workflow_path = write_workflow!("invalid-status")
      context = context("invalid-status", workflow_path: workflow_path, status: :invalid, errors: [:bad])

      refute ProjectContext.dispatchable?(context)
    end

    test "returns false due to disabled project" do
      workflow_path = write_workflow!("disabled")
      context = context("disabled", workflow_path: workflow_path, enabled: false, status: :disabled)

      refute ProjectContext.dispatchable?(context)
    end

    test "returns false due to disabled execution" do
      workflow_path = write_workflow!("execution-disabled")
      context = context("execution-disabled", workflow_path: workflow_path, execution: %{"enabled" => false})

      refute ProjectContext.dispatchable?(context)
    end

    test "returns false due to disabled gate" do
      workflow_path = write_workflow!("gate-disabled")
      context = context("gate-disabled", workflow_path: workflow_path, gates: %{"dispatch_enabled" => "false"})

      refute ProjectContext.dispatchable?(context)
    end

    test "returns false due to missing workflow file" do
      context = context("missing-workflow", workflow_path: missing_workflow_path("missing-workflow"))

      refute ProjectContext.dispatchable?(context)
    end
  end

  describe "dispatch_blocker/1" do
    test "returns nil for a dispatchable project" do
      workflow_path = write_workflow!("no-blocker")
      context = context("no-blocker", workflow_path: workflow_path)

      assert ProjectContext.dispatch_blocker(context) == nil
    end

    test "returns invalid project blocker" do
      workflow_path = write_workflow!("invalid-blocker")
      context = context("invalid-blocker", workflow_path: workflow_path, status: :invalid, errors: [:bad])

      assert ProjectContext.dispatch_blocker(context) == {:invalid_project, [:bad]}
    end

    test "returns disabled blocker for disabled flag or disabled status" do
      workflow_path = write_workflow!("disabled-blocker")

      assert ProjectContext.dispatch_blocker(context("disabled-flag", workflow_path: workflow_path, enabled: false)) == :disabled
      assert ProjectContext.dispatch_blocker(context("disabled-status", workflow_path: workflow_path, status: :disabled)) == :disabled
    end

    test "returns execution disabled blocker" do
      workflow_path = write_workflow!("execution-blocker")
      context = context("execution-blocker", workflow_path: workflow_path, execution: %{"enabled" => "false"})

      assert ProjectContext.dispatch_blocker(context) == :execution_disabled
    end

    test "returns gate disabled blocker" do
      workflow_path = write_workflow!("gate-blocker")
      context = context("gate-blocker", workflow_path: workflow_path, gates: %{"dispatch_enabled" => false})

      assert ProjectContext.dispatch_blocker(context) == :gate_disabled
    end

    test "returns missing workflow file blocker" do
      workflow_path = missing_workflow_path("missing-blocker")
      context = context("missing-blocker", workflow_path: workflow_path)

      assert ProjectContext.dispatch_blocker(context) == {:missing_workflow_file, workflow_path}
    end

    test "treats nil execution and gates as enabled defaults" do
      workflow_path = write_workflow!("nil-maps")
      context = context("nil-maps", workflow_path: workflow_path, execution: nil, gates: nil)

      assert ProjectContext.dispatch_blocker(context) == nil
      assert ProjectContext.dispatchable?(context)
    end
  end

  describe "process_names/1" do
    test "returns expected names for project ids" do
      assert ProjectContext.process_names("alpha") == %{
               workflow_store: {:symphony_project, "alpha", :workflow_store},
               task_supervisor: {:symphony_project, "alpha", :task_supervisor},
               orchestrator: {:symphony_project, "alpha", :orchestrator},
               http_server: {:symphony_project, "alpha", :http_server},
               status_dashboard: {:symphony_project, "alpha", :status_dashboard}
             }

      assert ProjectContext.process_names("beta_project").orchestrator == {:symphony_project, "beta_project", :orchestrator}
    end

    test "preserves unusual project ids with hyphens, underscores, and numbers" do
      project_id = "project-2_alpha"

      assert ProjectContext.process_names(project_id) == %{
               workflow_store: {:symphony_project, project_id, :workflow_store},
               task_supervisor: {:symphony_project, project_id, :task_supervisor},
               orchestrator: {:symphony_project, project_id, :orchestrator},
               http_server: {:symphony_project, project_id, :http_server},
               status_dashboard: {:symphony_project, project_id, :status_dashboard}
             }
    end
  end

  defp context(id, attrs) do
    ProjectContext.new(
      Map.merge(
        %{
          id: id,
          enabled: true,
          workflow_path: "/tmp/#{id}/WORKFLOW.md",
          execution: %{"enabled" => true},
          gates: %{"dispatch_enabled" => true}
        },
        Map.new(attrs)
      )
    )
  end

  defp write_workflow!(name) do
    path = missing_workflow_path(name)
    File.mkdir_p!(Path.dirname(path))
    File.write!(path, "---\ntracker:\n  kind: memory\n---\n#{name}")
    path
  end

  defp missing_workflow_path(name) do
    Path.join([System.tmp_dir!(), "symphony-project-context-#{System.unique_integer([:positive])}", name, "WORKFLOW.md"])
  end
end
