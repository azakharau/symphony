defmodule SymphonyElixir.IsolationRegression.ProcessWorkspaceTest do
  use ExUnit.Case, async: false

  import SymphonyElixir.IsolationRegressionSupport
  alias SymphonyElixir.Config
  alias SymphonyElixir.Linear.Issue
  alias SymphonyElixir.ProcessPolicy
  alias SymphonyElixir.ProjectRegistry
  alias SymphonyElixir.Workspace

  describe "process_policy per-project isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-process-policy-#{System.unique_integer([:positive])}"
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

    test "codex_owned_rca_required_state returns per-project value", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "process_policy:",
            "  rca_required_state: \"Need RCA\""
          ]
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              front_matter_overrides: [
                "process_policy:",
                "  rca_required_state: \"Analysis Pending\""
              ]
            },
            fn beta_ctx ->
              assert {:ok, "Need RCA"} = ProcessPolicy.codex_owned_rca_required_state(alpha_ctx)
              assert {:ok, "Analysis Pending"} = ProcessPolicy.codex_owned_rca_required_state(beta_ctx)
            end
          )
        end
      )
    end

    test "opencode_dispatch_decision uses per-project max_rejections_per_slice", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "process_policy:",
            "  rca_required_state: \"RCA Required\"",
            "  max_rejections_per_slice: 1"
          ]
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              front_matter_overrides: [
                "process_policy:",
                "  rca_required_state: \"RCA Required\"",
                "  max_rejections_per_slice: 5"
              ]
            },
            fn beta_ctx ->
              packet = %SymphonyElixir.OpenCode.TaskPrompt.Packet{
                slice_id: "slice-1",
                prompt: "test",
                fingerprint: "fp"
              }

              # With max_rejections_per_slice=1, a single rejected decision blocks alpha
              decisions = [
                %SymphonyElixir.ReviewDecision{
                  status: "rejected",
                  slice_id: "slice-1",
                  reason: "needs work"
                }
              ]

              # With max_rejections_per_slice=1 and one rejection, alpha blocks; beta allows
              result_alpha = ProcessPolicy.opencode_dispatch_decision(packet, decisions, alpha_ctx)
              result_beta = ProcessPolicy.opencode_dispatch_decision(packet, decisions, beta_ctx)

              assert match?({:block, _}, result_alpha)
              assert result_alpha |> elem(1) |> Map.get(:rejection_count) == 1
              assert result_alpha |> elem(1) |> Map.get(:reason) == :repair_loop_breaker

              assert result_beta == :allow
            end
          )
        end
      )
    end

    test "opencode_dispatch_decision block carries per-project rca_required_state", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "process_policy:",
            "  rca_required_state: \"Special Review\"",
            "  max_rejections_per_slice: 1"
          ]
        },
        fn alpha_ctx ->
          packet = %SymphonyElixir.OpenCode.TaskPrompt.Packet{
            slice_id: "slice-rca",
            prompt: "test",
            fingerprint: "fp"
          }

          decisions = [
            %SymphonyElixir.ReviewDecision{
              status: "rejected",
              slice_id: "slice-rca",
              reason: "fix needed"
            }
          ]

          result = ProcessPolicy.opencode_dispatch_decision(packet, decisions, alpha_ctx)

          assert match?({:block, _}, result)
          assert result |> elem(1) |> Map.get(:rca_required_state) == "Special Review"
        end
      )
    end
  end

  # ────────────────────────────────────────────────────────────────
  # Workspace per-project isolation tests
  # ────────────────────────────────────────────────────────────────

  describe "workspace per-project isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-workspace-#{System.unique_integer([:positive])}"
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

    test "workspace paths use per-project workspace roots", %{test_root: root} do
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

              assert alpha_settings.workspace.root == alpha_root
              assert beta_settings.workspace.root == beta_root
              refute String.starts_with?(alpha_root, beta_root)
              refute String.starts_with?(beta_root, alpha_root)

              issue = %Issue{id: "same-issue", identifier: "SYM-X", title: "Same ID", state: "Todo"}

              assert {:ok, alpha_ws} = Workspace.create_for_issue(issue, nil, alpha_settings)
              assert {:ok, beta_ws} = Workspace.create_for_issue(issue, nil, beta_settings)

              assert String.starts_with?(alpha_ws, alpha_root)
              assert String.starts_with?(beta_ws, beta_root)
              refute String.starts_with?(alpha_ws, beta_root)
              refute String.starts_with?(beta_ws, alpha_root)
            end
          )
        end
      )
    end

    test "workspace hooks use per-project timeout and hook scripts", %{test_root: root} do
      alpha_root = Path.join(root, "alpha-workspaces")
      beta_root = Path.join(root, "beta-workspaces")

      alpha_trace = Path.join(root, "alpha-hook-trace")
      beta_trace = Path.join(root, "beta-hook-trace")

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
            "  before_run: |",
            "    echo alpha-hook-run >> #{alpha_trace}",
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
                "  before_run: |",
                "    echo beta-hook-run >> #{beta_trace}",
                "  timeout_ms: 15000"
              ]
            },
            fn beta_ctx ->
              alpha_settings = Config.settings!(alpha_ctx)
              beta_settings = Config.settings!(beta_ctx)

              assert alpha_settings.hooks.timeout_ms == 30_000
              assert beta_settings.hooks.timeout_ms == 15_000

              issue = %Issue{id: "hook-issue", identifier: "HOOK-1", title: "Hook test", state: "Todo"}

              assert {:ok, alpha_ws} = Workspace.create_for_issue(issue, nil, alpha_settings)
              assert {:ok, beta_ws} = Workspace.create_for_issue(issue, nil, beta_settings)

              :ok = Workspace.run_before_run_hook(alpha_ws, issue, nil, alpha_settings)
              :ok = Workspace.run_before_run_hook(beta_ws, issue, nil, beta_settings)

              assert File.read!(alpha_trace) =~ "alpha-hook-run"
              assert File.read!(beta_trace) =~ "beta-hook-run"
              refute File.read!(alpha_trace) =~ "beta-hook-run"
            end
          )
        end
      )
    end

    test "workspace path safety rejects paths outside per-project workspace root", %{test_root: root} do
      alpha_root = Path.join(root, "alpha-workspaces")
      File.mkdir_p!(alpha_root)

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
          alpha_settings = Config.settings!(alpha_ctx)

          # safe_identifier converts ../outside-EVIL to .._outside-EVIL (flat dir name),
          # so no path traversal is possible and workspace creation succeeds
          assert {:ok, workspace} =
                   Workspace.create_for_issue(
                     %Issue{id: "escape", identifier: "../outside-EVIL", title: "Escape", state: "Todo"},
                     nil,
                     alpha_settings
                   )

          # Verify workspace is inside the expected root
          assert String.starts_with?(workspace, alpha_root <> "/")
          assert String.ends_with?(workspace, ".._outside-EVIL")
        end
      )
    end

    test "workspace keys are sanitized consistently across projects", %{test_root: root} do
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

              issue = %Issue{id: "speci@l", identifier: "SPECIAL-1", title: "Special chars", state: "Todo"}

              assert {:ok, alpha_ws} = Workspace.create_for_issue(issue, nil, alpha_settings)
              assert {:ok, beta_ws} = Workspace.create_for_issue(issue, nil, beta_settings)

              assert alpha_ws =~ "SPECIAL-1"
              assert beta_ws =~ "SPECIAL-1"
              assert String.starts_with?(alpha_ws, alpha_root)
              assert String.starts_with?(beta_ws, beta_root)
            end
          )
        end
      )
    end
  end
end
