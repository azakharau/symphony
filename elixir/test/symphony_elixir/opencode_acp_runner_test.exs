defmodule SymphonyElixir.OpenCodeACPRunnerTest do
  use SymphonyElixir.TestSupport

  alias SymphonyElixir.AgentRunner
  alias SymphonyElixir.OpenCode.ACPSessionStore
  alias SymphonyElixir.OpenCode.Runner

  defmodule FailingSessionStore do
    def fetch(_issue, _project_root), do: {:ok, nil}
    def fetch(_issue, _project_root, _session_scope), do: {:ok, nil}
    def put(_issue, _project_root, _session_id), do: {:error, :write_failed}
    def put(_issue, _project_root, _session_id, _session_scope), do: {:error, :write_failed}
    def prompt_scope(prompt), do: ACPSessionStore.prompt_scope(prompt)
  end

  test "opencode acp runner starts command with acp args and project root cwd" do
    {python, script} = fake_acp_server!()
    project_root = File.cwd!()
    issue = issue()
    workspace_root = workspace_root!()

    write_workflow_file!(Workflow.workflow_file_path(),
      opencode_protocol: "acp",
      opencode_command: python,
      opencode_args: [script, "runner"],
      opencode_project_root: project_root,
      opencode_stall_timeout_ms: 200,
      workspace_root: workspace_root
    )

    assert {:ok, %{command: [^python, ^script, "runner"], output: output}} =
             Runner.run("/tmp/workspace", issue, "prompt body")

    assert output =~ "processCwd"
    assert output =~ project_root
  end

  test "opencode acp runner reuses persisted issue session id" do
    {python, script} = fake_acp_server!()
    issue = issue()
    workspace_root = workspace_root!()
    write_session_store!(workspace_root, issue, File.cwd!(), "existing-session", "prompt body")

    write_workflow_file!(Workflow.workflow_file_path(),
      opencode_protocol: "acp",
      opencode_command: python,
      opencode_args: [script, "runner"],
      opencode_project_root: File.cwd!(),
      workspace_root: workspace_root
    )

    assert {:error, {:need_owner_input, {:opencode_acp_session_attached, "existing-session"}}} =
             Runner.run("/tmp/workspace", issue, "prompt body")
  end

  test "opencode acp runner attaches completed persisted session handoff instead of parking" do
    {python, script} = fake_acp_server!()
    issue = issue()
    workspace_root = workspace_root!()
    project_root = File.cwd!()
    write_session_store!(workspace_root, issue, project_root, "existing-session", "prompt body")

    write_workflow_file!(Workflow.workflow_file_path(),
      opencode_protocol: "acp",
      opencode_command: python,
      opencode_args: [script, "runner"],
      opencode_project_root: project_root,
      workspace_root: workspace_root
    )

    assert {:ok, %{output: output, session_id: "existing-session"}} =
             Runner.run("/tmp/workspace", issue, "prompt body",
               session_result_reader: fn ^project_root, "existing-session" ->
                 {:ok, "Latest assistant handoff:\n\nFinal persisted ACP handoff"}
               end
             )

    assert output =~ "Final persisted ACP handoff"
  end

  test "opencode acp runner starts a fresh session when the task prompt changes" do
    {python, script} = fake_acp_server!()
    issue = issue()
    workspace_root = workspace_root!()
    project_root = File.cwd!()
    write_session_store!(workspace_root, issue, project_root, "old-prompt-session", "old prompt")

    write_workflow_file!(Workflow.workflow_file_path(),
      opencode_protocol: "acp",
      opencode_command: python,
      opencode_args: [script, "runner"],
      opencode_project_root: project_root,
      workspace_root: workspace_root
    )

    assert {:ok, %{output: output, session_id: "new-session"}} =
             Runner.run("/tmp/workspace", issue, "new prompt")

    assert output =~ "ACP result"
  end

  test "opencode acp runner prefers completed local session handoff after fresh end_turn" do
    {python, script} = fake_acp_server!()
    issue = issue()
    workspace_root = workspace_root!()
    project_root = File.cwd!()

    write_workflow_file!(Workflow.workflow_file_path(),
      opencode_protocol: "acp",
      opencode_command: python,
      opencode_args: [script, "runner"],
      opencode_project_root: project_root,
      workspace_root: workspace_root
    )

    assert {:ok, %{output: output, session_id: "new-session"}} =
             Runner.run("/tmp/workspace", issue, "prompt body",
               session_result_reader: fn ^project_root, "new-session" ->
                 {:ok, "Latest assistant handoff:\n\nFresh persisted ACP handoff"}
               end
             )

    assert output =~ "Fresh persisted ACP handoff"
    refute output =~ "ACP result"
  end

  test "opencode acp runner does not resend initial prompt after resume" do
    {python, script} = fake_acp_server!()
    issue = issue()
    workspace_root = workspace_root!()
    write_session_store!(workspace_root, issue, File.cwd!(), "existing-session", "initial prompt must not be sent")

    write_workflow_file!(Workflow.workflow_file_path(),
      opencode_protocol: "acp",
      opencode_command: python,
      opencode_args: [script, "no-prompt-after-resume"],
      opencode_project_root: File.cwd!(),
      workspace_root: workspace_root
    )

    assert {:error, {:need_owner_input, {:opencode_acp_session_attached, "existing-session"}}} =
             Runner.run("/tmp/workspace", issue, "initial prompt must not be sent")
  end

  test "opencode acp dispatch records attached session diagnostic instead of handoff" do
    {python, script} = fake_acp_server!()
    issue = issue()
    workspace_root = workspace_root!()
    project_root = File.cwd!()

    write_session_store!(workspace_root, issue, project_root, "existing-session", "Existing session should not replay this prompt.")

    write_workflow_file!(Workflow.workflow_file_path(),
      tracker_kind: "memory",
      runner_routes: %{"In Progress" => "opencode"},
      opencode_protocol: "acp",
      opencode_command: python,
      opencode_args: [script, "runner"],
      opencode_project_root: project_root,
      workspace_root: workspace_root
    )

    Application.put_env(:symphony_elixir, :memory_tracker_recipient, self())

    Application.put_env(:symphony_elixir, :memory_tracker_opencode_comments, %{
      issue.id => [
        """
        <!-- symphony:opencode-task-prompt:v1 slice_id=attached-session -->
        ```text
        Existing session should not replay this prompt.
        ```
        """
      ]
    })

    assert :ok = AgentRunner.run(issue, self())

    assert_receive {:memory_tracker_comment, "issue-1", comment}
    assert comment =~ "## OpenCode Session Attached"
    assert comment =~ "Session ID: `existing-session`"
    refute comment =~ "## OpenCode Handoff"
    refute comment =~ "OpenCode requested owner input"

    assert_receive {:memory_tracker_state_update, "issue-1", "Need Owner Input"}
  end

  test "opencode acp runner durable session mapping survives runner process restart" do
    {python, script} = fake_acp_server!()
    workspace_root = workspace_root!()
    issue = issue()

    write_workflow_file!(Workflow.workflow_file_path(),
      opencode_protocol: "acp",
      opencode_command: python,
      opencode_args: [script, "runner"],
      opencode_project_root: File.cwd!(),
      workspace_root: workspace_root
    )

    assert {:ok, %{output: output}} = Runner.run("/tmp/workspace", issue, "initial prompt")
    assert output =~ "ACP result"

    assert {:error, {:need_owner_input, {:opencode_acp_session_attached, "new-session"}}} =
             Runner.run("/tmp/workspace", issue, "initial prompt")
  end

  test "opencode acp runner prevents duplicate sessions for same issue and project" do
    {python, script} = fake_acp_server!()
    workspace_root = workspace_root!()
    issue = issue()

    write_workflow_file!(Workflow.workflow_file_path(),
      opencode_protocol: "acp",
      opencode_command: python,
      opencode_args: [script, "count-new"],
      opencode_project_root: File.cwd!(),
      workspace_root: workspace_root
    )

    assert {:ok, _result} = Runner.run("/tmp/workspace", issue, "initial prompt")

    assert {:error, {:need_owner_input, {:opencode_acp_session_attached, "new-session"}}} =
             Runner.run("/tmp/workspace", issue, "initial prompt")
  end

  test "opencode acp runner does not prompt when durable session write fails" do
    {python, script} = fake_acp_server!()
    workspace_root = workspace_root!()

    write_workflow_file!(Workflow.workflow_file_path(),
      opencode_protocol: "acp",
      opencode_command: python,
      opencode_args: [script, "fail-if-prompt"],
      opencode_project_root: File.cwd!(),
      workspace_root: workspace_root
    )

    assert {:error, {:opencode_acp_session_store_failed, :write_failed}} =
             Runner.run("/tmp/workspace", issue(), "prompt body", session_store: FailingSessionStore)
  end

  test "opencode acp runner maps end_turn to successful handoff" do
    {python, script} = fake_acp_server!()
    workspace_root = workspace_root!()

    write_workflow_file!(Workflow.workflow_file_path(),
      opencode_protocol: "acp",
      opencode_command: python,
      opencode_args: [script, "runner"],
      opencode_project_root: File.cwd!(),
      workspace_root: workspace_root
    )

    assert {:ok, %{output: output}} = Runner.run("/tmp/workspace", issue(), "prompt body")
    assert output =~ "ACP result"
    assert output =~ "end_turn"
  end

  test "opencode acp runner streams session updates and usage through runner events" do
    {python, script} = fake_acp_server!()
    workspace_root = workspace_root!()
    test_pid = self()

    write_workflow_file!(Workflow.workflow_file_path(),
      opencode_protocol: "acp",
      opencode_command: python,
      opencode_args: [script, "streaming"],
      opencode_project_root: File.cwd!(),
      workspace_root: workspace_root
    )

    assert {:ok, %{output: output}} =
             Runner.run("/tmp/workspace", issue(), "prompt body", on_event: fn event -> send(test_pid, {:opencode_event, event}) end)

    assert output =~ "streamed text"

    assert_receive {:opencode_event, %{event: :command_prepared, phase: :command}}
    assert_receive {:opencode_event, %{event: :session_started, session_id: "new-session", phase: :session}}

    assert_receive {:opencode_event,
                    %{
                      event: :notification,
                      phase: :running,
                      payload: %{
                        "method" => "session/update",
                        "params" => %{"type" => "agent_text", "text" => "streamed text"}
                      }
                    }}

    assert_receive {:opencode_event,
                    %{
                      event: :notification,
                      phase: :usage,
                      usage: %{"inputTokens" => 12, "outputTokens" => 4, "totalTokens" => 16},
                      payload: %{
                        "method" => "session/update",
                        "params" => %{
                          "type" => "usage",
                          "usage" => %{"inputTokens" => 12, "outputTokens" => 4, "totalTokens" => 16}
                        }
                      }
                    }}
  end

  test "opencode acp runner streams persisted session usage when acp update has no usage payload" do
    {python, script} = fake_acp_server!()
    workspace_root = workspace_root!()
    test_pid = self()
    project_root = File.cwd!()

    write_workflow_file!(Workflow.workflow_file_path(),
      opencode_protocol: "acp",
      opencode_command: python,
      opencode_args: [script, "streaming-no-usage"],
      opencode_project_root: project_root,
      workspace_root: workspace_root
    )

    assert {:ok, %{output: output}} =
             Runner.run("/tmp/workspace", issue(), "prompt body",
               on_event: fn event -> send(test_pid, {:opencode_event, event}) end,
               session_usage_reader: fn ^project_root, "new-session" ->
                 {:ok, %{"inputTokens" => 81_069, "outputTokens" => 4_799, "totalTokens" => 85_868}}
               end
             )

    assert output =~ "streamed text"

    assert_receive {:opencode_event,
                    %{
                      event: :notification,
                      phase: :running,
                      usage: %{"inputTokens" => 81_069, "outputTokens" => 4_799, "totalTokens" => 85_868},
                      payload: %{
                        "method" => "session/update",
                        "params" => %{"type" => "agent_text", "text" => "streamed text"}
                      }
                    }}
  end

  test "opencode acp runner parks user input requests as need owner input" do
    {python, script} = fake_acp_server!()
    workspace_root = workspace_root!()

    write_workflow_file!(Workflow.workflow_file_path(),
      opencode_protocol: "acp",
      opencode_command: python,
      opencode_args: [script, "user-input"],
      opencode_project_root: File.cwd!(),
      opencode_stall_timeout_ms: 200,
      workspace_root: workspace_root
    )

    assert {:error, {:need_owner_input, _events}} =
             Runner.run("/tmp/workspace", issue(), "prompt body")
  end

  test "opencode acp runner disables stall timeout when configured as zero" do
    {python, script} = fake_acp_server!()
    workspace_root = workspace_root!()

    write_workflow_file!(Workflow.workflow_file_path(),
      opencode_protocol: "acp",
      opencode_command: python,
      opencode_args: [script, "slow-success"],
      opencode_project_root: File.cwd!(),
      opencode_stall_timeout_ms: 0,
      workspace_root: workspace_root
    )

    assert {:ok, %{output: output}} = Runner.run("/tmp/workspace", issue(), "prompt body")
    assert output =~ "slow done"
  end

  test "opencode acp runner falls back to cli when protocol is cli" do
    test_pid = self()
    issue = issue()

    write_workflow_file!(Workflow.workflow_file_path(), opencode_protocol: "cli")

    assert {:ok, %{output: "done\n"}} =
             Runner.run("/tmp/workspace", issue, "prompt body",
               runner: fn command, args, opts ->
                 send(test_pid, {:cli_called, command, args, opts})
                 {"done\n", 0}
               end
             )

    assert_received {:cli_called, "bash", _args, opts}
    assert opts[:cd] == "/tmp/workspace"
  end

  test "opencode acp runner returns deterministic command not found error" do
    write_workflow_file!(Workflow.workflow_file_path(),
      opencode_protocol: "acp",
      opencode_command: "definitely-not-opencode-acp",
      opencode_project_root: File.cwd!(),
      workspace_root: workspace_root!()
    )

    assert {:error, {:acp_command_not_found, "definitely-not-opencode-acp"}} =
             Runner.run("/tmp/workspace", issue(), "prompt body")
  end

  defp issue do
    %Issue{
      id: "issue-1",
      identifier: "NER-1",
      title: "Implement scoped change",
      description: "Task",
      state: "In Progress"
    }
  end

  defp workspace_root! do
    suffix =
      8
      |> :crypto.strong_rand_bytes()
      |> Base.url_encode64(padding: false)

    root =
      Path.join(
        System.tmp_dir!(),
        "opencode-acp-sessions-#{System.os_time(:nanosecond)}-#{suffix}"
      )

    File.rm_rf!(root)
    File.mkdir_p!(root)
    on_exit(fn -> File.rm_rf(root) end)
    root
  end

  defp write_session_store!(workspace_root, issue, project_root, session_id, prompt) do
    write_workflow_file!(Workflow.workflow_file_path(), workspace_root: workspace_root)
    :ok = ACPSessionStore.put(issue, project_root, session_id, ACPSessionStore.prompt_scope(prompt))
  end

  defp fake_acp_server! do
    path =
      Path.join(System.tmp_dir!(), "fake-acp-runner-#{System.unique_integer([:positive])}.py")

    File.write!(path, ~S'''
    import json, os, sys
    scenario = sys.argv[1] if len(sys.argv) > 1 else "runner"

    def send(obj):
        sys.stdout.write(json.dumps(obj) + "\n")
        sys.stdout.flush()

    for line in sys.stdin:
        msg = json.loads(line)
        if "method" not in msg:
            continue
        mid = msg["id"]
        method = msg["method"]
        params = msg.get("params", {})
        caps = {"session/new": True, "session/load": True, "session/resume": True, "session/prompt": True, "session/cancel": True}
        if method == "initialize":
            send({"jsonrpc":"2.0","id":mid,"result":{"capabilities":caps}})
        elif method == "session/new":
            send({"jsonrpc":"2.0","id":mid,"result":{"sessionId":"new-session","cwd":params.get("cwd"),"processCwd":os.getcwd()}})
        elif method == "session/resume":
            send({"jsonrpc":"2.0","id":mid,"result":{"sessionId":params.get("sessionId"),"resumed":True}})
        elif method == "session/load":
            send({"jsonrpc":"2.0","id":mid,"result":{"sessionId":params.get("sessionId"),"loaded":True}})
        elif method == "session/prompt" and scenario == "no-prompt-after-resume":
            send({"jsonrpc":"2.0","id":mid,"error":{"message":"prompt replayed"}})
        elif method == "session/prompt" and scenario == "user-input":
            send({"jsonrpc":"2.0","method":"session/update","params":{"type":"user_input_required","message":"permission needed"}})
            send({"jsonrpc":"2.0","id":mid,"result":{"stopReason":"user_input_required"}})
        elif method == "session/prompt" and scenario == "slow-success":
            import time
            time.sleep(0.05)
            send({"jsonrpc":"2.0","method":"session/update","params":{"type":"agent_text","text":"slow done"}})
            send({"jsonrpc":"2.0","method":"session/update","params":{"type":"end_turn","processCwd":os.getcwd()}})
            send({"jsonrpc":"2.0","id":mid,"result":{"stopReason":"end_turn","processCwd":os.getcwd()}})
        elif method == "session/prompt" and scenario == "streaming":
            send({"jsonrpc":"2.0","method":"session/update","params":{"type":"agent_text","text":"streamed text"}})
            send({"jsonrpc":"2.0","method":"session/update","params":{"type":"tool_plan","tool":"edit"}})
            send({"jsonrpc":"2.0","method":"session/update","params":{"type":"usage","usage":{"inputTokens":12,"outputTokens":4,"totalTokens":16}}})
            send({"jsonrpc":"2.0","method":"session/update","params":{"type":"end_turn","processCwd":os.getcwd()}})
            send({"jsonrpc":"2.0","id":mid,"result":{"stopReason":"end_turn","processCwd":os.getcwd()}})
        elif method == "session/prompt" and scenario == "streaming-no-usage":
            send({"jsonrpc":"2.0","method":"session/update","params":{"type":"agent_text","text":"streamed text"}})
            send({"jsonrpc":"2.0","method":"session/update","params":{"type":"end_turn","processCwd":os.getcwd()}})
            send({"jsonrpc":"2.0","id":mid,"result":{"stopReason":"end_turn","processCwd":os.getcwd()}})
        elif method == "session/prompt" and scenario == "fail-if-prompt":
            send({"jsonrpc":"2.0","id":mid,"error":{"message":"prompt should not be sent before durable session write"}})
        elif method == "session/prompt":
            send({"jsonrpc":"2.0","method":"session/update","params":{"type":"agent_text","text":"done"}})
            send({"jsonrpc":"2.0","method":"session/update","params":{"type":"end_turn","processCwd":os.getcwd()}})
            send({"jsonrpc":"2.0","id":mid,"result":{"stopReason":"end_turn","processCwd":os.getcwd()}})
        elif method == "session/cancel":
            send({"jsonrpc":"2.0","id":mid,"result":{"cancelled":True}})
    ''')

    {System.find_executable("python3") || System.find_executable("python"), path}
  end
end
