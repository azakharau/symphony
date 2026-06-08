defmodule SymphonyElixir.IsolationRegression.RunnerConfigErrorsTest do
  use ExUnit.Case, async: false

  import SymphonyElixir.IsolationRegressionSupport
  alias SymphonyElixir.Config
  alias SymphonyElixir.ProjectContext
  alias SymphonyElixir.ProjectRegistry
  alias SymphonyElixir.Workflow
  alias SymphonyElixir.WorkflowStore

  # ────────────────────────────────────────────────────────────────
  # Runner settings per-project isolation tests
  # ────────────────────────────────────────────────────────────────

  describe "runner settings per-project isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-runner-#{System.unique_integer([:positive])}"
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

    test "Config.settings!/1 returns per-project runner.default with codex routes for validation", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "runner:",
            "  default: opencode",
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
                "runner:",
                "  default: codex",
                "  routes:",
                "    \"RCA Required\": codex",
                "    \"In Review\": codex"
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).runner.default == "opencode"
              assert Config.settings!(beta_ctx).runner.default == "codex"
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project runner.routes", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "runner:",
            "  default: codex",
            "  routes:",
            "    \"In Progress\": opencode",
            "    \"In Review\": codex"
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
                "    \"Todo\": codex",
                "    \"RCA Required\": codex",
                "    \"In Review\": codex"
              ]
            },
            fn beta_ctx ->
              alpha_settings = Config.settings!(alpha_ctx)
              beta_settings = Config.settings!(beta_ctx)

              assert alpha_settings.runner.default == "codex"
              assert beta_settings.runner.default == "opencode"

              assert alpha_settings.runner.routes["in progress"] == "opencode"
              assert alpha_settings.runner.routes["in review"] == "codex"
              refute Map.has_key?(alpha_settings.runner.routes, "todo")

              assert beta_settings.runner.routes["todo"] == "codex"
              refute Map.has_key?(beta_settings.runner.routes, "in progress")
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project codex settings", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "codex:",
            "  command: alpha-codex",
            "  project_root: /tmp/alpha-codex",
            "  turn_timeout_ms: 500000",
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
                "codex:",
                "  command: beta-codex",
                "  project_root: /tmp/beta-codex",
                "  turn_timeout_ms: 999999",
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

              assert alpha_settings.codex.command == "alpha-codex"
              assert beta_settings.codex.command == "beta-codex"
              assert alpha_settings.codex.project_root == "/tmp/alpha-codex"
              assert beta_settings.codex.project_root == "/tmp/beta-codex"
              assert alpha_settings.codex.turn_timeout_ms == 500_000
              assert beta_settings.codex.turn_timeout_ms == 999_999
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project opencode settings", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "runner:",
            "  default: codex",
            "  routes:",
            "    \"RCA Required\": codex",
            "    \"In Review\": codex",
            "opencode:",
            "  command: alpha-oc",
            "  agent: build",
            "  result_state: Needs Review"
          ]
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              front_matter_overrides: [
                "runner:",
                "  default: codex",
                "  routes:",
                "    \"RCA Required\": codex",
                "    \"In Review\": codex",
                "opencode:",
                "  command: beta-oc",
                "  agent: architect",
                "  result_state: Human Review"
              ]
            },
            fn beta_ctx ->
              alpha_settings = Config.settings!(alpha_ctx)
              beta_settings = Config.settings!(beta_ctx)

              assert alpha_settings.opencode.command == "alpha-oc"
              assert beta_settings.opencode.command == "beta-oc"
              assert alpha_settings.opencode.agent == "build"
              assert beta_settings.opencode.agent == "architect"
              assert alpha_settings.opencode.result_state == "Needs Review"
              assert beta_settings.opencode.result_state == "Human Review"
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project process_policy settings", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "runner:",
            "  default: codex",
            "  routes:",
            "    \"RCA Required\": codex",
            "    \"In Review\": codex",
            "process_policy:",
            "  rca_required_state: RCA Required",
            "  max_rejections_per_slice: 1"
          ]
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              front_matter_overrides: [
                "runner:",
                "  default: codex",
                "  routes:",
                "    \"RCA Required\": codex",
                "    \"In Review\": codex",
                "process_policy:",
                "  rca_required_state: RCA Required",
                "  max_rejections_per_slice: 5"
              ]
            },
            fn beta_ctx ->
              alpha_settings = Config.settings!(alpha_ctx)
              beta_settings = Config.settings!(beta_ctx)

              assert alpha_settings.process_policy.rca_required_state == "RCA Required"
              assert beta_settings.process_policy.rca_required_state == "RCA Required"
              assert alpha_settings.process_policy.max_rejections_per_slice == 1
              assert beta_settings.process_policy.max_rejections_per_slice == 5
            end
          )
        end
      )
    end
  end

  # ────────────────────────────────────────────────────────────────
  # Config settings error isolation tests
  # ────────────────────────────────────────────────────────────────

  describe "config settings error isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-config-error-#{System.unique_integer([:positive])}"
        )

      File.mkdir_p!(test_root)

      unless Process.whereis(ProjectRegistry) do
        start_supervised!(ProjectRegistry)
      end

      on_exit(fn ->
        File.rm_rf(test_root)
        Application.delete_env(:symphony_elixir, :memory_tracker_recipient)
      end)

      %{test_root: test_root}
    end

    test "stopping one project's WorkflowStore does not affect Config.settings!/1 for another project", %{
      test_root: root
    } do
      Application.put_env(:symphony_elixir, :memory_tracker_recipient, self())

      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "runner:",
            "  default: codex",
            "  routes:",
            "    \"RCA Required\": codex",
            "    \"In Review\": codex",
            "agent:",
            "  max_concurrent_agents: 5"
          ]
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              front_matter_overrides: [
                "runner:",
                "  default: codex",
                "  routes:",
                "    \"RCA Required\": codex",
                "    \"In Review\": codex",
                "agent:",
                "  max_concurrent_agents: 10"
              ]
            },
            fn beta_ctx ->
              # Both should work initially
              assert Config.settings!(alpha_ctx).agent.max_concurrent_agents == 5
              assert Config.settings!(beta_ctx).agent.max_concurrent_agents == 10

              # Stop alpha's WorkflowStore via its supervised id
              alpha_store_name = ProjectRegistry.via_name(alpha_ctx.process_names.workflow_store)
              stop_supervised!({WorkflowStore, "alpha"})

              # Alpha's store process is dead
              assert GenServer.whereis(alpha_store_name) == nil

              # Beta's store is still alive and returns correct data
              assert Config.settings!(beta_ctx).agent.max_concurrent_agents == 10
            end
          )
        end
      )
    end

    test "stopping one project's WorkflowStore does not affect Config.workflow_prompt/1 for another project", %{
      test_root: root
    } do
      with_custom_project_context(
        "alpha",
        root,
        %{
          prompt_body: "Alpha exclusive prompt"
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              prompt_body: "Beta exclusive prompt"
            },
            fn beta_ctx ->
              assert Config.workflow_prompt(alpha_ctx) =~ "Alpha exclusive prompt"
              assert Config.workflow_prompt(beta_ctx) =~ "Beta exclusive prompt"

              # Stop beta's WorkflowStore via its supervised id
              beta_store_name = ProjectRegistry.via_name(beta_ctx.process_names.workflow_store)
              stop_supervised!({WorkflowStore, "beta"})

              # Beta's store process is dead
              assert GenServer.whereis(beta_store_name) == nil

              # Alpha should still work
              assert Config.workflow_prompt(alpha_ctx) =~ "Alpha exclusive prompt"

              # Note: after the store is dead, WorkflowStore.current falls back
              # to Workflow.load() which reads the real project WORKFLOW.md,
              # not the per-project test file. So we do not assert prompt content
              # for the stopped project.
            end
          )
        end
      )
    end

    test "one project's invalid workflow front matter does not affect another project's Config.settings!/1", %{
      test_root: root
    } do
      # Write invalid workflow for alpha (unclosed flow collection)
      alpha_path = Path.join([root, "alpha-invalid", "WORKFLOW.md"])
      File.mkdir_p!(Path.dirname(alpha_path))
      File.write!(alpha_path, "---\ninvalid_yaml: [unclosed\n---\nfallback prompt\n")

      # Verify that alpha's invalid YAML truly fails to parse
      assert {:error, _} = Workflow.load(alpha_path)

      # Attempting to start a WorkflowStore for alpha should fail
      # because the workflow file has invalid YAML front matter
      assert {:error, _reason} =
               start_supervised(
                 {WorkflowStore, [name: :alpha_failing_store_test, workflow_path: alpha_path]},
                 id: :alpha_failing_store_test
               )

      # Write valid workflow for beta
      beta_path = Path.join([root, "beta-valid", "WORKFLOW.md"])
      File.mkdir_p!(Path.dirname(beta_path))
      File.write!(beta_path, "---\nrunner:\n  default: codex\n  routes:\n    \"RCA Required\": codex\n    \"In Review\": codex\n---\nbeta prompt\n")

      beta_ctx =
        ProjectContext.new(%{
          id: "beta-valid",
          enabled: true,
          workflow_path: beta_path
        })

      beta_store_name = ProjectRegistry.via_name(beta_ctx.process_names.workflow_store)

      start_supervised!(
        {WorkflowStore, [name: beta_store_name, workflow_path: beta_path]},
        id: :valid_workflow_beta_store
      )

      # Beta is unaffected — its workflow parses and loads correctly
      assert Config.workflow_prompt(beta_ctx) =~ "beta prompt"
      assert Config.settings!(beta_ctx).runner.default == "codex"
    end
  end

  # ────────────────────────────────────────────────────────────────
end
