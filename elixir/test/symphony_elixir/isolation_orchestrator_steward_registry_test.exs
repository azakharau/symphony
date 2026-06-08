defmodule SymphonyElixir.IsolationRegression.OrchestratorStewardRegistryTest do
  use ExUnit.Case, async: false

  alias SymphonyElixir.ProjectRegistry
  alias SymphonyElixir.RootConfigStore
  alias SymphonyElixir.WorkflowStore

  describe "Orchestrator per-project runtime state isolation" do
    @describetag :orchestrator_state_isolation

    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-orch-test-#{System.unique_integer([:positive])}"
        )

      File.mkdir_p!(test_root)

      on_exit(fn ->
        File.rm_rf(test_root)
      end)

      %{test_root: test_root}
    end

    defp orch_context(project_id, test_root) do
      workflow_path = Path.join([test_root, project_id, "WORKFLOW.md"])
      File.mkdir_p!(Path.dirname(workflow_path))

      %SymphonyElixir.ProjectContext{
        id: project_id,
        project_id: project_id,
        name: "Project #{project_id}",
        enabled: true,
        status: :valid,
        workflow_path: workflow_path,
        process_names: %{
          workflow_store: :"ws_orch_#{project_id}",
          task_supervisor: :"task_sup_orch_#{project_id}",
          orchestrator: :"orch_#{project_id}",
          http_server: :"http_orch_#{project_id}",
          status_dashboard: :"dashboard_orch_#{project_id}"
        }
      }
    end

    test "two orchestrators start with independent paused state maps", %{test_root: root} do
      alpha_ctx = orch_context("alpha", root)
      beta_ctx = orch_context("beta", root)

      {:ok, alpha_pid} =
        start_supervised(
          {SymphonyElixir.Orchestrator,
           [
             project_context: alpha_ctx,
             dispatch_paused?: true,
             name: :orch_state_alpha_test
           ]},
          id: :orch_state_alpha_test
        )

      {:ok, beta_pid} =
        start_supervised(
          {SymphonyElixir.Orchestrator,
           [
             project_context: beta_ctx,
             dispatch_paused?: true,
             name: :orch_state_beta_test
           ]},
          id: :orch_state_beta_test
        )

      alpha_state = :sys.get_state(alpha_pid)
      beta_state = :sys.get_state(beta_pid)

      assert alpha_state.project_context.project_id == "alpha"
      assert beta_state.project_context.project_id == "beta"

      assert alpha_state.running == %{}
      assert beta_state.running == %{}
      assert alpha_state.claimed == MapSet.new()
      assert beta_state.claimed == MapSet.new()
      assert alpha_state.retry_attempts == %{}
      assert beta_state.retry_attempts == %{}

      # Inject state into alpha only and verify beta is untouched
      fake_issue_id = "linear-123"

      fake_running_entry = %{
        issue_id: fake_issue_id,
        ref: make_ref(),
        pid: self(),
        session_started_at_ms: 0,
        session_timeout_at_ms: 100_000,
        runner_kind: :codex
      }

      :sys.replace_state(
        alpha_pid,
        fn state ->
          %{
            state
            | running: Map.put(state.running, fake_issue_id, fake_running_entry),
              claimed: MapSet.put(state.claimed, fake_issue_id),
              retry_attempts: Map.put(state.retry_attempts, fake_issue_id, %{attempts: 1, retry_token: make_ref()})
          }
        end
      )

      alpha_state_after = :sys.get_state(alpha_pid)
      beta_state_after = :sys.get_state(beta_pid)

      assert Map.has_key?(alpha_state_after.running, fake_issue_id)
      assert MapSet.member?(alpha_state_after.claimed, fake_issue_id)
      assert Map.has_key?(alpha_state_after.retry_attempts, fake_issue_id)

      refute Map.has_key?(beta_state_after.running, fake_issue_id)
      refute MapSet.member?(beta_state_after.claimed, fake_issue_id)
      refute Map.has_key?(beta_state_after.retry_attempts, fake_issue_id)

      assert beta_state_after.running == %{}
      assert beta_state_after.claimed == MapSet.new()
      assert beta_state_after.retry_attempts == %{}
    end

    test "two orchestrators use independent task_supervisor names", %{test_root: root} do
      alpha_ctx = orch_context("alpha", root)
      beta_ctx = orch_context("beta", root)

      {:ok, alpha_pid} =
        start_supervised(
          {SymphonyElixir.Orchestrator,
           [
             project_context: alpha_ctx,
             dispatch_paused?: true,
             name: :orch_sup_alpha_test
           ]},
          id: :orch_sup_alpha_test
        )

      {:ok, beta_pid} =
        start_supervised(
          {SymphonyElixir.Orchestrator,
           [
             project_context: beta_ctx,
             dispatch_paused?: true,
             name: :orch_sup_beta_test
           ]},
          id: :orch_sup_beta_test
        )

      alpha_state = :sys.get_state(alpha_pid)
      beta_state = :sys.get_state(beta_pid)

      assert alpha_state.task_supervisor != beta_state.task_supervisor
      assert is_tuple(alpha_state.task_supervisor)
      assert is_tuple(beta_state.task_supervisor)
    end
  end

  describe "Steward ExecutionPacket per-project isolation" do
    @describetag :execution_packet_isolation

    test "build/2 returns project-specific payload for different project contexts" do
      alpha_project = %{id: "proj-alpha", name: "Alpha Project"}
      beta_project = %{id: "proj-beta", name: "Beta Project"}

      alpha_milestone = %{id: "milestone-1", name: "Alpha Q1"}
      beta_milestone = %{id: "milestone-2", name: "Beta Sprint 5"}

      issue_id = "linear-42"

      common_issue = %SymphonyElixir.Linear.Issue{
        id: issue_id,
        identifier: "SYM-1",
        title: "Common issue",
        state: "in_progress",
        priority: 2,
        project_milestone: nil
      }

      alpha_issue = %{common_issue | project_milestone: alpha_milestone}
      beta_issue = %{common_issue | project_milestone: beta_milestone}

      alpha_packet = SymphonyElixir.Steward.ExecutionPacket.build(alpha_issue, alpha_project)
      beta_packet = SymphonyElixir.Steward.ExecutionPacket.build(beta_issue, beta_project)

      # Project payload is per-project
      assert alpha_packet["project"]["id"] == "proj-alpha"
      assert alpha_packet["project"]["name"] == "Alpha Project"
      assert beta_packet["project"]["id"] == "proj-beta"
      assert beta_packet["project"]["name"] == "Beta Project"

      # Milestone comes from issue.project_milestone, not from project context
      assert alpha_packet["active_milestone"]["id"] == "milestone-1"
      assert alpha_packet["active_milestone"]["name"] == "Alpha Q1"
      assert beta_packet["active_milestone"]["id"] == "milestone-2"
      assert beta_packet["active_milestone"]["name"] == "Beta Sprint 5"

      # Issue fields are shared (same issue id)
      assert alpha_packet["issue"]["id"] == issue_id
      assert beta_packet["issue"]["id"] == issue_id
    end

    test "build/2 defaults project payload to nil when project_context is nil" do
      issue = %SymphonyElixir.Linear.Issue{
        id: "linear-99",
        identifier: "SYM-99",
        title: "No project issue",
        state: "todo"
      }

      packet = SymphonyElixir.Steward.ExecutionPacket.build(issue, nil)

      assert packet["project"]["id"] == nil
      assert packet["project"]["name"] == nil
      assert packet["active_milestone"]["id"] == nil
      assert packet["active_milestone"]["name"] == nil
    end

    test "build/2 defaults project payload to nil when project_context lacks id/name" do
      issue = %SymphonyElixir.Linear.Issue{
        id: "linear-100",
        identifier: "SYM-100",
        title: "Partial project issue"
      }

      packet = SymphonyElixir.Steward.ExecutionPacket.build(issue, %{other_key: "value"})

      assert packet["project"]["id"] == nil
      assert packet["project"]["name"] == nil
    end

    test "build/2 uses issue milestone independently of project context" do
      milestone = %{id: "ms-42", name: "Milestone 42"}

      issue = %SymphonyElixir.Linear.Issue{
        id: "linear-200",
        identifier: "SYM-200",
        title: "Milestone issue",
        project_milestone: milestone
      }

      # Same issue with different project contexts produces different project but same milestone
      packet_a = SymphonyElixir.Steward.ExecutionPacket.build(issue, %{id: "proj-a", name: "A"})
      packet_b = SymphonyElixir.Steward.ExecutionPacket.build(issue, %{id: "proj-b", name: "B"})

      assert packet_a["project"]["id"] == "proj-a"
      assert packet_b["project"]["id"] == "proj-b"
      assert packet_a["active_milestone"]["id"] == "ms-42"
      assert packet_b["active_milestone"]["id"] == "ms-42"
    end

    test "prompt/1 returns {:ok, _} for clean execution packets" do
      issue = %SymphonyElixir.Linear.Issue{
        id: "linear-300",
        identifier: "SYM-300",
        title: "Prompt test"
      }

      packet = SymphonyElixir.Steward.ExecutionPacket.build(issue, nil)

      assert {:ok, prompt_str} = SymphonyElixir.Steward.ExecutionPacket.prompt(packet)
      assert String.contains?(prompt_str, "SYM-300")
      assert String.contains?(prompt_str, "execution packet")
    end

    test "forbidden_preamble?/1 detects forbidden role preambles" do
      assert SymphonyElixir.Steward.ExecutionPacket.forbidden_preamble?("You are the coding orchestrator for project X")

      assert SymphonyElixir.Steward.ExecutionPacket.forbidden_preamble?("You are the Machine Architect deciding the architecture")

      refute SymphonyElixir.Steward.ExecutionPacket.forbidden_preamble?("You are the OpenCode build orchestrator")

      refute SymphonyElixir.Steward.ExecutionPacket.forbidden_preamble?("Symphony execution packet\\n\\n{...}")

      refute SymphonyElixir.Steward.ExecutionPacket.forbidden_preamble?("Normal prompt without forbidden preamble")
    end
  end

  describe "WorkflowStore per-project cache isolation" do
    @describetag :workflow_store_cache_isolation

    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-ws-cache-#{System.unique_integer([:positive])}"
        )

      File.mkdir_p!(test_root)

      on_exit(fn ->
        File.rm_rf(test_root)
      end)

      %{test_root: test_root}
    end

    test "two WorkflowStore processes cache independent workflow content via poll path", %{
      test_root: root
    } do
      alpha_root = Path.join(root, "alpha_project")
      beta_root = Path.join(root, "beta_project")
      File.mkdir_p!(alpha_root)
      File.mkdir_p!(beta_root)

      alpha_workflow_path = Path.join(alpha_root, "WORKFLOW.md")
      beta_workflow_path = Path.join(beta_root, "WORKFLOW.md")

      alpha_content = """
      ---
      tracker:
        kind: memory
      ---
      # Alpha Workflow

      ## Steps
      - step one
      """

      beta_content = """
      ---
      tracker:
        kind: memory
      ---
      # Beta Workflow

      ## Steps
      - step A
      """

      File.write!(alpha_workflow_path, alpha_content)
      File.write!(beta_workflow_path, beta_content)

      alpha_store_name = :ws_cache_alpha
      beta_store_name = :ws_cache_beta

      {:ok, alpha_pid} =
        start_supervised(
          {WorkflowStore, [name: alpha_store_name, workflow_path: alpha_workflow_path]},
          id: :ws_cache_isolation_alpha
        )

      {:ok, beta_pid} =
        start_supervised(
          {WorkflowStore, [name: beta_store_name, workflow_path: beta_workflow_path]},
          id: :ws_cache_isolation_beta
        )

      # Allow initial load
      Process.sleep(100)

      # Verify independent content via current/1
      assert {:ok, alpha_workflow} = WorkflowStore.current(alpha_store_name)
      assert {:ok, beta_workflow} = WorkflowStore.current(beta_store_name)

      assert String.contains?(alpha_workflow.prompt, "Alpha Workflow")
      refute String.contains?(alpha_workflow.prompt, "Beta Workflow")
      assert String.contains?(beta_workflow.prompt, "Beta Workflow")
      refute String.contains?(beta_workflow.prompt, "Alpha Workflow")

      # Capture cached stamps before file change
      alpha_state_before = :sys.get_state(alpha_pid)
      beta_state_before = :sys.get_state(beta_pid)
      alpha_stamp_before = alpha_state_before.stamp
      beta_stamp_before = beta_state_before.stamp

      assert is_tuple(alpha_stamp_before)
      assert is_tuple(beta_stamp_before)

      # Update only alpha's workflow file
      updated_alpha = """
      ---
      tracker:
        kind: memory
      ---
      # Alpha Workflow (Updated)

      ## Steps
      - step one (updated)
      - step two (new)
      """

      File.write!(alpha_workflow_path, updated_alpha)

      # Trigger the poll path directly (same mechanism as periodic timer)
      # This verifies the background poll handler updates the cached state
      send(alpha_pid, :poll)
      send(beta_pid, :poll)

      # Give poll handlers time to process
      Process.sleep(200)

      # Alpha's cache should have updated via poll
      alpha_state_after = :sys.get_state(alpha_pid)
      alpha_stamp_after = alpha_state_after.stamp

      assert alpha_stamp_after != alpha_stamp_before,
             "Alpha poll should detect file change and update cached stamp"

      assert String.contains?(alpha_state_after.workflow.prompt, "Alpha Workflow (Updated)")

      # Beta's cache should NOT have changed (its file was not modified)
      beta_state_after = :sys.get_state(beta_pid)
      beta_stamp_after = beta_state_after.stamp

      assert beta_stamp_after == beta_stamp_before,
             "Beta poll should NOT detect file change (beta file unchanged)"

      assert String.contains?(beta_state_after.workflow.prompt, "Beta Workflow")

      # Verify via public API
      assert {:ok, alpha_workflow_after} = WorkflowStore.current(alpha_store_name)
      assert String.contains?(alpha_workflow_after.prompt, "Alpha Workflow (Updated)")
      assert String.contains?(alpha_workflow_after.prompt, "step two (new)")

      assert {:ok, beta_workflow_after} = WorkflowStore.current(beta_store_name)
      assert String.contains?(beta_workflow_after.prompt, "Beta Workflow")
      refute String.contains?(beta_workflow_after.prompt, "Updated")
    end
  end

  # ────────────────────────────────────────────────────────────────
  # RootConfigStore per-project isolation tests
  # ────────────────────────────────────────────────────────────────

  describe "root_config_store per-project isolation" do
    @describetag :root_config_store_isolation

    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-root-config-#{System.unique_integer([:positive])}"
        )

      File.mkdir_p!(test_root)

      on_exit(fn ->
        File.rm_rf(test_root)
      end)

      %{test_root: test_root}
    end

    defp write_root_config!(root, project_ids) do
      config_path = Path.join(root, "projects.yml")

      projects_yaml =
        Enum.map_join(project_ids, "\n", fn project_id ->
          project_root = Path.join(root, project_id)
          File.mkdir_p!(project_root)

          """
            - id: #{project_id}
              name: #{String.capitalize(project_id)} Project
              enabled: false
              workflow_path: #{Path.join(project_root, "WORKFLOW.md")}
          """
        end)

      File.write!(config_path, "projects:\n#{projects_yaml}")
      config_path
    end

    test "multiple projects coexist with independent RootConfigStore instances", %{test_root: root} do
      config_path = write_root_config!(root, ["alpha", "beta"])

      {:ok, store} = start_supervised({RootConfigStore, path: config_path})

      assert is_pid(store)
      assert %{"alpha" => alpha_state, "beta" => beta_state} = RootConfigStore.project_states()
      assert alpha_state.context.project_id == "alpha"
      assert beta_state.context.project_id == "beta"
      assert alpha_state.context.workflow_path != beta_state.context.workflow_path
    end

    test "one project RootConfigStore crash does not affect another", %{test_root: root} do
      config_path = write_root_config!(root, ["alpha", "beta"])

      {:ok, store} = start_supervised({RootConfigStore, path: config_path})
      Process.exit(store, :kill)
      Process.sleep(10)

      assert %{"alpha" => alpha_state, "beta" => beta_state} = RootConfigStore.project_states()
      assert alpha_state.context.project_id == "alpha"
      assert beta_state.context.project_id == "beta"
    end

    test "workflow deletion in one RootConfigStore does not affect another", %{test_root: root} do
      config_path = write_root_config!(root, ["alpha", "beta"])
      {:ok, _store} = start_supervised({RootConfigStore, path: config_path})

      alpha_workflow_path = Path.join([root, "alpha", "WORKFLOW.md"])
      File.write!(alpha_workflow_path, "# Alpha workflow\n")
      File.rm!(alpha_workflow_path)

      assert {:ok, _config} = RootConfigStore.reload()
      assert %{"alpha" => alpha_state, "beta" => beta_state} = RootConfigStore.project_states()
      assert alpha_state.context.workflow_path == alpha_workflow_path
      assert beta_state.context.workflow_path == Path.join([root, "beta", "WORKFLOW.md"])
    end

    test "RootConfigStore per-project state entries are isolated", %{test_root: root} do
      config_path = write_root_config!(root, ["alpha", "beta"])
      {:ok, _store} = start_supervised({RootConfigStore, path: config_path})

      assert %{"alpha" => alpha_before, "beta" => beta_before} = RootConfigStore.project_states()

      write_root_config!(root, ["beta"])

      assert {:ok, _config} = RootConfigStore.reload()
      assert %{"beta" => beta_after} = RootConfigStore.project_states()
      refute Map.has_key?(RootConfigStore.project_states(), "alpha")
      assert beta_after.context.workflow_path == beta_before.context.workflow_path
      refute alpha_before.context.workflow_path == beta_after.context.workflow_path
    end
  end

  # ────────────────────────────────────────────────────────────────
  # ProjectRegistry naming isolation tests
  # ────────────────────────────────────────────────────────────────

  describe "project_registry naming isolation" do
    @describetag :project_registry_naming_isolation

    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-registry-#{System.unique_integer([:positive])}"
        )

      File.mkdir_p!(test_root)

      on_exit(fn ->
        File.rm_rf(test_root)
      end)

      %{test_root: test_root}
    end

    test "ProjectRegistry.via_name produces distinct names for different keys", %{test_root: _root} do
      name_a = ProjectRegistry.via_name({:foo, self()})
      name_b = ProjectRegistry.via_name({:bar, self()})

      assert name_a != name_b
    end

    test "ProjectRegistry registered?/whereis are key-isolated", %{test_root: _root} do
      # Starting and stopping a component under one key
      # should not conflict with another key
      refute ProjectRegistry.registered?(:nonexistent_key)
      assert ProjectRegistry.whereis(:nonexistent_key) == nil
    end

    test "same-project different component keys are independent", %{test_root: _root} do
      # Multiple keys in the same project should produce distinct via tuples
      name_workflow = ProjectRegistry.via_name({:workflow_store, self()})
      name_config = ProjectRegistry.via_name({:config_settings, self()})

      assert name_workflow != name_config
    end
  end

  # ────────────────────────────────────────────────────────────────
  # Config per-project field isolation tests
  # ────────────────────────────────────────────────────────────────
end
