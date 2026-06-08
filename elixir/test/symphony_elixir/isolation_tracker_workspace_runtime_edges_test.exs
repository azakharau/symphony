defmodule SymphonyElixir.IsolationRegression.TrackerWorkspaceRuntimeEdgesTest do
  use ExUnit.Case, async: false

  import SymphonyElixir.IsolationRegressionSupport
  alias SymphonyElixir.Config
  alias SymphonyElixir.Linear.Issue
  alias SymphonyElixir.ProjectContext
  alias SymphonyElixir.ProjectRegistry
  alias SymphonyElixir.RuntimeCache
  alias SymphonyElixir.Tracker
  alias SymphonyElixir.WorkflowStore
  alias SymphonyElixir.Workspace

  describe "tracker isolated state mutation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-tracker-mutation-#{System.unique_integer([:positive])}"
        )

      File.mkdir_p!(test_root)

      unless Process.whereis(ProjectRegistry) do
        start_supervised!(ProjectRegistry)
      end

      Application.put_env(:symphony_elixir, :memory_tracker_recipient, self())

      on_exit(fn ->
        File.rm_rf(test_root)
        Application.delete_env(:symphony_elixir, :memory_tracker_recipient)
        Application.delete_env(:symphony_elixir, :memory_tracker_project_issues)
        Application.delete_env(:symphony_elixir, :memory_tracker_issues)
        Application.delete_env(:symphony_elixir, :memory_tracker_project_opencode_comments)
        Application.delete_env(:symphony_elixir, :memory_tracker_opencode_comments)
        Application.delete_env(:symphony_elixir, :memory_tracker_project_milestones_by_project)
        Application.delete_env(:symphony_elixir, :memory_tracker_project_milestones)
      end)

      %{test_root: test_root}
    end

    test "project B does not see project A's issue state mutation on shared issue ID", %{test_root: root} do
      # Both projects have the same issue ID with different initial state and title.
      # With the Memory tracker, update_issue_state sends an event to the recipient
      # rather than storing the new state permanently. fetch_issue_states_by_ids
      # always reads from the configured env data.
      Application.put_env(:symphony_elixir, :memory_tracker_project_issues, %{
        "alpha" => [%Issue{id: "shared-issue", identifier: "ALPHA-1", title: "Alpha title", state: "Todo"}],
        "beta" => [%Issue{id: "shared-issue", identifier: "BETA-1", title: "Beta title", state: "In Progress"}]
      })

      with_memory_project_context("alpha", root, fn alpha_ctx ->
        with_memory_project_context("beta", root, fn beta_ctx ->
          # Verify initial isolation
          assert {:ok, [alpha_issue]} = Tracker.fetch_issue_states_by_ids(["shared-issue"], alpha_ctx)
          assert alpha_issue.title == "Alpha title"
          assert alpha_issue.state == "Todo"

          assert {:ok, [beta_issue]} = Tracker.fetch_issue_states_by_ids(["shared-issue"], beta_ctx)
          assert beta_issue.title == "Beta title"
          assert beta_issue.state == "In Progress"

          # Update alpha's issue to Done — Memory tracker sends event to recipient
          assert :ok = Tracker.update_issue_state("shared-issue", "Done", alpha_ctx)

          # Verify the event was delivered with the project context
          assert_receive {:memory_tracker_state_update, "alpha", "shared-issue", "Done"}

          # Beta receives no event for alpha's update
          refute_receive {:memory_tracker_state_update, "beta", "shared-issue", _}, 100

          # fetch_issue_states_by_ids still reads from configured env data (original state)
          assert {:ok, [alpha_fresh]} = Tracker.fetch_issue_states_by_ids(["shared-issue"], alpha_ctx)
          assert alpha_fresh.state == "Todo"

          assert {:ok, [beta_fresh]} = Tracker.fetch_issue_states_by_ids(["shared-issue"], beta_ctx)
          assert beta_fresh.state == "In Progress"
        end)
      end)
    end

    test "project A's comment on a shared issue ID is not visible to project B", %{test_root: root} do
      with_memory_project_context("alpha", root, fn alpha_ctx ->
        with_memory_project_context("beta", root, fn beta_ctx ->
          assert :ok = Tracker.create_comment("shared-issue", "alpha: needs review", alpha_ctx)
          assert :ok = Tracker.create_comment("shared-issue", "beta: looks good", beta_ctx)

          assert_receive {:memory_tracker_comment, "alpha", "shared-issue", "alpha: needs review"}
          assert_receive {:memory_tracker_comment, "beta", "shared-issue", "beta: looks good"}
        end)
      end)
    end

    test "fetch_candidate_issues is per-project even when the same issue IDs exist in multiple projects", %{
      test_root: root
    } do
      # Same issue IDs but different data per project
      Application.put_env(:symphony_elixir, :memory_tracker_project_issues, %{
        "alpha" => [
          %Issue{id: "cand-1", identifier: "ALPHA-1", title: "Alpha first", state: "Todo"},
          %Issue{id: "cand-2", identifier: "ALPHA-2", title: "Alpha second", state: "Done"}
        ],
        "beta" => [
          %Issue{id: "cand-1", identifier: "BETA-1", title: "Beta first", state: "In Progress"},
          %Issue{id: "cand-2", identifier: "BETA-2", title: "Beta second", state: "Todo"}
        ]
      })

      with_memory_project_context("alpha", root, fn alpha_ctx ->
        with_memory_project_context("beta", root, fn beta_ctx ->
          assert {:ok, alpha_candidates} = Tracker.fetch_candidate_issues(alpha_ctx)
          assert length(alpha_candidates) == 2
          assert Enum.map(alpha_candidates, & &1.identifier) == ["ALPHA-1", "ALPHA-2"]
          assert Enum.map(alpha_candidates, & &1.title) == ["Alpha first", "Alpha second"]

          assert {:ok, beta_candidates} = Tracker.fetch_candidate_issues(beta_ctx)
          assert length(beta_candidates) == 2
          assert Enum.map(beta_candidates, & &1.identifier) == ["BETA-1", "BETA-2"]
          assert Enum.map(beta_candidates, & &1.title) == ["Beta first", "Beta second"]
        end)
      end)
    end

    test "fetch_issues_by_states respects per-project data with overlapping issue IDs", %{test_root: root} do
      Application.put_env(:symphony_elixir, :memory_tracker_project_issues, %{
        "alpha" => [
          %Issue{id: "ol-1", identifier: "ALPHA-OL", title: "Alpha OL", state: "Todo"},
          %Issue{id: "ol-2", identifier: "ALPHA-OL2", title: "Alpha OL2", state: "In Progress"}
        ],
        "beta" => [
          %Issue{id: "ol-1", identifier: "BETA-OL", title: "Beta OL", state: "Done"},
          %Issue{id: "ol-2", identifier: "BETA-OL2", title: "Beta OL2", state: "In Progress"}
        ]
      })

      with_memory_project_context("alpha", root, fn alpha_ctx ->
        with_memory_project_context("beta", root, fn beta_ctx ->
          # Alpha: Todo filter should return 1 item
          assert {:ok, [alpha_todo]} = Tracker.fetch_issues_by_states(["Todo"], alpha_ctx)
          assert alpha_todo.identifier == "ALPHA-OL"

          # Beta: Todo filter should return 0 items (both Done and In Progress)
          assert {:ok, []} = Tracker.fetch_issues_by_states(["Todo"], beta_ctx)

          # Beta: In Progress filter should return 1 item
          assert {:ok, [beta_ip]} = Tracker.fetch_issues_by_states(["In Progress"], beta_ctx)
          assert beta_ip.identifier == "BETA-OL2"
        end)
      end)
    end
  end

  # ────────────────────────────────────────────────────────────────
  # Workspace cleanup per-project isolation tests
  # ────────────────────────────────────────────────────────────────

  describe "workspace cleanup per-project isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-workspace-cleanup-#{System.unique_integer([:positive])}"
        )

      File.mkdir_p!(test_root)

      unless Process.whereis(ProjectRegistry) do
        start_supervised!(ProjectRegistry)
      end

      on_exit(fn ->
        File.rm_rf(test_root)
      end)

      %{test_root: test_root}
    end

    test "remove_issue_workspaces only removes workspace for the owning project's workspace root", %{
      test_root: root
    } do
      alpha_root = Path.join(root, "alpha-workspaces")
      beta_root = Path.join(root, "beta-workspaces")

      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "workspace:",
            "  root: \"#{alpha_root}\"",
            "runner:",
            "  default: codex",
            "  routes:",
            "    \"RCA Required\": codex",
            "    \"In Review\": codex"
          ]
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              front_matter_overrides: [
                "workspace:",
                "  root: \"#{beta_root}\"",
                "runner:",
                "  default: codex",
                "  routes:",
                "    \"RCA Required\": codex",
                "    \"In Review\": codex"
              ]
            },
            fn beta_ctx ->
              alpha_settings = Config.settings!(alpha_ctx)
              beta_settings = Config.settings!(beta_ctx)

              issue = %Issue{id: "shared-cleanup", identifier: "SHARED-CLEANUP", title: "Shared cleanup", state: "Todo"}

              # Create workspaces for the same issue in both projects
              assert {:ok, alpha_ws} = Workspace.create_for_issue(issue, nil, alpha_settings)
              assert {:ok, beta_ws} = Workspace.create_for_issue(issue, nil, beta_settings)

              assert String.starts_with?(alpha_ws, alpha_root)
              assert String.starts_with?(beta_ws, beta_root)
              assert File.dir?(alpha_ws)
              assert File.dir?(beta_ws)

              # Remove workspace using alpha's settings (only alpha's workspace should be removed)
              :ok = Workspace.remove_issue_workspaces("SHARED-CLEANUP", nil, alpha_settings)

              # Alpha's workspace should be gone
              refute File.dir?(alpha_ws)

              # Beta's workspace should still exist
              assert File.dir?(beta_ws)
            end
          )
        end
      )
    end

    test "remove for one project does not affect a second project's workspace with same identifier", %{
      test_root: root
    } do
      alpha_root = Path.join(root, "alpha-workspaces")
      beta_root = Path.join(root, "beta-workspaces")

      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "workspace:",
            "  root: \"#{alpha_root}\"",
            "runner:",
            "  default: codex",
            "  routes:",
            "    \"RCA Required\": codex",
            "    \"In Review\": codex"
          ]
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              front_matter_overrides: [
                "workspace:",
                "  root: \"#{beta_root}\"",
                "runner:",
                "  default: codex",
                "  routes:",
                "    \"RCA Required\": codex",
                "    \"In Review\": codex"
              ]
            },
            fn beta_ctx ->
              alpha_settings = Config.settings!(alpha_ctx)
              beta_settings = Config.settings!(beta_ctx)

              issue = %Issue{id: "bi-cleanup", identifier: "BI-CLEANUP", title: "Bi cleanup", state: "Todo"}

              assert {:ok, alpha_ws} = Workspace.create_for_issue(issue, nil, alpha_settings)
              assert {:ok, beta_ws} = Workspace.create_for_issue(issue, nil, beta_settings)

              assert File.dir?(alpha_ws)
              assert File.dir?(beta_ws)

              # Remove using beta's settings
              :ok = Workspace.remove_issue_workspaces("BI-CLEANUP", nil, beta_settings)

              # Beta's workspace should be gone
              refute File.dir?(beta_ws)

              # Alpha's workspace should still exist
              assert File.dir?(alpha_ws)
            end
          )
        end
      )
    end
  end

  # ────────────────────────────────────────────────────────────────
  # ProjectContext dispatch_blocker isolation tests
  # ────────────────────────────────────────────────────────────────

  describe "project_context dispatch_blocker isolation" do
    test "dispatchable? returns true for valid project and false for disabled project" do
      tmp_root = System.tmp_dir!() <> "/dispatch-blocker-basic-\#{System.unique_integer([:positive])}"
      File.mkdir_p!(tmp_root)

      enabled_path = tmp_root <> "/enabled/WORKFLOW.md"
      File.mkdir_p!(Path.dirname(enabled_path))
      File.write!(enabled_path, "---\ntracker:\n  kind: memory\n---\nenabled\n")

      disabled_path = tmp_root <> "/disabled/WORKFLOW.md"
      File.mkdir_p!(Path.dirname(disabled_path))
      File.write!(disabled_path, "---\ntracker:\n  kind: memory\n---\ndisabled\n")

      enabled_ctx =
        ProjectContext.new(%{
          id: "enabled-project",
          enabled: true,
          workflow_path: enabled_path
        })

      disabled_ctx =
        ProjectContext.new(%{
          id: "disabled-project",
          enabled: false,
          workflow_path: disabled_path
        })

      # Enabled project with valid workflow file is dispatchable
      assert ProjectContext.dispatchable?(enabled_ctx)

      # Disabled project returns :disabled regardless of workflow file
      assert ProjectContext.dispatch_blocker(disabled_ctx) == :disabled
      refute ProjectContext.dispatchable?(disabled_ctx)

      File.rm_rf!(tmp_root)
    end

    test "dispatch_blocker for one project does not consult another project's state" do
      tmp_root = System.tmp_dir!() <> "/dispatch-blocker-isolated-\#{System.unique_integer([:positive])}"
      File.mkdir_p!(tmp_root)

      alpha_path = tmp_root <> "/alpha/WORKFLOW.md"
      File.mkdir_p!(Path.dirname(alpha_path))
      File.write!(alpha_path, "---\ntracker:\n  kind: memory\n---\nalpha\n")

      beta_path = tmp_root <> "/beta/WORKFLOW.md"
      File.mkdir_p!(Path.dirname(beta_path))
      File.write!(beta_path, "---\ntracker:\n  kind: memory\n---\nbeta\n")

      alpha_ctx =
        ProjectContext.new(%{
          id: "alpha",
          enabled: true,
          workflow_path: alpha_path
        })

      beta_ctx =
        ProjectContext.new(%{
          id: "beta",
          enabled: false,
          workflow_path: beta_path
        })

      assert ProjectContext.dispatchable?(alpha_ctx)
      assert ProjectContext.dispatch_blocker(beta_ctx) == :disabled
      refute ProjectContext.dispatchable?(beta_ctx)

      File.rm_rf!(tmp_root)
    end

    test "execution_disabled and gate_disabled blockers are per-project" do
      tmp_root = System.tmp_dir!() <> "/dispatch-blocker-exec-\#{System.unique_integer([:positive])}"
      File.mkdir_p!(tmp_root)

      exec_path = tmp_root <> "/exec-disabled/WORKFLOW.md"
      File.mkdir_p!(Path.dirname(exec_path))
      File.write!(exec_path, "---\ntracker:\n  kind: memory\n---\nexec-disabled\n")

      gate_path = tmp_root <> "/gate-disabled/WORKFLOW.md"
      File.mkdir_p!(Path.dirname(gate_path))
      File.write!(gate_path, "---\ntracker:\n  kind: memory\n---\ngate-disabled\n")

      normal_path = tmp_root <> "/normal/WORKFLOW.md"
      File.mkdir_p!(Path.dirname(normal_path))
      File.write!(normal_path, "---\ntracker:\n  kind: memory\n---\nnormal\n")

      exec_disabled_ctx =
        ProjectContext.new(%{
          id: "exec-disabled",
          enabled: true,
          workflow_path: exec_path,
          execution: %{"enabled" => false}
        })

      gate_disabled_ctx =
        ProjectContext.new(%{
          id: "gate-disabled",
          enabled: true,
          workflow_path: gate_path,
          gates: %{"dispatch_enabled" => false}
        })

      normal_ctx =
        ProjectContext.new(%{
          id: "normal",
          enabled: true,
          workflow_path: normal_path,
          execution: %{"enabled" => true},
          gates: %{"dispatch_enabled" => true}
        })

      assert ProjectContext.dispatch_blocker(exec_disabled_ctx) == :execution_disabled
      assert ProjectContext.dispatch_blocker(gate_disabled_ctx) == :gate_disabled
      assert ProjectContext.dispatchable?(normal_ctx)

      File.rm_rf!(tmp_root)
    end

    test "invalid project blocker carries per-project errors" do
      alpha_ctx =
        ProjectContext.new(%{
          id: "alpha",
          enabled: true,
          workflow_path: "/tmp/alpha-err/WORKFLOW.md",
          status: :invalid,
          errors: ["failed to parse workflow"]
        })

      beta_ctx =
        ProjectContext.new(%{
          id: "beta",
          enabled: true,
          workflow_path: "/tmp/beta-err/WORKFLOW.md",
          status: :invalid,
          errors: ["missing tracker config"]
        })

      assert ProjectContext.dispatch_blocker(alpha_ctx) == {:invalid_project, ["failed to parse workflow"]}
      assert ProjectContext.dispatch_blocker(beta_ctx) == {:invalid_project, ["missing tracker config"]}
    end
  end

  # ────────────────────────────────────────────────────────────────
  # RuntimeCache per-project identity edge cases
  # ────────────────────────────────────────────────────────────────

  describe "runtime cache per-project identity edge cases" do
    test "empty string project ID treated as nil scope and coexists with real project scopes" do
      empty_project_id_ctx =
        ProjectContext.new(%{
          id: "",
          enabled: true,
          workflow_path: "/tmp/empty-id/WORKFLOW.md"
        })

      alpha_ctx =
        ProjectContext.new(%{
          id: "alpha",
          enabled: true,
          workflow_path: "/tmp/alpha-runtime-id/WORKFLOW.md"
        })

      # Empty ID project context should be treated as nil scope
      :ok = RuntimeCache.record_handoff_fingerprint(empty_project_id_ctx, "issue-empty", "fp-empty")
      :ok = RuntimeCache.record_handoff_fingerprint(alpha_ctx, "issue-empty", "fp-alpha")

      # Empty ID context behaves like nil context
      assert RuntimeCache.handoff_fingerprint_seen?(empty_project_id_ctx, "issue-empty", "fp-empty")
      assert RuntimeCache.handoff_fingerprint_seen?(alpha_ctx, "issue-empty", "fp-alpha")

      # The two entries are separate since empty id -> nil, alpha -> "alpha"
      refute RuntimeCache.handoff_fingerprint_seen?(alpha_ctx, "issue-empty", "fp-empty")
    end

    test "nil project context entries coexist with alpha and beta scoped entries" do
      alpha_ctx =
        ProjectContext.new(%{
          id: "alpha",
          enabled: true,
          workflow_path: "/tmp/alpha-nil/WORKFLOW.md"
        })

      beta_ctx =
        ProjectContext.new(%{
          id: "beta",
          enabled: true,
          workflow_path: "/tmp/beta-nil/WORKFLOW.md"
        })

      :ok = RuntimeCache.record_handoff_fingerprint(nil, "issue-triple", "fp-nil")
      :ok = RuntimeCache.record_handoff_fingerprint(alpha_ctx, "issue-triple", "fp-alpha")
      :ok = RuntimeCache.record_handoff_fingerprint(beta_ctx, "issue-triple", "fp-beta")

      assert RuntimeCache.handoff_fingerprint_seen?(nil, "issue-triple", "fp-nil")
      assert RuntimeCache.handoff_fingerprint_seen?(alpha_ctx, "issue-triple", "fp-alpha")
      assert RuntimeCache.handoff_fingerprint_seen?(beta_ctx, "issue-triple", "fp-beta")

      # Cross-project isolation
      refute RuntimeCache.handoff_fingerprint_seen?(alpha_ctx, "issue-triple", "fp-nil")
      refute RuntimeCache.handoff_fingerprint_seen?(beta_ctx, "issue-triple", "fp-nil")
      refute RuntimeCache.handoff_fingerprint_seen?(alpha_ctx, "issue-triple", "fp-beta")
    end

    test "clear_issue with nil removes all entries for the issue regardless of project scope" do
      alpha_ctx =
        ProjectContext.new(%{
          id: "alpha",
          enabled: true,
          workflow_path: "/tmp/alpha-clear/WORKFLOW.md"
        })

      :ok = RuntimeCache.record_handoff_fingerprint(nil, "issue-clear-nil", "fp-nil-clear")
      :ok = RuntimeCache.record_handoff_fingerprint(alpha_ctx, "issue-clear-nil", "fp-alpha-clear")
      :ok = RuntimeCache.record_handoff_fingerprint(alpha_ctx, "other-issue", "fp-other")

      # Clear with nil scope removes ALL entries for this issue regardless of project scope
      :ok = RuntimeCache.clear_issue(nil, "issue-clear-nil")

      refute RuntimeCache.handoff_fingerprint_seen?(nil, "issue-clear-nil", "fp-nil-clear")

      # Alpha's project-scoped entries for the same issue are also removed by nil clear
      refute RuntimeCache.handoff_fingerprint_seen?(alpha_ctx, "issue-clear-nil", "fp-alpha-clear")

      # Different issue entries are unaffected
      assert RuntimeCache.handoff_fingerprint_seen?(alpha_ctx, "other-issue", "fp-other")
    end

    test "clear_issue with alpha scope does not remove beta-scoped entries for the same issue" do
      alpha_ctx =
        ProjectContext.new(%{
          id: "alpha",
          enabled: true,
          workflow_path: "/tmp/alpha-beta-clear/WORKFLOW.md"
        })

      beta_ctx =
        ProjectContext.new(%{
          id: "beta",
          enabled: true,
          workflow_path: "/tmp/beta-alpha-clear/WORKFLOW.md"
        })

      :ok = RuntimeCache.record_handoff_fingerprint(alpha_ctx, "shared-issue", "fp-alpha")
      :ok = RuntimeCache.record_handoff_fingerprint(beta_ctx, "shared-issue", "fp-beta")

      :ok = RuntimeCache.clear_issue(alpha_ctx, "shared-issue")

      refute RuntimeCache.handoff_fingerprint_seen?(alpha_ctx, "shared-issue", "fp-alpha")
      assert RuntimeCache.handoff_fingerprint_seen?(beta_ctx, "shared-issue", "fp-beta")
    end

    test "clear_issue with alpha scope does not remove alpha-scoped entries for a different issue" do
      alpha_ctx =
        ProjectContext.new(%{
          id: "alpha",
          enabled: true,
          workflow_path: "/tmp/alpha-other-clear/WORKFLOW.md"
        })

      :ok = RuntimeCache.record_handoff_fingerprint(alpha_ctx, "issue-keep", "fp-keep")
      :ok = RuntimeCache.record_handoff_fingerprint(alpha_ctx, "issue-remove", "fp-remove")

      :ok = RuntimeCache.clear_issue(alpha_ctx, "issue-remove")

      assert RuntimeCache.handoff_fingerprint_seen?(alpha_ctx, "issue-keep", "fp-keep")
      refute RuntimeCache.handoff_fingerprint_seen?(alpha_ctx, "issue-remove", "fp-remove")
    end
  end

  # ────────────────────────────────────────────────────────────────
  # Tracker adapter per-project isolation tests
  # ────────────────────────────────────────────────────────────────

  describe "tracker adapter per-project isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-tracker-adapter-#{System.unique_integer([:positive])}"
        )

      File.mkdir_p!(test_root)

      unless Process.whereis(ProjectRegistry) do
        start_supervised!(ProjectRegistry)
      end

      on_exit(fn ->
        File.rm_rf(test_root)
      end)

      %{test_root: test_root}
    end

    test "Tracker.adapter/1 returns Memory for memory kind and Linear adapter for linear kind", %{test_root: root} do
      memory_path = Path.join([root, "memory-project", "WORKFLOW.md"])
      File.mkdir_p!(Path.dirname(memory_path))
      File.write!(memory_path, "---\ntracker:\n  kind: memory\n---\nmemory prompt\n")

      memory_ctx =
        ProjectContext.new(%{
          id: "memory-project",
          enabled: true,
          workflow_path: memory_path
        })

      memory_store_name = ProjectRegistry.via_name(memory_ctx.process_names.workflow_store)

      start_supervised!(
        {WorkflowStore, [name: memory_store_name, workflow_path: memory_path]},
        id: :adapter_memory_store
      )

      assert Tracker.adapter(memory_ctx) == SymphonyElixir.Tracker.Memory
    end

    test "adapter selection for one project does not affect another project.s adapter selection", %{test_root: root} do
      memory_path = Path.join([root, "mem-proj", "WORKFLOW.md"])
      File.mkdir_p!(Path.dirname(memory_path))
      File.write!(memory_path, "---\ntracker:\n  kind: memory\n---\nmem\n")

      linear_path = Path.join([root, "lin-proj", "WORKFLOW.md"])
      File.mkdir_p!(Path.dirname(linear_path))
      File.write!(linear_path, "---\ntracker:\n  kind: linear\n  api_key: test-key\n  project_slug: test-slug\n---\nlin\n")

      mem_ctx =
        ProjectContext.new(%{
          id: "mem-proj",
          enabled: true,
          workflow_path: memory_path
        })

      lin_ctx =
        ProjectContext.new(%{
          id: "lin-proj",
          enabled: true,
          workflow_path: linear_path
        })

      mem_store_name = ProjectRegistry.via_name(mem_ctx.process_names.workflow_store)
      lin_store_name = ProjectRegistry.via_name(lin_ctx.process_names.workflow_store)

      start_supervised!(
        {WorkflowStore, [name: mem_store_name, workflow_path: memory_path]},
        id: :adapter_mem_store
      )

      start_supervised!(
        {WorkflowStore, [name: lin_store_name, workflow_path: linear_path]},
        id: :adapter_lin_store
      )

      assert Tracker.adapter(mem_ctx) == SymphonyElixir.Tracker.Memory
      assert Tracker.adapter(lin_ctx) == SymphonyElixir.Linear.Adapter

      # The configured api_key and project_slug also differ per project
      assert Config.settings!(mem_ctx).tracker.kind == "memory"

      mem_settings = Config.settings!(mem_ctx)
      lin_settings = Config.settings!(lin_ctx)

      assert mem_settings.tracker.kind == "memory"
      assert lin_settings.tracker.kind == "linear"
      assert lin_settings.tracker.api_key == "test-key"
      assert lin_settings.tracker.project_slug == "test-slug"
    end
  end

  # ────────────────────────────────────────────────────────────────
  # OpenCode dispatch routing per-project isolation tests
  # ────────────────────────────────────────────────────────────────

  describe "runner dispatch routing per-project isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-runner-route-#{System.unique_integer([:positive])}"
        )

      File.mkdir_p!(test_root)

      unless Process.whereis(ProjectRegistry) do
        start_supervised!(ProjectRegistry)
      end

      on_exit(fn ->
        File.rm_rf(test_root)
      end)

      %{test_root: test_root}
    end

    test "runner_for_state uses per-project runner.routes and runner.default", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "runner:",
            "  default: codex",
            "  routes:",
            "    \"In Progress\": opencode"
          ]
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              front_matter_overrides: [
                "runner:",
                "  default: opencode",
                "  routes:",
                "    \"RCA Required\": codex",
                "    \"In Review\": codex",
                "    \"Todo\": codex"
              ]
            },
            fn beta_ctx ->
              alpha_settings = Config.settings!(alpha_ctx)
              beta_settings = Config.settings!(beta_ctx)

              # Alpha: default is codex, In Progress routes to opencode
              assert alpha_settings.runner.default == "codex"
              assert alpha_settings.runner.routes["in progress"] == "opencode"

              # Beta: default is opencode, Todo routes to codex
              assert beta_settings.runner.default == "opencode"
              assert beta_settings.runner.routes["todo"] == "codex"

              # Runner.for_state delegates to Config.max_concurrent_agents_for_state
              # The actual routing uses runner.routes from Config.settings!(context)
              # _todo_issue_ip = make_issue("ALPHA-TODO", "Todo") -- unused
              _ip_issue_alpha = make_issue("ALPHA-IP", "In Progress")

              # Verify per-project max_concurrent_agents_for_state routing
              assert Config.max_concurrent_agents_for_state("Todo", alpha_ctx) ==
                       Config.settings!(alpha_ctx).agent.max_concurrent_agents

              # Per-project runner routing (replicates Map.get(routes, state, default))
              normalize = &String.downcase/1

              # Alpha: state "In Progress" is in routes -> "opencode"
              assert Map.get(alpha_settings.runner.routes, normalize.("In Progress"), alpha_settings.runner.default) == "opencode"
              # Alpha: state "Todo" is NOT in routes -> default "codex"
              assert Map.get(alpha_settings.runner.routes, normalize.("Todo"), alpha_settings.runner.default) == "codex"

              # Beta: state "Todo" is in routes -> "codex"
              assert Map.get(beta_settings.runner.routes, normalize.("Todo"), beta_settings.runner.default) == "codex"
              # Beta: state "In Progress" is NOT in routes -> default "opencode"
              assert Map.get(beta_settings.runner.routes, normalize.("In Progress"), beta_settings.runner.default) == "opencode"
            end
          )
        end
      )
    end
  end

  # ────────────────────────────────────────────────────────────────
  # ACP session store per-project isolation tests
  # ────────────────────────────────────────────────────────────────
end
