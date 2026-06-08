defmodule SymphonyElixir.ProjectRuntimeTest do
  use SymphonyElixir.TestSupport

  alias SymphonyElixir.OpenCode.Runner, as: OpenCodeRunner
  alias SymphonyElixir.ProjectContext
  alias SymphonyElixir.ProjectRegistry

  setup do
    unless Process.whereis(ProjectRegistry) do
      start_supervised!(ProjectRegistry)
    end

    :ok
  end

  test "codex app server uses project-scoped settings for cwd guards and command launch" do
    test_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-project-runtime-codex-#{System.unique_integer([:positive])}"
      )

    try do
      global_workspace_root = Path.join(test_root, "global-workspaces")
      alpha_workspace_root = Path.join(test_root, "alpha-workspaces")
      alpha_workspace = Path.join(alpha_workspace_root, "SYM-4")
      alpha_codex = Path.join(test_root, "alpha-codex")
      trace_file = Path.join(test_root, "alpha-codex.trace")

      File.mkdir_p!(global_workspace_root)
      File.mkdir_p!(alpha_workspace)
      File.mkdir_p!(alpha_codex)

      write_workflow_file!(Workflow.workflow_file_path(),
        workspace_root: global_workspace_root,
        codex_command: "global-codex-command-that-must-not-run"
      )

      codex_binary = write_fake_codex_app_server!(test_root, trace_file)

      alpha_context =
        start_project_workflow!(test_root, "alpha",
          workspace_root: alpha_workspace_root,
          codex_command: "#{codex_binary} app-server",
          codex_project_root: alpha_codex,
          codex_thread_id: "thread-alpha",
          prompt: "alpha prompt"
        )

      alpha_settings = Config.settings!(alpha_context)

      issue = %Issue{
        id: "issue-alpha",
        identifier: "SYM-4",
        title: "Project runtime isolation",
        state: "Todo"
      }

      assert {:ok, %{thread_id: "thread-alpha"}} =
               AppServer.run(alpha_codex, "Use alpha settings", issue, settings: alpha_settings)

      trace = File.read!(trace_file)
      assert trace =~ "PWD:#{alpha_codex} "
      refute trace =~ "global-codex-command-that-must-not-run"
    after
      File.rm_rf(test_root)
    end
  end

  test "opencode runner uses project-scoped settings for command, project root, and prompt storage" do
    test_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-project-runtime-opencode-#{System.unique_integer([:positive])}"
      )

    beta_project_root =
      Path.join(
        System.user_home!(),
        "symphony-project-runtime-opencode-root-#{System.unique_integer([:positive])}"
      )

    try do
      global_workspace_root = Path.join(test_root, "global-workspaces")
      beta_workspace_root = Path.join(test_root, "beta-workspaces")
      beta_workspace = Path.join(beta_workspace_root, "SYM-4")

      File.mkdir_p!(global_workspace_root)
      File.mkdir_p!(beta_workspace)
      File.mkdir_p!(beta_project_root)

      write_workflow_file!(Workflow.workflow_file_path(),
        workspace_root: global_workspace_root,
        opencode_command: "global-opencode-command",
        opencode_project_root: Path.join(test_root, "global-opencode-root")
      )

      beta_context =
        start_project_workflow!(test_root, "beta",
          workspace_root: beta_workspace_root,
          opencode_command: "beta-opencode-command",
          opencode_project_root: beta_project_root,
          prompt: "beta prompt"
        )

      beta_settings = Config.settings!(beta_context)

      issue = %Issue{
        id: "issue-beta",
        identifier: "SYM-4",
        title: "Project runtime isolation",
        state: "In Progress"
      }

      test_pid = self()

      assert {:ok, %{output: "done\n", project_root: ^beta_project_root}} =
               OpenCodeRunner.run(beta_workspace, issue, "prompt body",
                 settings: beta_settings,
                 runner: fn command, args, opts ->
                   prompt = File.read!(Enum.at(args, 3))
                   send(test_pid, {:opencode_called, command, args, opts, prompt})
                   {"done\n", 0}
                 end
               )

      assert_received {:opencode_called, "bash", received_args, opts, "prompt body"}

      assert [
               "-lc",
               "prompt_file=$1; shift; exec \"$@\" < \"$prompt_file\"",
               "symphony-opencode",
               prompt_path,
               "beta-opencode-command",
               "run",
               "--dir",
               ^beta_project_root | _rest
             ] = received_args

      assert String.starts_with?(prompt_path, Path.join(beta_project_root, ".symphony"))
      refute Enum.member?(received_args, "global-opencode-command")
      assert opts[:cd] == beta_project_root
    after
      File.rm_rf(test_root)
      File.rm_rf(beta_project_root)
    end
  end

  defp start_project_workflow!(root, project_id, opts) do
    workflow_path = write_project_workflow!(root, project_id, opts)

    context =
      ProjectContext.new(%{
        id: project_id,
        enabled: true,
        workflow_path: workflow_path
      })

    workflow_store_name =
      ProjectRegistry.via_name(context.process_names.workflow_store)

    start_supervised!({WorkflowStore, name: workflow_store_name, workflow_path: workflow_path})

    context
  end

  defp write_project_workflow!(root, project_id, opts) do
    path = Path.join([root, project_id, "WORKFLOW.md"])
    File.mkdir_p!(Path.dirname(path))

    File.write!(path, """
    ---
    tracker:
      kind: memory
      project_slug: "#{project_id}-slug"
      active_states: [Todo, In Progress]
      terminal_states: [Closed, Done]
    workspace:
      root: "#{opts[:workspace_root]}"
    agent:
      max_concurrent_agents: 1
      max_turns: 1
    runner:
      default: codex
      routes:
        In Progress: opencode
    codex:
      command: "#{opts[:codex_command] || "codex app-server"}"
      project_root: #{yaml_string(opts[:codex_project_root])}
      thread_id: #{yaml_string(opts[:codex_thread_id])}
      turn_timeout_ms: 1000
      read_timeout_ms: 1000
    opencode:
      protocol: cli
      command: "#{opts[:opencode_command] || "opencode"}"
      project_root: #{yaml_string(opts[:opencode_project_root])}
      agent: build
      format: json
      result_state: In Review
      timeout_ms: 1000
      read_timeout_ms: 1000
    ---
    #{opts[:prompt] || project_id}
    """)

    path
  end

  defp write_fake_codex_app_server!(root, trace_file) do
    codex_binary = Path.join(root, "fake-codex")

    File.write!(codex_binary, """
    #!/bin/sh
    trace_file="#{trace_file}"
    count=0

    while IFS= read -r line; do
      count=$((count + 1))
      printf 'PWD:%s JSON:%s\\n' "$(pwd)" "$line" >> "$trace_file"

      case "$count" in
        1)
          printf '%s\\n' '{"id":1,"result":{}}'
          ;;
        2)
          printf '%s\\n' '{"id":2,"result":{"thread":{"id":"thread-alpha"}}}'
          ;;
        3)
          printf '%s\\n' '{"id":3,"result":{"turn":{"id":"turn-alpha"}}}'
          ;;
        4)
          printf '%s\\n' '{"method":"turn/completed"}'
          exit 0
          ;;
        *)
          exit 0
          ;;
      esac
    done
    """)

    File.chmod!(codex_binary, 0o755)
    codex_binary
  end

  defp yaml_string(nil), do: "null"
  defp yaml_string(value) when is_binary(value), do: ~s("#{value}")
end
