defmodule SymphonyElixir.IsolationRegression.ConfigRetryOpencodeTest do
  use ExUnit.Case, async: false

  import SymphonyElixir.IsolationRegressionSupport
  alias SymphonyElixir.Config

  describe "config agent.max_retry_backoff_ms per-project isolation" do
    @describetag :config_max_retry_backoff_ms_isolation

    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-config-backoff-#{System.unique_integer([:positive])}"
        )

      File.mkdir_p!(test_root)

      on_exit(fn ->
        File.rm_rf(test_root)
      end)

      %{test_root: test_root}
    end

    test "Config.settings!/1 returns per-project agent.max_retry_backoff_ms", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "agent:",
            "  max_retry_backoff_ms: 5000",
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
                "agent:",
                "  max_retry_backoff_ms: 15000",
                "  format: json",
                "  result_state: \"In Review\""
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).agent.max_retry_backoff_ms == 5_000
              assert Config.settings!(beta_ctx).agent.max_retry_backoff_ms == 15_000
            end
          )
        end
      )
    end

    test "Config.settings!/1 uses default for agent.max_retry_backoff_ms when unset", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "agent:",
            "  format: json",
            "  result_state: \"In Review\""
          ]
        },
        fn _alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              front_matter_overrides: [
                "agent:",
                "  max_retry_backoff_ms: 20000",
                "  format: json",
                "  result_state: \"In Review\""
              ]
            },
            fn beta_ctx ->
              # alpha does not set max_retry_backoff_ms, uses default
              # beta sets it to 20000
              assert Config.settings!(beta_ctx).agent.max_retry_backoff_ms == 20_000
            end
          )
        end
      )
    end
  end

  describe "config opencode per-project settings isolation" do
    @describetag :config_opencode_settings_isolation

    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-config-opencode-#{System.unique_integer([:positive])}"
        )

      File.mkdir_p!(test_root)

      on_exit(fn ->
        File.rm_rf(test_root)
      end)

      %{test_root: test_root}
    end

    test "Config.settings!/1 returns per-project opencode.args", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "opencode:",
            "  args:",
            "    - --verbose",
            "    - --timeout",
            "    - \"60\"",
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
                "opencode:",
                "  args:",
                "    - --quiet",
                "  agent: build",
                "  format: json",
                "  result_state: \"In Review\""
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).opencode.args == ["--verbose", "--timeout", "60"]
              assert Config.settings!(beta_ctx).opencode.args == ["--quiet"]
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project opencode.model", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "opencode:",
            "  model: gpt-4",
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
                "opencode:",
                "  model: claude-3",
                "  agent: build",
                "  format: json",
                "  result_state: \"In Review\""
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).opencode.model == "gpt-4"
              assert Config.settings!(beta_ctx).opencode.model == "claude-3"
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project opencode.read_timeout_ms and stall_timeout_ms", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "opencode:",
            "  read_timeout_ms: 10000",
            "  stall_timeout_ms: 60000",
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
                "opencode:",
                "  read_timeout_ms: 30000",
                "  stall_timeout_ms: 120000",
                "  agent: build",
                "  format: json",
                "  result_state: \"In Review\""
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).opencode.read_timeout_ms == 10_000
              assert Config.settings!(alpha_ctx).opencode.stall_timeout_ms == 60_000
              assert Config.settings!(beta_ctx).opencode.read_timeout_ms == 30_000
              assert Config.settings!(beta_ctx).opencode.stall_timeout_ms == 120_000
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project opencode.permission_policy", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "opencode:",
            "  permission_policy: cancel",
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
                "opencode:",
                "  permission_policy: reject",
                "  agent: build",
                "  format: json",
                "  result_state: \"In Review\""
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).opencode.permission_policy == "cancel"
              assert Config.settings!(beta_ctx).opencode.permission_policy == "reject"
            end
          )
        end
      )
    end

    test "Config.settings!/1 uses defaults for opencode.args, model, permission_policy when unset", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
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
                "opencode:",
                "  permission_policy: cancel",
                "  agent: build",
                "  format: json",
                "  result_state: \"In Review\""
              ]
            },
            fn beta_ctx ->
              # args defaults to nil (no default in schema)
              assert Config.settings!(alpha_ctx).opencode.args == nil

              # model defaults to nil
              assert Config.settings!(alpha_ctx).opencode.model == nil

              # permission_policy defaults to "reject"
              assert Config.settings!(alpha_ctx).opencode.permission_policy == "reject"
              assert Config.settings!(beta_ctx).opencode.permission_policy == "cancel"
            end
          )
        end
      )
    end
  end
end
