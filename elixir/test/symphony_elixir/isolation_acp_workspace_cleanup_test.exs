defmodule SymphonyElixir.IsolationRegression.AcpWorkspaceCleanupTest do
  use ExUnit.Case, async: false

  import SymphonyElixir.IsolationRegressionSupport
  alias SymphonyElixir.Config
  alias SymphonyElixir.Linear.Issue
  alias SymphonyElixir.OpenCode.ACPSessionStore
  alias SymphonyElixir.ProjectRegistry
  alias SymphonyElixir.Workspace

  describe "acp session store per-project isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-acp-session-#{System.unique_integer([:positive])}"
        )

      File.mkdir_p!(test_root)

      unless Process.whereis(ProjectRegistry) do
        start_supervised!(ProjectRegistry)
      end

      Application.put_env(:symphony_elixir, :memory_tracker_recipient, self())

      on_exit(fn ->
        File.rm_rf(test_root)
        Application.delete_env(:symphony_elixir, :memory_tracker_recipient)
      end)

      %{test_root: test_root}
    end

    test "acp session store paths differ per project workspace root", %{test_root: root} do
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

              alpha_session_path = ACPSessionStore.path(settings: alpha_settings)
              beta_session_path = ACPSessionStore.path(settings: beta_settings)

              # Different workspace roots produce different session store paths
              refute alpha_session_path == beta_session_path
              assert String.starts_with?(alpha_session_path, alpha_root)
              assert String.starts_with?(beta_session_path, beta_root)
            end
          )
        end
      )
    end

    test "acp session store put/fetch is isolated per project workspace root", %{test_root: root} do
      alpha_root = Path.join(root, "alpha-workspaces")
      beta_root = Path.join(root, "beta-workspaces")

      File.mkdir_p!(alpha_root)
      File.mkdir_p!(beta_root)

      issue = %Issue{id: "shared-acp", identifier: "SHARED-ACP", title: "Shared ACP", state: "Todo"}

      # Store a session for the same issue in alpha's workspace root
      assert :ok = ACPSessionStore.put(issue, alpha_root, "ses-alpha-123")

      # Store a different session for the same issue in beta's workspace root
      assert :ok = ACPSessionStore.put(issue, beta_root, "ses-beta-456")

      # Alpha can retrieve its session
      assert {:ok, "ses-alpha-123"} = ACPSessionStore.fetch(issue, alpha_root)

      # Beta can retrieve its own session
      assert {:ok, "ses-beta-456"} = ACPSessionStore.fetch(issue, beta_root)

      # Alpha does not see beta's session and vice versa
      refute {:ok, "ses-beta-456"} == ACPSessionStore.fetch(issue, alpha_root)
      refute {:ok, "ses-alpha-123"} == ACPSessionStore.fetch(issue, beta_root)
    end

    test "acp session store remove_issue only affects the specified project's workspace root", %{test_root: root} do
      alpha_root = Path.join(root, "alpha-workspaces")
      beta_root = Path.join(root, "beta-workspaces")

      File.mkdir_p!(alpha_root)
      File.mkdir_p!(beta_root)

      alpha_opts = [settings: %{workspace: %{root: alpha_root}}]
      beta_opts = [settings: %{workspace: %{root: beta_root}}]

      issue = %Issue{id: "shared-acp-remove", identifier: "SHARED-ACP-REMOVE", title: "Shared ACP remove", state: "Todo"}

      # Store sessions in per-project store files
      assert :ok = ACPSessionStore.put(issue, alpha_root, "ses-alpha-789", nil, alpha_opts)
      assert :ok = ACPSessionStore.put(issue, beta_root, "ses-beta-012", nil, beta_opts)

      # Remove from alpha's store
      assert :ok = ACPSessionStore.remove_issue(issue, alpha_opts)

      # Alpha's session is gone
      assert {:ok, nil} = ACPSessionStore.fetch(issue, alpha_root, nil, alpha_opts)

      # Beta's session remains
      assert {:ok, "ses-beta-012"} = ACPSessionStore.fetch(issue, beta_root, nil, beta_opts)
    end
  end

  # ────────────────────────────────────────────────────────────────
  # Workspace hooks after_run per-project isolation tests
  # ────────────────────────────────────────────────────────────────

  describe "workspace hooks per-project isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-workspace-hooks-#{System.unique_integer([:positive])}"
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

    test "workspace after_run hooks use per-project timeout and hook scripts", %{test_root: root} do
      alpha_root = Path.join(root, "alpha-workspaces")
      beta_root = Path.join(root, "beta-workspaces")

      alpha_trace = Path.join(root, "alpha-after-run-trace")
      beta_trace = Path.join(root, "beta-after-run-trace")

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
            "    \"In Review\": codex",
            "hooks:",
            "  after_run: |",
            "    echo alpha-after-run >> #{alpha_trace}",
            "  timeout_ms: 30000"
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
                "    \"In Review\": codex",
                "hooks:",
                "  after_run: |",
                "    echo beta-after-run >> #{beta_trace}",
                "  timeout_ms: 15000"
              ]
            },
            fn beta_ctx ->
              alpha_settings = Config.settings!(alpha_ctx)
              beta_settings = Config.settings!(beta_ctx)

              assert alpha_settings.hooks.timeout_ms == 30_000
              assert beta_settings.hooks.timeout_ms == 15_000

              issue = %Issue{id: "after-run-issue", identifier: "AFTER-RUN", title: "After run test", state: "Todo"}

              assert {:ok, alpha_ws} = Workspace.create_for_issue(issue, nil, alpha_settings)
              assert {:ok, beta_ws} = Workspace.create_for_issue(issue, nil, beta_settings)

              :ok = Workspace.run_after_run_hook(alpha_ws, issue, nil, alpha_settings)
              :ok = Workspace.run_after_run_hook(beta_ws, issue, nil, beta_settings)

              assert File.read!(alpha_trace) =~ "alpha-after-run"
              assert File.read!(beta_trace) =~ "beta-after-run"
              refute File.read!(alpha_trace) =~ "beta-after-run"
            end
          )
        end
      )
    end

    test "workspace hooks: after_run for one project does not leak to another project's workspace", %{test_root: root} do
      alpha_root = Path.join(root, "alpha-workspaces")
      beta_root = Path.join(root, "beta-workspaces")

      alpha_trace = Path.join(root, "alpha-after-trace-leak")
      beta_trace = Path.join(root, "beta-after-trace-leak")

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
            "    \"In Review\": codex",
            "hooks:",
            "  after_run: |",
            "    echo alpha-after-run >> #{alpha_trace}",
            "  timeout_ms: 10000"
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
                "    \"In Review\": codex",
                "hooks:",
                "  after_run: |",
                "    echo beta-after-run >> #{beta_trace}",
                "  timeout_ms: 10000"
              ]
            },
            fn beta_ctx ->
              alpha_settings = Config.settings!(alpha_ctx)
              beta_settings = Config.settings!(beta_ctx)

              issue = %Issue{id: "leak-test", identifier: "LEAK-AFTER", title: "Leak test", state: "Todo"}

              assert {:ok, alpha_ws} = Workspace.create_for_issue(issue, nil, alpha_settings)
              assert {:ok, _beta_ws} = Workspace.create_for_issue(issue, nil, beta_settings)

              # Run alpha's after_run only
              :ok = Workspace.run_after_run_hook(alpha_ws, issue, nil, alpha_settings)

              # Only alpha's trace file should be written
              assert File.read!(alpha_trace) =~ "alpha-after-run"
              refute File.exists?(beta_trace)
            end
          )
        end
      )
    end
  end

  # ────────────────────────────────────────────────────────────────
  # Workspace sweep and legacy cache per-project isolation tests
  # ────────────────────────────────────────────────────────────────

  describe "workspace sweep and legacy cache per-project isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-workspace-sweep-#{System.unique_integer([:positive])}"
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

    test "sweep_abandoned_runtime_cache only scans and removes from the project's own workspace root", %{test_root: root} do
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

              # Create abandoned workspaces in both project roots
              alpha_abandoned = Path.join(alpha_root, "SYM-ABANDONED-ALPHA")
              beta_abandoned = Path.join(beta_root, "SYM-ABANDONED-BETA")

              # Also create active workspaces
              alpha_active = Path.join(alpha_root, "SYM-ACTIVE-ALPHA")
              beta_active = Path.join(beta_root, "SYM-ACTIVE-BETA")

              Enum.each([alpha_abandoned, beta_abandoned, alpha_active, beta_active], fn path ->
                File.mkdir_p!(path)
                File.write!(Path.join(path, "marker.txt"), Path.basename(path))
              end)

              old_mtime = {{2024, 1, 1}, {0, 0, 0}}
              File.touch!(alpha_abandoned, old_mtime)
              File.touch!(beta_abandoned, old_mtime)

              # Sweep alpha's workspace root with short TTL
              assert {:ok, alpha_removed} =
                       Workspace.sweep_abandoned_runtime_cache(["SYM-ACTIVE-ALPHA"], alpha_settings, 60_000)

              # Only alpha's abandoned workspace should be removed
              assert alpha_abandoned in alpha_removed
              refute File.exists?(alpha_abandoned)
              assert File.exists?(alpha_active)

              # Beta's workspaces should be untouched by alpha's sweep
              assert File.exists?(beta_abandoned)
              assert File.exists?(beta_active)

              # Sweep beta's workspace root
              assert {:ok, beta_removed} =
                       Workspace.sweep_abandoned_runtime_cache(["SYM-ACTIVE-BETA"], beta_settings, 60_000)

              assert beta_abandoned in beta_removed
              refute File.exists?(beta_abandoned)
              assert File.exists?(beta_active)

              # Alpha's active workspace remains untouched by beta's sweep
              assert File.exists?(alpha_active)
            end
          )
        end
      )
    end

    test "remove_legacy_runtime_cache only removes pulse_ledger.json from the project's own workspace root", %{test_root: root} do
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

              # Create pulse_ledger.json in both workspace roots
              alpha_ledger = Path.join(alpha_root, "pulse_ledger.json")
              beta_ledger = Path.join(beta_root, "pulse_ledger.json")

              File.mkdir_p!(alpha_root)
              File.mkdir_p!(beta_root)
              File.write!(alpha_ledger, Jason.encode!(%{"alpha" => "data"}))
              File.write!(beta_ledger, Jason.encode!(%{"beta" => "data"}))

              # Remove legacy cache from alpha only
              assert :ok = Workspace.remove_legacy_runtime_cache(alpha_settings)

              # Alpha's ledger is gone
              refute File.exists?(alpha_ledger)

              # Beta's ledger remains
              assert File.exists?(beta_ledger)

              # Remove legacy cache from beta
              assert :ok = Workspace.remove_legacy_runtime_cache(beta_settings)
              refute File.exists?(beta_ledger)
            end
          )
        end
      )
    end
  end

  # ────────────────────────────────────────────────────────────────
  # Workspace cleanup_issue_runtime_cache per-project isolation tests
  # ────────────────────────────────────────────────────────────────

  describe "workspace cleanup_issue_runtime_cache per-project isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-cleanup-runtime-#{System.unique_integer([:positive])}"
        )

      File.mkdir_p!(test_root)

      Application.put_env(:symphony_elixir, :memory_tracker_recipient, self())

      unless Process.whereis(ProjectRegistry) do
        start_supervised!(ProjectRegistry)
      end

      on_exit(fn ->
        File.rm_rf(test_root)
        Application.delete_env(:symphony_elixir, :memory_tracker_recipient)
        Application.delete_env(:symphony_elixir, :memory_tracker_project_issues)
        Application.delete_env(:symphony_elixir, :memory_tracker_issues)
      end)

      %{test_root: test_root}
    end

    test "cleanup_issue_runtime_cache with project settings only removes that project's workspace", %{test_root: root} do
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

              issue = %Issue{id: "cleanup-test", identifier: "CLEANUP-TEST", title: "Cleanup test", state: "Todo"}

              # Create workspaces for the same issue in both projects
              assert {:ok, alpha_ws} = Workspace.create_for_issue(issue, nil, alpha_settings)
              assert {:ok, beta_ws} = Workspace.create_for_issue(issue, nil, beta_settings)

              assert File.dir?(alpha_ws)
              assert File.dir?(beta_ws)

              # Clean up only alpha's runtime cache
              assert :ok = Workspace.cleanup_issue_runtime_cache(issue, alpha_settings)

              # Alpha's workspace should be removed
              refute File.dir?(alpha_ws)

              # Beta's workspace should still exist
              assert File.dir?(beta_ws)
            end
          )
        end
      )
    end
  end

  # ────────────────────────────────────────────────────────────────
  # Workspace hooks before_remove per-project isolation tests
  # ────────────────────────────────────────────────────────────────

  describe "workspace hooks before_remove per-project isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-before-remove-hooks-#{System.unique_integer([:positive])}"
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

    test "workspace before_remove hooks fire per-project when removing runtime cache", %{test_root: root} do
      alpha_root = Path.join(root, "alpha-workspaces")
      beta_root = Path.join(root, "beta-workspaces")
      alpha_trace = Path.join(root, "alpha-before-remove-trace")
      beta_trace = Path.join(root, "beta-before-remove-trace")

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
            "    \"In Review\": codex",
            "hooks:",
            "  before_remove: |",
            "    echo alpha-before-remove >> #{alpha_trace}",
            "  timeout_ms: 10000"
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
                "    \"In Review\": codex",
                "hooks:",
                "  before_remove: |",
                "    echo beta-before-remove >> #{beta_trace}",
                "  timeout_ms: 10000"
              ]
            },
            fn beta_ctx ->
              alpha_settings = Config.settings!(alpha_ctx)
              beta_settings = Config.settings!(beta_ctx)

              issue = %Issue{id: "before-remove-test", identifier: "BEFORE-REMOVE", title: "Before remove test", state: "Todo"}

              # Create workspaces for the same issue in both projects
              assert {:ok, alpha_ws} = Workspace.create_for_issue(issue, nil, alpha_settings)
              assert {:ok, beta_ws} = Workspace.create_for_issue(issue, nil, beta_settings)

              assert File.dir?(alpha_ws)
              assert File.dir?(beta_ws)

              # Remove alpha's workspace — should fire alpha's before_remove hook only
              assert {:ok, _} = Workspace.remove(alpha_ws, nil, alpha_settings)

              assert File.read!(alpha_trace) =~ "alpha-before-remove"
              refute File.exists?(beta_trace)

              # Now remove beta's workspace
              assert {:ok, _} = Workspace.remove(beta_ws, nil, beta_settings)

              assert File.read!(beta_trace) =~ "beta-before-remove"
            end
          )
        end
      )
    end

    test "workspace before_remove hooks for one project do not fire for another project's workspace removal", %{test_root: root} do
      alpha_root = Path.join(root, "alpha-workspaces")
      beta_root = Path.join(root, "beta-workspaces")
      alpha_trace = Path.join(root, "alpha-before-leak")
      beta_trace = Path.join(root, "beta-before-leak")

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
            "    \"In Review\": codex",
            "hooks:",
            "  before_remove: |",
            "    echo alpha-before-remove >> #{alpha_trace}",
            "  timeout_ms: 10000"
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
                "    \"In Review\": codex",
                "hooks:",
                "  before_remove: |",
                "    echo beta-before-remove >> #{beta_trace}",
                "  timeout_ms: 10000"
              ]
            },
            fn beta_ctx ->
              alpha_settings = Config.settings!(alpha_ctx)
              beta_settings = Config.settings!(beta_ctx)

              issue = %Issue{id: "before-remove-leak", identifier: "BEFORE-LEAK", title: "Before remove leak test", state: "Todo"}

              assert {:ok, alpha_ws} = Workspace.create_for_issue(issue, nil, alpha_settings)
              assert {:ok, _beta_ws} = Workspace.create_for_issue(issue, nil, beta_settings)

              # Remove alpha's workspace only
              assert {:ok, _} = Workspace.remove(alpha_ws, nil, alpha_settings)

              # Only alpha's before_remove hook should have fired
              assert File.read!(alpha_trace) =~ "alpha-before-remove"
              refute File.exists?(beta_trace)
            end
          )
        end
      )
    end
  end

  # ────────────────────────────────────────────────────────────────
  # Config Codex settings per-project isolation tests
  # ────────────────────────────────────────────────────────────────
end
