defmodule SymphonyElixir.CLITest do
  use ExUnit.Case, async: true

  alias SymphonyElixir.CLI

  @ack_flag "--i-understand-that-this-will-be-running-without-the-usual-guardrails"

  test "returns the guardrails acknowledgement banner when the flag is missing" do
    parent = self()

    deps = %{
      file_regular?: fn _path ->
        send(parent, :file_checked)
        true
      end,
      set_workflow_file_path: fn _path ->
        send(parent, :workflow_set)
        :ok
      end,
      set_logs_root: fn _path ->
        send(parent, :logs_root_set)
        :ok
      end,
      set_server_port_override: fn _port ->
        send(parent, :port_set)
        :ok
      end,
      ensure_all_started: fn ->
        send(parent, :started)
        {:ok, [:symphony_elixir]}
      end
    }

    assert {:error, banner} = CLI.evaluate(["WORKFLOW.md"], deps)
    assert banner =~ "This Symphony implementation is a low key engineering preview."
    assert banner =~ "Codex will run without any guardrails."
    assert banner =~ "SymphonyElixir is not a supported product and is presented as-is."
    assert banner =~ @ack_flag
    refute_received :file_checked
    refute_received :workflow_set
    refute_received :logs_root_set
    refute_received :port_set
    refute_received :started
  end

  test "defaults to WORKFLOW.md when workflow path is missing" do
    deps = %{
      file_regular?: fn path -> Path.basename(path) == "WORKFLOW.md" end,
      set_workflow_file_path: fn _path -> :ok end,
      set_logs_root: fn _path -> :ok end,
      set_server_port_override: fn _port -> :ok end,
      ensure_all_started: fn -> {:ok, [:symphony_elixir]} end
    }

    assert :ok = CLI.evaluate([@ack_flag], deps)
  end

  test "uses an explicit workflow path override when provided" do
    parent = self()
    workflow_path = "tmp/custom/WORKFLOW.md"
    expanded_path = Path.expand(workflow_path)

    deps = %{
      file_regular?: fn path ->
        send(parent, {:workflow_checked, path})
        path == expanded_path
      end,
      set_workflow_file_path: fn path ->
        send(parent, {:workflow_set, path})
        :ok
      end,
      set_logs_root: fn _path -> :ok end,
      set_server_port_override: fn _port -> :ok end,
      ensure_all_started: fn -> {:ok, [:symphony_elixir]} end
    }

    assert :ok = CLI.evaluate([@ack_flag, workflow_path], deps)
    assert_received {:workflow_checked, ^expanded_path}
    assert_received {:workflow_set, ^expanded_path}
  end

  test "accepts --logs-root and passes an expanded root to runtime deps" do
    parent = self()

    deps = %{
      file_regular?: fn _path -> true end,
      set_workflow_file_path: fn _path -> :ok end,
      set_logs_root: fn path ->
        send(parent, {:logs_root, path})
        :ok
      end,
      set_server_port_override: fn _port -> :ok end,
      ensure_all_started: fn -> {:ok, [:symphony_elixir]} end
    }

    assert :ok = CLI.evaluate([@ack_flag, "--logs-root", "tmp/custom-logs", "WORKFLOW.md"], deps)
    assert_received {:logs_root, expanded_path}
    assert expanded_path == Path.expand("tmp/custom-logs")
  end

  test "returns not found when workflow file does not exist" do
    deps = %{
      file_regular?: fn _path -> false end,
      set_workflow_file_path: fn _path -> :ok end,
      set_logs_root: fn _path -> :ok end,
      set_server_port_override: fn _port -> :ok end,
      ensure_all_started: fn -> {:ok, [:symphony_elixir]} end
    }

    assert {:error, message} = CLI.evaluate([@ack_flag, "WORKFLOW.md"], deps)
    assert message =~ "Workflow file not found:"
  end

  test "returns startup error when app cannot start" do
    deps = %{
      file_regular?: fn _path -> true end,
      set_workflow_file_path: fn _path -> :ok end,
      set_logs_root: fn _path -> :ok end,
      set_server_port_override: fn _port -> :ok end,
      ensure_all_started: fn -> {:error, :boom} end
    }

    assert {:error, message} = CLI.evaluate([@ack_flag, "WORKFLOW.md"], deps)
    assert message =~ "Failed to start Symphony with workflow"
    assert message =~ ":boom"
  end

  test "returns ok when workflow exists and app starts" do
    deps = %{
      file_regular?: fn _path -> true end,
      set_workflow_file_path: fn _path -> :ok end,
      set_logs_root: fn _path -> :ok end,
      set_server_port_override: fn _port -> :ok end,
      ensure_all_started: fn -> {:ok, [:symphony_elixir]} end
    }

    assert :ok = CLI.evaluate([@ack_flag, "WORKFLOW.md"], deps)
  end

  test "parses --projects-config, sets root path, and starts root application" do
    parent = self()
    config_path = "tmp/projects.yml"
    expanded_path = Path.expand(config_path)

    root_config = %SymphonyElixir.RootConfig{
      server: %{host: "127.0.0.1", port: 4100},
      projects: []
    }

    deps = %{
      file_regular?: fn path ->
        send(parent, {:projects_config_checked, path})
        path == expanded_path
      end,
      set_workflow_file_path: fn _path ->
        send(parent, :workflow_set)
        :ok
      end,
      set_logs_root: fn _path -> :ok end,
      set_server_port_override: fn _port -> :ok end,
      set_root_config_path: fn path ->
        send(parent, {:root_config_path_set, path})
        :ok
      end,
      load_root_config: fn path ->
        send(parent, {:projects_config_loaded, path})
        {:ok, root_config}
      end,
      ensure_root_started: fn ^root_config ->
        send(parent, {:root_started, root_config})
        {:ok, [:symphony_elixir]}
      end,
      ensure_all_started: fn ->
        send(parent, :single_project_started)
        {:ok, [:symphony_elixir]}
      end
    }

    assert :ok = CLI.evaluate([@ack_flag, "--projects-config", config_path], deps)
    assert_received {:projects_config_checked, ^expanded_path}
    assert_received {:projects_config_loaded, ^expanded_path}
    assert_received {:root_config_path_set, ^expanded_path}
    assert_received {:root_started, ^root_config}
    refute_received :workflow_set
    refute_received :single_project_started
  end

  test "rejects --projects-config combined with a workflow path" do
    deps = %{
      file_regular?: fn _path -> true end,
      set_workflow_file_path: fn _path -> :ok end,
      set_logs_root: fn _path -> :ok end,
      set_server_port_override: fn _port -> :ok end,
      ensure_all_started: fn -> {:ok, [:symphony_elixir]} end
    }

    assert {:error, message} =
             CLI.evaluate([@ack_flag, "--projects-config", "projects.yml", "WORKFLOW.md"], deps)

    assert message =~ "--projects-config"
    assert message =~ "path-to-WORKFLOW.md"
  end

  test "returns not found when projects config file does not exist" do
    deps = %{
      file_regular?: fn _path -> false end,
      set_workflow_file_path: fn _path -> :ok end,
      set_logs_root: fn _path -> :ok end,
      set_server_port_override: fn _port -> :ok end,
      set_root_config_path: fn _path -> :ok end,
      load_root_config: fn _path -> {:ok, %{}} end,
      ensure_root_started: fn _root_config -> {:ok, [:symphony_elixir]} end,
      ensure_all_started: fn -> {:ok, [:symphony_elixir]} end
    }

    assert {:error, message} = CLI.evaluate([@ack_flag, "--projects-config", "nonexistent.yml"], deps)
    assert message =~ "Projects config file not found:"
  end

  test "returns error when projects config file is a directory" do
    parent = self()

    deps = %{
      file_regular?: fn path ->
        send(parent, {:config_checked, path})
        false
      end,
      set_workflow_file_path: fn _path -> :ok end,
      set_logs_root: fn _path -> :ok end,
      set_server_port_override: fn _port -> :ok end,
      set_root_config_path: fn _path -> :ok end,
      load_root_config: fn _path -> {:ok, %{}} end,
      ensure_root_started: fn _root_config -> {:ok, [:symphony_elixir]} end,
      ensure_all_started: fn -> {:ok, [:symphony_elixir]} end
    }

    assert {:error, message} = CLI.evaluate([@ack_flag, "--projects-config", "tmp/mydir"], deps)
    assert message =~ "Projects config file not found:"
    assert_received {:config_checked, _path}
  end

  test "returns error when projects config loading fails" do
    parent = self()
    config_path = "tmp/broken.yml"
    expanded_path = Path.expand(config_path)

    deps = %{
      file_regular?: fn _path -> true end,
      set_workflow_file_path: fn _path -> :ok end,
      set_logs_root: fn _path -> :ok end,
      set_server_port_override: fn _port -> :ok end,
      set_root_config_path: fn path ->
        send(parent, {:root_config_path_set, path})
        :ok
      end,
      load_root_config: fn path ->
        send(parent, {:projects_config_loaded, path})
        {:error, :root_config_parse_error}
      end,
      ensure_root_started: fn _root_config -> {:ok, [:symphony_elixir]} end,
      ensure_all_started: fn -> {:ok, [:symphony_elixir]} end
    }

    assert {:error, message} = CLI.evaluate([@ack_flag, "--projects-config", config_path], deps)
    assert message =~ "Invalid projects config"
    assert message =~ "root_config_parse_error"
    assert_received {:projects_config_loaded, ^expanded_path}
    refute_received {:root_config_path_set, ^expanded_path}
  end

  test "returns error when root application fails to start with projects config" do
    parent = self()
    config_path = "tmp/start-fail.yml"
    expanded_path = Path.expand(config_path)

    root_config = %SymphonyElixir.RootConfig{
      server: %{host: "127.0.0.1", port: 4100},
      projects: []
    }

    deps = %{
      file_regular?: fn _path -> true end,
      set_workflow_file_path: fn _path -> :ok end,
      set_logs_root: fn _path -> :ok end,
      set_server_port_override: fn _port -> :ok end,
      set_root_config_path: fn path ->
        send(parent, {:root_config_path_set, path})
        :ok
      end,
      load_root_config: fn path ->
        send(parent, {:projects_config_loaded, path})
        {:ok, root_config}
      end,
      ensure_root_started: fn ^root_config ->
        send(parent, {:root_started, root_config})
        {:error, :root_boot_failed}
      end,
      ensure_all_started: fn -> {:ok, [:symphony_elixir]} end
    }

    assert {:error, message} = CLI.evaluate([@ack_flag, "--projects-config", config_path], deps)
    assert message =~ "Failed to start Symphony with projects config"
    assert message =~ "root_boot_failed"
    assert_received {:projects_config_loaded, ^expanded_path}
    assert_received {:root_config_path_set, ^expanded_path}
    assert_received {:root_started, ^root_config}
  end

  test "handles absolute path for --projects-config without double-expanding" do
    parent = self()
    absolute_path = "/tmp/absolute-projects.yml"
    expanded_absolute = Path.expand(absolute_path)

    deps = %{
      file_regular?: fn path ->
        send(parent, {:config_checked, path})
        path == absolute_path
      end,
      set_workflow_file_path: fn _path -> :ok end,
      set_logs_root: fn _path -> :ok end,
      set_server_port_override: fn _port -> :ok end,
      set_root_config_path: fn path ->
        send(parent, {:root_config_path_set, path})
        :ok
      end,
      load_root_config: fn path ->
        send(parent, {:projects_config_loaded, path})
        {:ok, %SymphonyElixir.RootConfig{server: %{host: "127.0.0.1", port: 4100}, projects: []}}
      end,
      ensure_root_started: fn _root_config ->
        send(parent, :root_started)
        {:ok, [:symphony_elixir]}
      end,
      ensure_all_started: fn -> {:ok, [:symphony_elixir]} end
    }

    assert :ok = CLI.evaluate([@ack_flag, "--projects-config", absolute_path], deps)
    assert_received {:config_checked, ^expanded_absolute}
    assert_received {:projects_config_loaded, ^expanded_absolute}
  end
end
