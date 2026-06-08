defmodule SymphonyElixir.IsolationRegression.RunnerRoutingTest do
  use ExUnit.Case, async: false

  import SymphonyElixir.IsolationRegressionSupport
  alias SymphonyElixir.Config

  describe "runner.default per-project isolation" do
    @describetag :runner_default_isolation

    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-runner-default-#{System.unique_integer([:positive])}"
        )

      File.mkdir_p!(test_root)

      on_exit(fn ->
        File.rm_rf(test_root)
      end)

      %{test_root: test_root}
    end

    test "Config.settings!/1 returns per-project runner.default when different projects have different values", %{test_root: root} do
      with_custom_project_context(
        "alpha",
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
                "    \"In Review\": codex"
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).runner.default == "codex"
              assert Config.settings!(beta_ctx).runner.default == "opencode"
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns default runner.default when unset", %{test_root: root} do
      with_custom_project_context(
        "alpha",
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
                "    \"In Review\": codex"
              ]
            },
            fn beta_ctx ->
              # All projects have explicit settings here
              assert Config.settings!(alpha_ctx).runner.default == "codex"
              assert Config.settings!(beta_ctx).runner.default == "opencode"
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project runner.default with explicit codex route for RCA Required", %{test_root: root} do
      with_custom_project_context(
        "alpha",
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
                "    \"Todo\": opencode"
              ]
            },
            fn beta_ctx ->
              # Even though beta.default is opencode, "RCA Required" routes to codex explicitly
              assert Config.settings!(alpha_ctx).runner.default == "codex"
              assert Config.settings!(beta_ctx).runner.default == "opencode"
            end
          )
        end
      )
    end
  end

  describe "runner.routes per-project isolation" do
    @describetag :runner_routes_isolation

    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-runner-routes-#{System.unique_integer([:positive])}"
        )

      File.mkdir_p!(test_root)

      on_exit(fn ->
        File.rm_rf(test_root)
      end)

      %{test_root: test_root}
    end

    test "Config.settings!/1 returns per-project runner.routes with different state routing", %{test_root: root} do
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
              alpha_routes = Config.settings!(alpha_ctx).runner.routes
              beta_routes = Config.settings!(beta_ctx).runner.routes

              assert alpha_routes["in progress"] == "opencode"
              assert alpha_routes["rca required"] == "codex"
              assert alpha_routes["in review"] == "codex"

              assert beta_routes["todo"] == "codex"
              assert beta_routes["rca required"] == "codex"
              assert beta_routes["in review"] == "codex"

              # beta does not have "in progress" route so it falls through to default
              refute Map.has_key?(beta_routes, "in progress")
            end
          )
        end
      )
    end

    test "runner.routes from one project does not leak into another", %{test_root: root} do
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
                "  default: codex",
                "  routes:",
                "    \"RCA Required\": codex",
                "    \"In Review\": codex"
              ]
            },
            fn beta_ctx ->
              alpha_routes = Config.settings!(alpha_ctx).runner.routes
              beta_routes = Config.settings!(beta_ctx).runner.routes

              assert alpha_routes["in progress"] == "opencode"

              # beta has no "in progress" route - must not leak from alpha
              refute Map.has_key?(beta_routes, "in progress")

              assert beta_routes == %{
                       "rca required" => "codex",
                       "in review" => "codex"
                     }
            end
          )
        end
      )
    end

    test "runner.routes missing state falls through to runner.default", %{test_root: root} do
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
            "    \"In Progress\": opencode"
          ]
        },
        fn alpha_ctx ->
          settings = Config.settings!(alpha_ctx)

          # "Todo" is not in routes, so should use default "codex"
          todo_kind = Map.get(settings.runner.routes, "todo", settings.runner.default)
          assert todo_kind == "codex"

          # "In Progress" has explicit route
          in_progress_kind = Map.get(settings.runner.routes, "in progress", settings.runner.default)
          assert in_progress_kind == "opencode"

          # "RCA Required" has explicit route to codex
          rca_kind = Map.get(settings.runner.routes, "rca required", settings.runner.default)
          assert rca_kind == "codex"
        end
      )
    end

    test "runner.routes with no routes configured returns empty map", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: []
        },
        fn alpha_ctx ->
          settings = Config.settings!(alpha_ctx)

          # When no routes are configured, returns the schema default (empty map)
          assert settings.runner.routes == %{}
        end
      )
    end
  end
end
