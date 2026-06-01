defmodule SymphonyElixir.OpenCodeLiveGateTest do
  use SymphonyElixir.TestSupport

  alias SymphonyElixir.Linear.Issue

  @moduletag :opencode_live

  @project_root "/home/agent/proj/symphony"
  @opencode_command "/usr/local/bin/opencode"

  test "live OpenCode gate routes In Progress through OpenCode and records In Review handoff" do
    command = required_live_opencode_command!()
    server_url = live_server_url()
    before_status = repo_status!()

    workspace_root = Path.join(System.tmp_dir!(), "symphony-opencode-live-#{System.unique_integer([:positive])}")

    write_workflow_file!(Workflow.workflow_file_path(),
      tracker_kind: "memory",
      workspace_root: Path.join(workspace_root, "workspaces"),
      runner_routes: %{"In Progress" => "opencode"},
      opencode_command: command,
      opencode_project_root: @project_root,
      opencode_server_url: server_url,
      opencode_agent: "build",
      opencode_format: "json",
      opencode_result_state: "In Review",
      prompt: "Fallback prompt must not be used by the OpenCode live gate."
    )

    Application.put_env(:symphony_elixir, :memory_tracker_recipient, self())

    Application.put_env(:symphony_elixir, :memory_tracker_opencode_comments, %{
      "issue-opencode-live" => [live_task_packet()]
    })

    issue = %Issue{
      id: "issue-opencode-live",
      identifier: "SYMLIVE-12",
      title: "Validate OpenCode live gate",
      description: "Evidence-only live OpenCode validation; do not edit files.",
      state: "In Progress"
    }

    assert :ok = AgentRunner.run(issue, self())

    assert_receive {:worker_runtime_info, "issue-opencode-live", %{workspace_path: workspace_path}}
    assert workspace_path =~ Path.join(workspace_root, "workspaces")

    command_update = receive_runner_update("issue-opencode-live", :command_prepared, [:dispatch_allowed])
    assert command_update.event == :command_prepared
    assert command_update.runner_kind == "opencode"
    assert command_update.runner_owner == "opencode"
    assert command_update.project_root == @project_root
    assert command_update.workspace_path == workspace_path
    assert_opencode_command(command_update.command, command, server_url)
    assert_session_id_shape(command_update.session_id)

    assert_receive {:memory_tracker_comment, "issue-opencode-live", comment}, 600_000
    assert comment =~ "## OpenCode Handoff"
    assert comment =~ "Runner: OpenCode"
    assert comment =~ @project_root
    assert comment =~ "SYMLIVE-12"
    refute comment =~ "Fallback prompt must not be used"

    assert_receive {:memory_tracker_state_update, "issue-opencode-live", "In Review"}

    handoff_update = receive_runner_update("issue-opencode-live", :handoff_recorded, [:completed])
    assert handoff_update.event == :handoff_recorded
    assert handoff_update.runner_kind == "opencode"
    assert handoff_update.runner_owner == "opencode"
    assert handoff_update.result_state == "In Review"
    assert handoff_update.project_root == @project_root
    assert handoff_update.attach_url == server_url
    assert_session_id_shape(handoff_update.session_id)

    assert repo_status!() == before_status
  end

  defp receive_runner_update(issue_id, target_event, expected_prior_events, timeout \\ 600_000) do
    deadline = System.monotonic_time(:millisecond) + timeout
    receive_runner_update_until(issue_id, target_event, expected_prior_events, deadline)
  end

  defp receive_runner_update_until(issue_id, target_event, expected_prior_events, deadline) do
    remaining = max(deadline - System.monotonic_time(:millisecond), 0)

    receive do
      {:runner_worker_update, ^issue_id, %{event: ^target_event} = update} ->
        update

      {:runner_worker_update, ^issue_id, %{event: event} = update} ->
        if event in expected_prior_events do
          receive_runner_update_until(issue_id, target_event, expected_prior_events, deadline)
        else
          flunk("expected runner_worker_update event #{inspect(target_event)}, got #{inspect(update)}")
        end

      {:runner_worker_update, ^issue_id, update} ->
        flunk("expected runner_worker_update event #{inspect(target_event)}, got #{inspect(update)}")
    after
      remaining ->
        flunk("expected runner_worker_update event #{inspect(target_event)} within #{remaining}ms")
    end
  end

  defp required_live_opencode_command! do
    unless System.get_env("SYMPHONY_OPENCODE_LIVE") == "1" do
      flunk("set SYMPHONY_OPENCODE_LIVE=1 to run the opt-in OpenCode live gate")
    end

    command = System.get_env("OPENCODE_COMMAND") || ""

    unless Path.expand(command) == @opencode_command do
      flunk("set OPENCODE_COMMAND=#{@opencode_command} to prove the canonical OpenCode binary")
    end

    unless File.exists?(command) do
      flunk("#{command} does not exist")
    end

    command
  end

  defp live_server_url do
    case System.get_env("OPENCODE_SERVER_URL") do
      value when is_binary(value) and value != "" -> value
      _ -> nil
    end
  end

  defp assert_opencode_command(command_args, command, server_url) do
    assert [
             ^command,
             "run",
             "--dir",
             @project_root,
             "--agent",
             "build",
             "--format",
             "json",
             "--title",
             title | attach_args
           ] = command_args

    assert title =~ "SYMLIVE-12 Validate OpenCode live gate "
    assert title =~ ~r/^SYMLIVE-12 Validate OpenCode live gate \[slice=live-gate fp=[0-9a-f]{12}\]$/

    case server_url do
      value when is_binary(value) and value != "" ->
        assert_attach_args(attach_args, value)

      _ ->
        assert attach_args == []
    end
  end

  defp assert_attach_args(["--attach", attach_url], expected_url) do
    assert attach_url == expected_url
  end

  defp assert_attach_args(["--session", session_id, "--attach", attach_url], expected_url) do
    assert_session_id_shape(session_id)
    assert attach_url == expected_url
  end

  defp assert_attach_args(attach_args, _expected_url) do
    flunk("unexpected OpenCode attach arguments: #{inspect(attach_args)}")
  end

  defp assert_session_id_shape(nil), do: :ok

  defp assert_session_id_shape(session_id) when is_binary(session_id) do
    assert session_id != ""
    assert session_id =~ ~r/^[A-Za-z0-9_-]+$/
  end

  defp live_task_packet do
    """
    <!-- symphony:opencode-task-prompt:v1 slice_id=live-gate -->
    ```text
    SYM-12 OpenCode live validation gate.

    This is an evidence-only, no-edit prompt. Do not modify, stage, commit, push,
    reset, clean, revert, start services, stop services, or mutate Linear.

    Report only concise visible evidence that the real OpenCode build route ran:
    - title: SYMLIVE-12 Validate OpenCode live gate [slice=live-gate fp=<12hex>]
    - agent: build
    - directory: /home/agent/proj/symphony
    - session id if available to you
    - final status suitable for Symphony handoff
    ```
    """
  end

  defp repo_status! do
    {status, 0} = System.cmd("git", ["-C", @project_root, "status", "--short"], stderr_to_stdout: true)
    status
  end
end
