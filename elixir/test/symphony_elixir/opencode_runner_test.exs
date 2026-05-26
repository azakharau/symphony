defmodule SymphonyElixir.OpenCodeRunnerTest do
  use SymphonyElixir.TestSupport

  alias SymphonyElixir.OpenCode.Runner

  test "opencode runner invokes configured command with Symphony task context" do
    issue = %Issue{
      id: "issue-1",
      identifier: "NER-1",
      title: "Implement scoped change",
      description: "Task packet from Codex Architect",
      state: "In Progress"
    }

    test_pid = self()

    assert {:ok, %{output: "done\n", command: ["opencode" | _args]}} =
             Runner.run("/tmp/workspace", issue, "prompt body",
               runner: fn command, args, opts ->
                 send(test_pid, {:opencode_called, command, args, opts})
                 {"done\n", 0}
               end
             )

    assert_received {:opencode_called, "opencode", received_args, opts}
    assert ["run", "--dir", "/tmp/workspace", "--agent", "build", "--format", "json", "--title", "NER-1 Implement scoped change", "prompt body"] = received_args
    assert opts[:cd] == "/tmp/workspace"
    assert opts[:stderr_to_stdout] == true
  end

  test "agent runner routes In Progress issues to OpenCode and returns to In Review" do
    workspace_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-opencode-route-#{System.unique_integer([:positive])}"
      )

    command = Path.join(workspace_root, "fake-opencode")
    File.mkdir_p!(workspace_root)

    File.write!(command, """
    #!/usr/bin/env bash
    printf 'opencode completed for %s\\n' "$*"
    """)

    File.chmod!(command, 0o755)

    write_workflow_file!(Workflow.workflow_file_path(),
      tracker_kind: "memory",
      workspace_root: Path.join(workspace_root, "workspaces"),
      runner_routes: %{"In Progress" => "opencode"},
      opencode_command: command,
      opencode_result_state: "In Review",
      prompt: "OpenCode prompt for {{ issue.identifier }}: {{ issue.description }}"
    )

    Application.put_env(:symphony_elixir, :memory_tracker_recipient, self())

    issue = %Issue{
      id: "issue-1",
      identifier: "NER-42",
      title: "Run OpenCode",
      description: "Implement via OpenCode",
      state: "In Progress"
    }

    assert :ok = AgentRunner.run(issue, nil)

    assert_receive {:memory_tracker_comment, "issue-1", comment}
    assert comment =~ "## OpenCode Handoff"
    assert comment =~ "opencode completed"

    assert_receive {:memory_tracker_state_update, "issue-1", "In Review"}
  end
end
