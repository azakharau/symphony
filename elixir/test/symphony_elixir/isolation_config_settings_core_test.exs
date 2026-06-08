defmodule SymphonyElixir.IsolationRegression.ConfigSettingsCoreTest do
  use ExUnit.Case, async: false

  import SymphonyElixir.IsolationRegressionSupport
  alias SymphonyElixir.Config
  alias SymphonyElixir.ProjectRegistry

  describe "config codex settings per-project isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-codex-settings-#{System.unique_integer([:positive])}"
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

    test "Config.settings!/1 returns per-project codex.command", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "codex:",
            "  command: alpha-codex-command"
          ]
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              front_matter_overrides: [
                "codex:",
                "  command: beta-codex-cmd"
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).codex.command == "alpha-codex-command"
              assert Config.settings!(beta_ctx).codex.command == "beta-codex-cmd"
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project codex.project_root", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "codex:",
            "  project_root: \"#{Path.join(root, "alpha-codex")}\""
          ]
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              front_matter_overrides: [
                "codex:",
                "  project_root: \"#{Path.join(root, "beta-codex")}\""
              ]
            },
            fn beta_ctx ->
              alpha_root = Config.settings!(alpha_ctx).codex.project_root
              beta_root = Config.settings!(beta_ctx).codex.project_root

              assert alpha_root =~ "alpha-codex"
              assert beta_root =~ "beta-codex"
              refute alpha_root =~ "beta-codex"
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project codex.thread_sandbox", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "codex:",
            "  thread_sandbox: workspace-read"
          ]
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              front_matter_overrides: [
                "codex:",
                "  thread_sandbox: none"
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).codex.thread_sandbox == "workspace-read"
              assert Config.settings!(beta_ctx).codex.thread_sandbox == "none"
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project codex.turn_timeout_ms", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "codex:",
            "  turn_timeout_ms: 1800000"
          ]
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              front_matter_overrides: [
                "codex:",
                "  turn_timeout_ms: 7200000"
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).codex.turn_timeout_ms == 1_800_000
              assert Config.settings!(beta_ctx).codex.turn_timeout_ms == 7_200_000
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project codex.read_timeout_ms and stall_timeout_ms", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "codex:",
            "  read_timeout_ms: 3000",
            "  stall_timeout_ms: 60000"
          ]
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              front_matter_overrides: [
                "codex:",
                "  read_timeout_ms: 10000",
                "  stall_timeout_ms: 120000"
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).codex.read_timeout_ms == 3_000
              assert Config.settings!(alpha_ctx).codex.stall_timeout_ms == 60_000

              assert Config.settings!(beta_ctx).codex.read_timeout_ms == 10_000
              assert Config.settings!(beta_ctx).codex.stall_timeout_ms == 120_000
            end
          )
        end
      )
    end

    test "Config.resolve_turn_sandbox_policy/2 uses per-project workspace root", %{test_root: root} do
      alpha_workspace_root = Path.join(root, "alpha-workspaces")
      beta_workspace_root = Path.join(root, "beta-workspaces")

      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "workspace:",
            "  root: \"#{alpha_workspace_root}\"",
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
                "  root: \"#{beta_workspace_root}\"",
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

              alpha_policy = Config.Schema.resolve_turn_sandbox_policy(alpha_settings)
              beta_policy = Config.Schema.resolve_turn_sandbox_policy(beta_settings)

              assert alpha_policy["writableRoots"] == [alpha_settings.workspace.root]
              assert beta_policy["writableRoots"] == [beta_settings.workspace.root]
              refute alpha_policy["writableRoots"] == beta_policy["writableRoots"]
            end
          )
        end
      )
    end
  end

  # ────────────────────────────────────────────────────────────────
  # Config OpenCode settings per-project isolation tests
  # ────────────────────────────────────────────────────────────────

  describe "config opencode settings per-project isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-opencode-settings-#{System.unique_integer([:positive])}"
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

    test "Config.settings!/1 returns per-project opencode.command", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "opencode:",
            "  command: alpha-opencode",
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
                "  command: beta-opencode",
                "  agent: architect",
                "  format: markdown",
                "  result_state: \"Human Review\""
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).opencode.command == "alpha-opencode"
              assert Config.settings!(beta_ctx).opencode.command == "beta-opencode"
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project opencode.agent and opencode.format", %{test_root: root} do
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
                "  agent: architect",
                "  format: markdown",
                "  result_state: \"Human Review\""
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).opencode.agent == "build"
              assert Config.settings!(alpha_ctx).opencode.format == "json"
              assert Config.settings!(alpha_ctx).opencode.result_state == "In Review"

              assert Config.settings!(beta_ctx).opencode.agent == "architect"
              assert Config.settings!(beta_ctx).opencode.format == "markdown"
              assert Config.settings!(beta_ctx).opencode.result_state == "Human Review"
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project opencode.project_root", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "opencode:",
            "  project_root: \"#{Path.join(root, "alpha-oc")}\"",
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
                "  project_root: \"#{Path.join(root, "beta-oc")}\"",
                "  agent: build",
                "  format: json",
                "  result_state: \"In Review\""
              ]
            },
            fn beta_ctx ->
              alpha_root = Config.settings!(alpha_ctx).opencode.project_root
              beta_root = Config.settings!(beta_ctx).opencode.project_root

              assert alpha_root =~ "alpha-oc"
              assert beta_root =~ "beta-oc"
              refute alpha_root =~ "beta-oc"
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project opencode.timeout_ms", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "opencode:",
            "  timeout_ms: 1800000",
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
                "  timeout_ms: 7200000",
                "  agent: build",
                "  format: json",
                "  result_state: \"In Review\""
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).opencode.timeout_ms == 1_800_000
              assert Config.settings!(beta_ctx).opencode.timeout_ms == 7_200_000
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project opencode.protocol and opencode.server_url", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "opencode:",
            "  protocol: acp",
            "  server_url: http://alpha-server:8080",
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
                "  protocol: cli",
                "  agent: build",
                "  format: json",
                "  result_state: \"Human Review\""
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).opencode.protocol == "acp"
              assert Config.settings!(alpha_ctx).opencode.server_url == "http://alpha-server:8080"

              assert Config.settings!(beta_ctx).opencode.protocol == "cli"
              assert Config.settings!(beta_ctx).opencode.server_url == nil
            end
          )
        end
      )
    end
  end

  # ────────────────────────────────────────────────────────────────
  # Config Worker settings per-project isolation tests
  # ────────────────────────────────────────────────────────────────

  describe "config worker settings per-project isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-worker-settings-#{System.unique_integer([:positive])}"
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

    test "Config.settings!/1 returns per-project worker.ssh_hosts", %{test_root: root} do
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
            "worker:",
            "  ssh_hosts:",
            "    - alpha-host-1",
            "    - alpha-host-2"
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
                "worker:",
                "  ssh_hosts:",
                "    - beta-host-1"
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).worker.ssh_hosts == ["alpha-host-1", "alpha-host-2"]
              assert Config.settings!(beta_ctx).worker.ssh_hosts == ["beta-host-1"]
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project worker.max_concurrent_agents_per_host", %{test_root: root} do
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
            "worker:",
            "  max_concurrent_agents_per_host: 2"
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
                "worker:",
                "  max_concurrent_agents_per_host: 5"
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).worker.max_concurrent_agents_per_host == 2
              assert Config.settings!(beta_ctx).worker.max_concurrent_agents_per_host == 5
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns default empty ssh_hosts per project when unset", %{test_root: root} do
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
            "worker:",
            "  ssh_hosts:",
            "    - alpha-host"
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
              assert Config.settings!(alpha_ctx).worker.ssh_hosts == ["alpha-host"]
              assert Config.settings!(beta_ctx).worker.ssh_hosts == []
            end
          )
        end
      )
    end
  end

  # ────────────────────────────────────────────────────────────────
  # Config Observability settings per-project isolation tests
  # ────────────────────────────────────────────────────────────────

  describe "config observability settings per-project isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-observability-settings-#{System.unique_integer([:positive])}"
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

    test "Config.settings!/1 returns per-project observability.dashboard_enabled", %{test_root: root} do
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
            "observability:",
            "  dashboard_enabled: false"
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
                "observability:",
                "  dashboard_enabled: true"
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).observability.dashboard_enabled == false
              assert Config.settings!(beta_ctx).observability.dashboard_enabled == true
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project observability.refresh_ms and render_interval_ms", %{test_root: root} do
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
            "observability:",
            "  refresh_ms: 500",
            "  render_interval_ms: 33"
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
                "observability:",
                "  refresh_ms: 2000",
                "  render_interval_ms: 100"
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).observability.refresh_ms == 500
              assert Config.settings!(alpha_ctx).observability.render_interval_ms == 33

              assert Config.settings!(beta_ctx).observability.refresh_ms == 2000
              assert Config.settings!(beta_ctx).observability.render_interval_ms == 100
            end
          )
        end
      )
    end
  end

  # ────────────────────────────────────────────────────────────────
  # Config Server settings per-project isolation tests
  # ────────────────────────────────────────────────────────────────
end
