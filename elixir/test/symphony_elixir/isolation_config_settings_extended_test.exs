defmodule SymphonyElixir.IsolationRegression.ConfigSettingsExtendedTest do
  use ExUnit.Case, async: false

  import SymphonyElixir.IsolationRegressionSupport
  alias SymphonyElixir.Config
  alias SymphonyElixir.ProjectRegistry

  describe "config server settings per-project isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-server-settings-#{System.unique_integer([:positive])}"
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

    test "Config.settings!/1 returns per-project server.port", %{test_root: root} do
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
            "server:",
            "  port: 4001"
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
                "server:",
                "  port: 4002"
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).server.port == 4001
              assert Config.settings!(beta_ctx).server.port == 4002
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project server.host", %{test_root: root} do
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
            "server:",
            "  host: 0.0.0.0",
            "  port: 4001"
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
                "server:",
                "  host: 127.0.0.1",
                "  port: 4002"
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).server.host == "0.0.0.0"
              assert Config.settings!(beta_ctx).server.host == "127.0.0.1"
            end
          )
        end
      )
    end

    test "Config.settings!/1 uses default server.host per project when unset", %{test_root: root} do
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
            "server:",
            "  port: 5000"
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
              assert Config.settings!(alpha_ctx).server.host == "127.0.0.1"
              assert Config.settings!(alpha_ctx).server.port == 5000
              assert Config.settings!(beta_ctx).server.port == nil
            end
          )
        end
      )
    end
  end

  # ────────────────────────────────────────────────────────────────
  # Config Stewardship settings per-project isolation tests
  # ────────────────────────────────────────────────────────────────

  describe "config stewardship settings per-project isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-stewardship-settings-#{System.unique_integer([:positive])}"
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

    test "Config.settings!/1 returns per-project stewardship.active_milestone_id", %{test_root: root} do
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
            "stewardship:",
            "  active_milestone_id: milestone-alpha-1",
            "  active_milestone_name: \"Alpha Phase 1\""
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
                "stewardship:",
                "  active_milestone_id: milestone-beta-2",
                "  active_milestone_name: \"Beta Phase 2\""
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).stewardship.active_milestone_id == "milestone-alpha-1"
              assert Config.settings!(alpha_ctx).stewardship.active_milestone_name == "Alpha Phase 1"

              assert Config.settings!(beta_ctx).stewardship.active_milestone_id == "milestone-beta-2"
              assert Config.settings!(beta_ctx).stewardship.active_milestone_name == "Beta Phase 2"
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns nil stewardship values when unset", %{test_root: root} do
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
          assert Config.settings!(alpha_ctx).stewardship.active_milestone_id == nil
          assert Config.settings!(alpha_ctx).stewardship.active_milestone_name == nil
        end
      )
    end
  end

  # ────────────────────────────────────────────────────────────────
  # Config Polling full_interval_ms and fast_states per-project isolation tests
  # ────────────────────────────────────────────────────────────────

  describe "config polling extended settings per-project isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-polling-ext-#{System.unique_integer([:positive])}"
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

    test "Config.settings!/1 returns per-project polling.full_interval_ms", %{test_root: root} do
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
            "polling:",
            "  full_interval_ms: 120000"
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
                "polling:",
                "  full_interval_ms: 300000"
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).polling.full_interval_ms == 120_000
              assert Config.settings!(beta_ctx).polling.full_interval_ms == 300_000
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project polling.fast_states", %{test_root: root} do
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
            "polling:",
            "  fast_states:",
            "    - Todo",
            "    - \"Need Owner Input\""
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
                "polling:",
                "  fast_states:",
                "    - \"In Progress\""
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).polling.fast_states == ["Todo", "Need Owner Input"]
              assert Config.settings!(beta_ctx).polling.fast_states == ["In Progress"]
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns default polling.fast_states per project when unset", %{test_root: root} do
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
            "polling:",
            "  fast_states:",
            "    - \"Critical\""
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
              assert Config.settings!(alpha_ctx).polling.fast_states == ["Critical"]
              assert Config.settings!(beta_ctx).polling.fast_states == ["Todo", "Preparing", "Need Owner Input"]
            end
          )
        end
      )
    end
  end

  # ────────────────────────────────────────────────────────────────
  # Config Runner routes comprehensive per-project isolation tests
  # ────────────────────────────────────────────────────────────────

  describe "config runner routes comprehensive per-project isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-runner-routes-full-#{System.unique_integer([:positive])}"
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

    test "Config.settings!/1 returns per-project runner.routes with mixed codex/opencode targets", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "runner:",
            "  default: opencode",
            "  routes:",
            "    \"RCA Required\": codex",
            "    \"In Review\": codex",
            "opencode:",
            "  agent: build",
            "  format: json",
            "  result_state: \"In Review\""
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
                "    \"Todo\": opencode",
                "    \"RCA Required\": codex",
                "opencode:",
                "  agent: build",
                "  format: json",
                "  result_state: \"In Review\""
              ]
            },
            fn beta_ctx ->
              alpha_settings = Config.settings!(alpha_ctx)
              beta_settings = Config.settings!(beta_ctx)

              # Alpha: default=opencode, RCA Required route=codex
              assert alpha_settings.runner.default == "opencode"
              assert alpha_settings.runner.routes["rca required"] == "codex"
              assert alpha_settings.runner.routes["in review"] == "codex"

              # Beta: default=codex, Todo route=opencode
              assert beta_settings.runner.default == "codex"
              assert beta_settings.runner.routes["todo"] == "opencode"
              assert beta_settings.runner.routes["rca required"] == "codex"
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project runner.routes when only defaults differ", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "runner:",
            "  default: codex",
            "  routes:",
            "    \"Todo\": opencode",
            "    \"In Progress\": opencode",
            "opencode:",
            "  agent: build",
            "  format: json",
            "  result_state: \"In Review\""
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
                "    \"Todo\": codex",
                "    \"In Progress\": codex"
              ]
            },
            fn beta_ctx ->
              alpha_settings = Config.settings!(alpha_ctx)
              beta_settings = Config.settings!(beta_ctx)

              assert alpha_settings.runner.routes["todo"] == "opencode"
              assert alpha_settings.runner.routes["in progress"] == "opencode"

              assert beta_settings.runner.routes["todo"] == "codex"
              assert beta_settings.runner.routes["in progress"] == "codex"
            end
          )
        end
      )
    end

    test "Runner dispatch routing resolves per-project routes correctly", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "runner:",
            "  default: codex",
            "  routes:",
            "    \"Todo\": opencode",
            "    \"RCA Required\": codex",
            "opencode:",
            "  agent: build",
            "  format: json",
            "  result_state: \"In Review\""
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
                "opencode:",
                "  agent: build",
                "  format: json",
                "  result_state: \"In Review\""
              ]
            },
            fn beta_ctx ->
              alpha_settings = Config.settings!(alpha_ctx)
              beta_settings = Config.settings!(beta_ctx)

              # Alpha: Todo routes to opencode (due to route), In Progress defaults to codex
              assert alpha_settings.runner.default == "codex"
              assert alpha_settings.runner.routes["todo"] == "opencode"

              # Beta: Todo defaults to opencode (default=opencode), In Progress defaults to opencode
              assert beta_settings.runner.default == "opencode"
              assert beta_settings.runner.routes["todo"] == nil
            end
          )
        end
      )
    end
  end

  # ────────────────────────────────────────────────────────────────
  # Config Tracker extended settings per-project isolation tests
  # ────────────────────────────────────────────────────────────────

  describe "config tracker extended settings per-project isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-tracker-ext-#{System.unique_integer([:positive])}"
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

    test "Config.settings!/1 returns per-project tracker.active_states", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "  active_states:",
            "    - Todo",
            "    - \"In Progress\"",
            "    - \"Need Review\""
          ]
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              front_matter_overrides: [
                "  active_states:",
                "    - Todo",
                "    - \"In Progress\""
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).tracker.active_states == ["Todo", "In Progress", "Need Review"]
              assert Config.settings!(beta_ctx).tracker.active_states == ["Todo", "In Progress"]
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project tracker.terminal_states", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "  terminal_states:",
            "    - Closed",
            "    - Done",
            "    - Cancelled"
          ]
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              front_matter_overrides: [
                "  terminal_states:",
                "    - Done",
                "    - WontFix"
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).tracker.terminal_states == ["Closed", "Done", "Cancelled"]
              assert Config.settings!(beta_ctx).tracker.terminal_states == ["Done", "WontFix"]
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project tracker.assignee", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "  assignee: alpha-user"
          ]
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              front_matter_overrides: [
                "  assignee: beta-user"
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).tracker.assignee == "alpha-user"
              assert Config.settings!(beta_ctx).tracker.assignee == "beta-user"
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project tracker.endpoint", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "  endpoint: https://alpha.linear.app/graphql"
          ]
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              front_matter_overrides: [
                "  endpoint: https://beta.linear.app/graphql"
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).tracker.endpoint == "https://alpha.linear.app/graphql"
              assert Config.settings!(beta_ctx).tracker.endpoint == "https://beta.linear.app/graphql"
            end
          )
        end
      )
    end
  end
end
