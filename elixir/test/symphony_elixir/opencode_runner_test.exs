defmodule SymphonyElixir.OpenCodeRunnerTest do
  use SymphonyElixir.TestSupport

  alias SymphonyElixir.OpenCode.{Runner, TaskPrompt}

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
             "opencode",
             "run",
             "--dir",
             "/tmp/workspace",
             "--agent",
             "build",
             "--format",
             "json",
             "--title",
             "NER-1 Implement scoped change"
           ] = received_args

    assert prompt_path =~ "symphony-opencode-prompt-"
    refute Enum.member?(received_args, "prompt body")
    assert opts[:cd] == "/tmp/workspace"
    assert opts[:stderr_to_stdout] == true
  end

  test "opencode runner attaches to configured OpenCode server" do
    issue = %Issue{
      id: "issue-1",
      identifier: "NER-1",
      title: "Implement scoped change",
      description: "Task packet from Codex Architect",
      state: "In Progress"
    }

    test_pid = self()

    write_workflow_file!(
      Workflow.workflow_file_path(),
      opencode_server_url: "http://127.0.0.1:3000",
      prompt: "OpenCode prompt for {{ issue.identifier }}"
    )

    assert {:ok, %{output: "done\n"}} =
             Runner.run("/tmp/workspace", issue, "prompt body",
               runner: fn command, args, opts ->
                 prompt = File.read!(Enum.at(args, 3))
                 send(test_pid, {:opencode_called, command, args, opts, prompt})
                 {"done\n", 0}
               end
             )

    assert_received {:opencode_called, "bash", received_args, opts, "prompt body"}

    assert received_args == [
             "-lc",
             "prompt_file=$1; shift; exec \"$@\" < \"$prompt_file\"",
             "symphony-opencode",
             Enum.at(received_args, 3),
             "opencode",
             "run",
             "--dir",
             "/tmp/workspace",
             "--agent",
             "build",
             "--format",
             "json",
             "--title",
             "NER-1 Implement scoped change",
             "--attach",
             "http://127.0.0.1:3000"
           ]

    refute Enum.member?(received_args, "prompt body")
    assert opts[:cd] == "/tmp/workspace"
  end

  test "opencode runner resumes completed visible session from OpenCode state without replaying prompt" do
    issue = %Issue{
      id: "issue-1",
      identifier: "NER-13",
      title: "Repair WCB target-closure executable readiness",
      description: "Task packet from Codex Architect",
      state: "In Progress"
    }

    test_pid = self()

    write_workflow_file!(
      Workflow.workflow_file_path(),
      opencode_project_root: "/home/agent/proj/mnemesh",
      opencode_server_url: "http://127.0.0.1:3000",
      prompt: "OpenCode prompt for {{ issue.identifier }}"
    )

    assert {:ok, %{output: "completed handoff", command: ["opencode", "session", "resume-result", "ses_newest"]}} =
             Runner.run("/tmp/symphony/workspaces/mnemesh/NER-13", issue, "prompt body",
               session_lister: fn command, execution_dir, title ->
                 send(test_pid, {:session_lister_called, command, execution_dir, title})

                 {:ok,
                  [
                    %{
                      "id" => "ses_older",
                      "title" => "NER-13 Repair WCB target-closure executable readiness",
                      "directory" => "/home/agent/proj/mnemesh",
                      "updated" => 10
                    },
                    %{
                      "id" => "ses_newest",
                      "title" => "NER-13 Repair WCB target-closure executable readiness",
                      "directory" => "/home/agent/proj/mnemesh",
                      "updated" => 20
                    },
                    %{
                      "id" => "ses_other_project",
                      "title" => "NER-13 Repair WCB target-closure executable readiness",
                      "directory" => "/home/agent/proj/other",
                      "updated" => 30
                    }
                  ]}
               end,
               session_result_reader: fn execution_dir, session_id ->
                 send(test_pid, {:session_result_reader_called, execution_dir, session_id})
                 {:ok, "completed handoff"}
               end
             )

    assert_received {:session_lister_called, "opencode", "/home/agent/proj/mnemesh", "NER-13 Repair WCB target-closure executable readiness"}
    assert_received {:session_result_reader_called, "/home/agent/proj/mnemesh", "ses_newest"}
    refute_received {:opencode_called, _command, _args, _opts, _prompt}
  end

  test "opencode runner reuses newest visible session with matching issue identifier prefix" do
    issue = %Issue{
      id: "issue-1",
      identifier: "NER-27",
      title: "Implement WCB target acquisition after NER-27 benchmark",
      description: "Task packet from Codex Architect",
      state: "In Progress"
    }

    write_workflow_file!(
      Workflow.workflow_file_path(),
      opencode_project_root: "/home/agent/proj/mnemesh",
      opencode_server_url: "http://127.0.0.1:3000",
      prompt: "OpenCode prompt for {{ issue.identifier }}"
    )

    test_pid = self()

    assert {:ok, %{output: "continued\n", command: _command}} =
             Runner.run("/tmp/symphony/workspaces/mnemesh/NER-27", issue, "prompt body",
               session_lister: fn _command, _execution_dir, _title ->
                 {:ok,
                  [
                    %{
                      "id" => "ses_exact_older",
                      "title" => "NER-27 Implement WCB target acquisition after NER-27 benchmark",
                      "directory" => "/home/agent/proj/mnemesh",
                      "updated" => 10
                    },
                    %{
                      "id" => "ses_manual_newest",
                      "title" => "NER-27 WCB target acquisition implementation",
                      "directory" => "/home/agent/proj/mnemesh",
                      "updated" => 20
                    }
                  ]}
               end,
               session_result_reader: fn _execution_dir, session_id ->
                 {:error, {:opencode_session_handoff_incomplete, session_id}}
               end,
               runner: fn command, received_args, opts ->
                 send(test_pid, {:opencode_called, command, received_args, opts})
                 {"continued\n", 0}
               end
             )

    assert_receive {:opencode_called, "bash", received_args, opts}
    assert opts[:cd] == "/home/agent/proj/mnemesh"
    assert "--session" in received_args
    assert "ses_manual_newest" in received_args
  end

  test "opencode runner continues when existing session has no completed assistant text yet" do
    issue = %Issue{
      id: "issue-1",
      identifier: "NER-27",
      title: "Implement WCB target acquisition after NER-27 benchmark",
      description: "Task packet from Codex Architect",
      state: "In Progress"
    }

    data_home = Path.join(System.tmp_dir!(), "opencode-runner-xdg-#{System.unique_integer([:positive, :monotonic])}")
    db_dir = Path.join([data_home, "opencode"])
    db_path = Path.join(db_dir, "opencode.db")
    File.mkdir_p!(db_dir)

    previous_xdg_data_home = System.get_env("XDG_DATA_HOME")
    System.put_env("XDG_DATA_HOME", data_home)

    on_exit(fn ->
      if previous_xdg_data_home do
        System.put_env("XDG_DATA_HOME", previous_xdg_data_home)
      else
        System.delete_env("XDG_DATA_HOME")
      end

      File.rm_rf(data_home)
    end)

    sql = """
    create table session (
      id text primary key,
      title text,
      directory text,
      time_created integer,
      time_updated integer,
      summary_files integer,
      summary_additions integer,
      summary_deletions integer,
      tokens_input integer,
      tokens_output integer
    );

    create table message (
      id text primary key,
      session_id text,
      time_created integer,
      data text
    );

    create table part (
      id text primary key,
      message_id text,
      time_created integer,
      data text
    );

    insert into session
      (id,title,directory,time_created,time_updated,summary_files,summary_additions,summary_deletions,tokens_input,tokens_output)
      values
      ('ses_running','NER-27 WCB target acquisition implementation','/home/agent/proj/mnemesh',1,2,0,0,0,0,0);
    """

    assert {"", 0} = System.cmd("sqlite3", [db_path, sql], stderr_to_stdout: true)

    write_workflow_file!(
      Workflow.workflow_file_path(),
      opencode_project_root: "/home/agent/proj/mnemesh",
      opencode_server_url: "http://127.0.0.1:3000",
      prompt: "OpenCode prompt for {{ issue.identifier }}"
    )

    test_pid = self()

    assert {:ok, %{output: "continued\n", command: command}} =
             Runner.run("/tmp/symphony/workspaces/mnemesh/NER-27", issue, "prompt body",
               session_lister: fn _command, _execution_dir, _title ->
                 {:ok,
                  [
                    %{
                      "id" => "ses_running",
                      "title" => "NER-27 WCB target acquisition implementation",
                      "directory" => "/home/agent/proj/mnemesh",
                      "updated" => 20
                    }
                  ]}
               end,
               runner: fn command, received_args, opts ->
                 prompt_file = Enum.at(received_args, 3)
                 prompt = File.read!(prompt_file)
                 send(test_pid, {:opencode_called, command, received_args, opts, prompt})
                 {"continued\n", 0}
               end
             )

    assert command == [
             "opencode",
             "run",
             "--dir",
             "/home/agent/proj/mnemesh",
             "--agent",
             "build",
             "--format",
             "json",
             "--title",
             "NER-27 Implement WCB target acquisition after NER-27 benchmark",
             "--session",
             "ses_running",
             "--attach",
             "http://127.0.0.1:3000"
           ]

    assert_receive {:opencode_called, "bash", _received_args, opts, prompt}
    assert opts[:cd] == "/home/agent/proj/mnemesh"
    assert prompt =~ "Continue the existing OpenCode task"
    refute prompt =~ "prompt body"
  end

  test "opencode runner waits for an existing session when latest handoff is incomplete" do
    issue = %Issue{
      id: "issue-1",
      identifier: "NER-13",
      title: "Repair WCB target-closure executable readiness",
      description: "Task packet from Codex Architect",
      state: "In Progress"
    }

    test_pid = self()

    write_workflow_file!(
      Workflow.workflow_file_path(),
      opencode_project_root: "/home/agent/proj/mnemesh",
      opencode_server_url: "http://127.0.0.1:3000",
      prompt: "OpenCode prompt for {{ issue.identifier }}"
    )

    assert {:ok, %{output: "continued\n", command: command}} =
             Runner.run("/tmp/symphony/workspaces/mnemesh/NER-13", issue, "original full prompt",
               session_lister: fn _command, _execution_dir, _title ->
                 {:ok,
                  [
                    %{
                      "id" => "ses_incomplete",
                      "title" => "NER-13 Repair WCB target-closure executable readiness",
                      "directory" => "/home/agent/proj/mnemesh",
                      "updated" => 20
                    }
                  ]}
               end,
               session_result_reader: fn _execution_dir, session_id ->
                 {:error, {:opencode_session_handoff_incomplete, session_id}}
               end,
               runner: fn command, received_args, opts ->
                 prompt_file = Enum.at(received_args, 3)
                 prompt = File.read!(prompt_file)
                 send(test_pid, {:opencode_called, command, received_args, opts, prompt})
                 {"continued\n", 0}
               end
             )

    assert command == [
             "opencode",
             "run",
             "--dir",
             "/home/agent/proj/mnemesh",
             "--agent",
             "build",
             "--format",
             "json",
             "--title",
             "NER-13 Repair WCB target-closure executable readiness",
             "--session",
             "ses_incomplete",
             "--attach",
             "http://127.0.0.1:3000"
           ]

    assert_receive {:opencode_called, "bash", received_args, opts, prompt}
    assert opts[:cd] == "/home/agent/proj/mnemesh"

    assert received_args == [
             "-lc",
             "prompt_file=$1; shift; exec \"$@\" < \"$prompt_file\"",
             "symphony-opencode",
             Enum.at(received_args, 3),
             "opencode",
             "run",
             "--dir",
             "/home/agent/proj/mnemesh",
             "--agent",
             "build",
             "--format",
             "json",
             "--title",
             "NER-13 Repair WCB target-closure executable readiness",
             "--session",
             "ses_incomplete",
             "--attach",
             "http://127.0.0.1:3000"
           ]

    assert prompt =~ "Continue the existing OpenCode task"
    refute prompt =~ "original full prompt"
  end

  test "opencode runner continues when existing session asks for owner clarification" do
    issue = %Issue{
      id: "issue-1",
      identifier: "NER-13",
      title: "Repair WCB target-closure executable readiness",
      description: "Task packet from Codex Architect",
      state: "In Progress"
    }

    test_pid = self()
    data_home = Path.join(System.tmp_dir!(), "opencode-runner-xdg-#{System.unique_integer([:positive, :monotonic])}")
    db_dir = Path.join([data_home, "opencode"])
    db_path = Path.join(db_dir, "opencode.db")
    File.mkdir_p!(db_dir)

    previous_xdg_data_home = System.get_env("XDG_DATA_HOME")
    System.put_env("XDG_DATA_HOME", data_home)

    on_exit(fn ->
      if previous_xdg_data_home do
        System.put_env("XDG_DATA_HOME", previous_xdg_data_home)
      else
        System.delete_env("XDG_DATA_HOME")
      end

      File.rm_rf(data_home)
    end)

    assistant_text =
      "Есть блокер перед финальным принятием: evaluator вернул `revise`.\n\n" <>
        "Нужно уточнение: можно откатить `WORKFLOW.md` к HEAD?"

    message_data = Jason.encode!(%{"role" => "assistant", "finish" => "stop"})
    part_data = Jason.encode!(%{"type" => "text", "text" => assistant_text})

    sql = """
    create table session (
      id text primary key,
      title text,
      directory text,
      time_created integer,
      time_updated integer,
      summary_files integer,
      summary_additions integer,
      summary_deletions integer,
      tokens_input integer,
      tokens_output integer
    );

    create table message (
      id text primary key,
      session_id text,
      time_created integer,
      data text
    );

    create table part (
      id text primary key,
      message_id text,
      time_created integer,
      data text
    );

    insert into session
      (id,title,directory,time_created,time_updated,summary_files,summary_additions,summary_deletions,tokens_input,tokens_output)
      values
      ('ses_owner_question','NER-13 Repair WCB target-closure executable readiness','/home/agent/proj/mnemesh',1,2,4,10,2,100,20);

    insert into message (id,session_id,time_created,data)
      values ('msg_owner_question','ses_owner_question',2,#{sql_quote(message_data)});

    insert into part (id,message_id,time_created,data)
      values ('part_owner_question','msg_owner_question',3,#{sql_quote(part_data)});
    """

    assert {"", 0} = System.cmd("sqlite3", [db_path, sql], stderr_to_stdout: true)

    write_workflow_file!(
      Workflow.workflow_file_path(),
      opencode_project_root: "/home/agent/proj/mnemesh",
      opencode_server_url: "http://127.0.0.1:3000",
      prompt: "OpenCode prompt for {{ issue.identifier }}"
    )

    assert {:ok, %{output: "continued\n", command: _command}} =
             Runner.run("/tmp/symphony/workspaces/mnemesh/NER-13", issue, "original full prompt",
               session_lister: fn _command, _execution_dir, _title ->
                 {:ok,
                  [
                    %{
                      "id" => "ses_owner_question",
                      "title" => "NER-13 Repair WCB target-closure executable readiness",
                      "directory" => "/home/agent/proj/mnemesh",
                      "updated" => 20
                    }
                  ]}
               end,
               runner: fn command, received_args, opts ->
                 prompt_file = Enum.at(received_args, 3)
                 prompt = File.read!(prompt_file)
                 send(test_pid, {:opencode_called, command, received_args, opts, prompt})
                 {"continued\n", 0}
               end
             )

    assert_receive {:opencode_called, "bash", received_args, opts, prompt}
    assert opts[:cd] == "/home/agent/proj/mnemesh"
    assert "--session" in received_args
    assert "ses_owner_question" in received_args
    assert prompt =~ "Continue the existing OpenCode task"
    assert prompt =~ "no unresolved question"
  end

  test "opencode runner uses configured project root for visible shared sessions" do
    issue = %Issue{
      id: "issue-1",
      identifier: "NER-1",
      title: "Implement scoped change",
      description: "Task packet from Codex Architect",
      state: "In Progress"
    }

    test_pid = self()

    write_workflow_file!(
      Workflow.workflow_file_path(),
      opencode_project_root: "/home/agent/proj/nervure",
      opencode_server_url: "http://127.0.0.1:3000",
      prompt: "OpenCode prompt for {{ issue.identifier }}"
    )

    assert {:ok, %{output: "done\n"}} =
             Runner.run("/tmp/symphony/workspaces/nervure/NER-1", issue, "prompt body",
               runner: fn command, args, opts ->
                 prompt = File.read!(Enum.at(args, 3))
                 send(test_pid, {:opencode_called, command, args, opts, prompt})
                 {"done\n", 0}
               end
             )

    assert_received {:opencode_called, "bash", received_args, opts, "prompt body"}

    assert received_args == [
             "-lc",
             "prompt_file=$1; shift; exec \"$@\" < \"$prompt_file\"",
             "symphony-opencode",
             Enum.at(received_args, 3),
             "opencode",
             "run",
             "--dir",
             "/home/agent/proj/nervure",
             "--agent",
             "build",
             "--format",
             "json",
             "--title",
             "NER-1 Implement scoped change",
             "--attach",
             "http://127.0.0.1:3000"
           ]

    refute Enum.member?(received_args, "prompt body")
    assert opts[:cd] == "/home/agent/proj/nervure"
  end

  test "task prompt extracts architect-authored OpenCode packet from marked comment" do
    body = """
    Codex notes before the handoff.

    <!-- symphony:opencode-task-prompt:v1 slice_id=cli-test-boundary -->
    ```text
    Task: implement the exact scoped repair

    Validation:
    - cargo check
    ```
    """

    assert {:ok, prompt} = TaskPrompt.extract(body)
    assert prompt =~ "Task: implement the exact scoped repair"
    assert prompt =~ "Validation:"
    refute prompt =~ "symphony:opencode-task-prompt"

    assert {:ok, packet} = TaskPrompt.extract_packet(body)
    assert packet.slice_id == "cli-test-boundary"
    assert is_binary(packet.fingerprint)
  end

  test "task prompt requires an explicit architect marker" do
    assert {:error, :opencode_task_prompt_not_found} =
             TaskPrompt.extract("Task: this should not be picked up implicitly")
  end

  test "agent runner blocks third same-slice OpenCode dispatch and moves issue to RCA Required" do
    workspace_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-opencode-loop-breaker-#{System.unique_integer([:positive])}"
      )

    command = Path.join(workspace_root, "fake-opencode")
    File.mkdir_p!(workspace_root)

    File.write!(command, """
    #!/usr/bin/env bash
    printf 'this should not run\\n'
    exit 7
    """)

    File.chmod!(command, 0o755)

    write_workflow_file!(Workflow.workflow_file_path(),
      tracker_kind: "memory",
      workspace_root: Path.join(workspace_root, "workspaces"),
      runner_routes: %{"In Progress" => "opencode", "RCA Required" => "codex"},
      opencode_command: command,
      opencode_result_state: "In Review",
      process_policy_rca_required_state: "RCA Required",
      process_policy_max_rejections_per_slice: 2,
      prompt: "OpenCode prompt for {{ issue.identifier }}: {{ issue.description }}"
    )

    Application.put_env(:symphony_elixir, :memory_tracker_recipient, self())

    Application.put_env(:symphony_elixir, :memory_tracker_opencode_comments, %{
      "issue-1" => [
        """
        <!-- symphony:review-decision:v1 -->
        ```text
        status: rejected
        slice_id: cli-test-boundary
        reason: first miss
        ```
        """,
        """
        <!-- symphony:review-decision:v1 -->
        ```text
        status: rejected
        slice_id: cli-test-boundary
        reason: second miss
        ```
        """,
        """
        <!-- symphony:opencode-task-prompt:v1 slice_id=cli-test-boundary -->
        ```text
        Architect-authored full OpenCode prompt
        ```
        """
      ]
    })

    issue = %Issue{
      id: "issue-1",
      identifier: "NER-43",
      title: "Run OpenCode after repeated reject",
      description: "Implement via OpenCode",
      state: "In Progress"
    }

    assert :ok = AgentRunner.run(issue, nil)

    assert_receive {:memory_tracker_comment, "issue-1", comment}
    assert comment =~ "Symphony Stop Rule"
    assert comment =~ "repair loop breaker"
    assert comment =~ "cli-test-boundary"
    refute comment =~ "this should not run"

    assert_receive {:memory_tracker_state_update, "issue-1", "RCA Required"}
    refute_received {:memory_tracker_state_update, "issue-1", "In Review"}
  end

  test "codex runner stops when refreshed issue routes to OpenCode" do
    workspace_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-codex-to-opencode-handoff-#{System.unique_integer([:positive])}"
      )

    codex_binary = Path.join(workspace_root, "fake-codex")
    trace_file = Path.join(workspace_root, "codex.trace")

    File.mkdir_p!(workspace_root)

    File.write!(codex_binary, """
    #!/usr/bin/env bash
    trace_file="#{trace_file}"
    count=0

    while IFS= read -r line; do
      count=$((count + 1))
      echo "$line" >> "$trace_file"

      case "$count" in
        1) printf '%s\n' '{"id":1,"result":{}}' ;;
        2) printf '%s\n' '{"id":2,"result":{"thread":{"id":"architect-thread"}}}' ;;
        3) printf '%s\n' '{"id":3,"result":{"turn":{"id":"architect-turn"}}}' ;;
        4) printf '%s\n' '{"method":"turn/completed"}'; exit 0 ;;
        *) printf '%s\n' '{"id":999,"error":{"message":"unexpected continuation"}}'; exit 2 ;;
      esac
    done
    """)

    File.chmod!(codex_binary, 0o755)

    write_workflow_file!(Workflow.workflow_file_path(),
      tracker_kind: "memory",
      workspace_root: Path.join(workspace_root, "workspaces"),
      runner_routes: %{"Todo" => "codex", "In Progress" => "opencode"},
      codex_command: codex_binary,
      max_turns: 2,
      prompt: "Architect prompt for {{ issue.identifier }}"
    )

    Application.put_env(:symphony_elixir, :memory_tracker_issues, [
      %Issue{
        id: "issue-1",
        identifier: "NER-8",
        title: "Implementation handoff",
        description: "Ready for OpenCode",
        state: "In Progress"
      }
    ])

    issue = %Issue{
      id: "issue-1",
      identifier: "NER-8",
      title: "Owner input pulse",
      description: "Owner answered",
      state: "Todo"
    }

    assert :ok = AgentRunner.run(issue, nil)

    trace = File.read!(trace_file)
    assert trace =~ "turn/start"
    assert length(Regex.scan(~r/turn\/start/, trace)) == 1
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
    printf 'stdin:\\n'
    cat
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

    Application.put_env(:symphony_elixir, :memory_tracker_opencode_comments, %{
      "issue-1" => [
        """
        <!-- symphony:opencode-task-prompt:v1 -->
        ```text
        Architect-authored full OpenCode prompt

        Repo: /tmp/workspace
        Validation: exact command list
        ```
        """
      ]
    })

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
    assert comment =~ "Architect-authored full OpenCode prompt"
    refute comment =~ "OpenCode prompt for NER-42"

    assert_receive {:memory_tracker_state_update, "issue-1", "In Review"}
  end

  defp sql_quote(value), do: "'" <> String.replace(value, "'", "''") <> "'"
end
