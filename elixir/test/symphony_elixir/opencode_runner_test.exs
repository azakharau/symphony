defmodule SymphonyElixir.OpenCodeRunnerTest do
  use SymphonyElixir.TestSupport

  alias SymphonyElixir.OpenCode.{Runner, TaskPrompt}
  alias SymphonyElixir.ReviewDecision
  alias SymphonyElixir.Runner.{CodexAdapter, OpenCodeAdapter, OpenCodeDispatch}
  alias SymphonyElixir.Runner.Outcome

  defmodule DispatchLinearClient do
    def fetch_candidate_issues, do: {:ok, []}
    def fetch_issues_by_states(_states), do: {:ok, []}
    def fetch_issue_states_by_ids(_issue_ids), do: {:ok, []}
    def fetch_project_milestones, do: {:ok, []}

    def graphql(query, variables) do
      send(self(), {:dispatch_graphql_called, query, variables})

      case Process.get({__MODULE__, :graphql_results}) do
        [result | rest] ->
          Process.put({__MODULE__, :graphql_results}, rest)
          result

        _ ->
          Process.get({__MODULE__, :graphql_result})
      end
    end
  end

  setup do
    linear_client_module = Application.get_env(:symphony_elixir, :linear_client_module)

    on_exit(fn ->
      if is_nil(linear_client_module) do
        Application.delete_env(:symphony_elixir, :linear_client_module)
      else
        Application.put_env(:symphony_elixir, :linear_client_module, linear_client_module)
      end
    end)

    :ok
  end

  test "runner outcome carries normalized policy block failure details" do
    assert %Outcome{
             kind: :policy_blocked,
             reason: :remote_runner_unsupported,
             detail: "builder-1",
             result_state: "RCA Required",
             failure: %{
               reason: :remote_runner_unsupported,
               detail: "builder-1",
               workspace: "/tmp/workspace"
             }
           } =
             Outcome.policy_blocked(
               reason: :remote_runner_unsupported,
               detail: "builder-1",
               result_state: "RCA Required",
               failure: %{workspace: "/tmp/workspace"}
             )
  end

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

  test "opencode runner rejects non-local worker_host before invoking local command" do
    issue = %Issue{
      id: "issue-remote",
      identifier: "NER-REMOTE",
      title: "Implement remotely",
      description: "Task packet from Codex Architect",
      state: "In Progress"
    }

    assert {:error, {:opencode_remote_worker_host_unsupported, %{worker_host: "builder-1", workspace: "/tmp/workspace", project_root: "/tmp/workspace"}}} =
             Runner.run("/tmp/workspace", issue, "prompt body",
               worker_host: "builder-1",
               runner: fn _command, _args, _opts ->
                 flunk("local OpenCode command must not be invoked for a remote worker_host")
               end
             )
  end

  test "OpenCode dispatch durably blocks remote worker_host without invoking command" do
    workspace_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-opencode-remote-block-#{System.unique_integer([:positive])}"
      )

    command = Path.join(workspace_root, "fake-opencode")
    trace_file = Path.join(workspace_root, "opencode.trace")
    File.mkdir_p!(workspace_root)

    File.write!(command, """
    #!/usr/bin/env bash
    echo started > #{trace_file}
    printf 'this should not run\n'
    exit 7
    """)

    File.chmod!(command, 0o755)

    write_workflow_file!(Workflow.workflow_file_path(),
      tracker_kind: "memory",
      workspace_root: Path.join(workspace_root, "workspaces"),
      runner_routes: %{"In Progress" => "opencode"},
      opencode_command: command,
      prompt: "OpenCode prompt for {{ issue.identifier }}"
    )

    Application.put_env(:symphony_elixir, :memory_tracker_recipient, self())

    Application.put_env(:symphony_elixir, :memory_tracker_opencode_comments, %{
      "issue-remote" => [
        """
        <!-- symphony:opencode-task-prompt:v1 slice_id=remote-guard -->
        ```text
        Architect-authored full OpenCode prompt
        ```
        """
      ]
    })

    issue = %Issue{
      id: "issue-remote",
      identifier: "NER-REMOTE",
      title: "Run OpenCode on selected remote host",
      description: "Implement via OpenCode",
      state: "In Progress"
    }

    assert %Outcome{kind: :rerouted, reason: :opencode_remote_worker_host_unsupported, result_state: "RCA Required"} =
             OpenCodeDispatch.run(%{
               workspace: "/tmp/workspace",
               issue: issue,
               opts: [],
               worker_host: "builder-1",
               emit_update: fn update ->
                 send(self(), {:runner_update, update})
                 :ok
               end
             })

    assert_receive {:runner_update,
                    %{
                      event: :remote_worker_host_blocked,
                      phase: :policy_blocked,
                      outcome: :rerouted,
                      result_state: "RCA Required",
                      worker_host: "builder-1",
                      failure: %{reason: :opencode_remote_worker_host_unsupported}
                    }}

    assert_receive {:memory_tracker_comment, "issue-remote", comment}
    assert comment =~ "OpenCode remote worker host is not supported"
    assert comment =~ "builder-1"
    assert comment =~ "/tmp/workspace"
    refute comment =~ "this should not run"
    assert_receive {:memory_tracker_state_update, "issue-remote", "RCA Required"}
    refute File.exists?(trace_file)
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

  test "opencode runner reuses newest visible session with exact legacy title" do
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
    assert "ses_exact_older" in received_args
  end

  test "opencode runner does not reuse completed session when packet fingerprint differs" do
    issue = %Issue{id: "issue-1", identifier: "NER-28", title: "Implement scoped change", state: "In Progress"}
    packet = task_packet("same-slice", "new prompt body")
    old_packet = task_packet("same-slice", "old prompt body")
    old_title = "NER-28 Implement scoped change [#{TaskPrompt.title_suffix(old_packet)}]"

    write_workflow_file!(Workflow.workflow_file_path(),
      opencode_project_root: "/home/agent/proj/mnemesh",
      opencode_server_url: "http://127.0.0.1:3000",
      prompt: "OpenCode prompt"
    )

    test_pid = self()

    assert {:ok, %{output: "fresh\n", session_id: nil}} =
             Runner.run("/tmp/symphony/workspaces/mnemesh/NER-28", issue, packet,
               session_lister: fn _command, _execution_dir, _title ->
                 {:ok, [%{"id" => "ses_old", "title" => old_title, "directory" => "/home/agent/proj/mnemesh", "updated" => 20}]}
               end,
               session_result_reader: fn _execution_dir, _session_id -> flunk("stale session must not be read") end,
               runner: fn command, received_args, opts ->
                 send(test_pid, {:opencode_called, command, received_args, opts})
                 {"fresh\n", 0}
               end
             )

    assert_receive {:opencode_called, "bash", received_args, _opts}
    refute "--session" in received_args
    refute "ses_old" in received_args
  end

  test "opencode runner reuses matching packet identity session and captures fresh session id from output" do
    issue = %Issue{id: "issue-1", identifier: "NER-29", title: "Implement scoped change", state: "In Progress"}
    packet = task_packet("same-slice", "matching prompt body")
    title = "NER-29 Implement scoped change [#{TaskPrompt.title_suffix(packet)}]"

    write_workflow_file!(Workflow.workflow_file_path(),
      opencode_project_root: "/home/agent/proj/mnemesh",
      opencode_server_url: "http://127.0.0.1:3000",
      prompt: "OpenCode prompt"
    )

    assert {:ok, %{output: "completed handoff", session_id: "ses_match"}} =
             Runner.run("/tmp/symphony/workspaces/mnemesh/NER-29", issue, packet,
               session_lister: fn _command, _execution_dir, ^title ->
                 {:ok, [%{"id" => "ses_match", "title" => title, "directory" => "/home/agent/proj/mnemesh", "updated" => 20}]}
               end,
               session_result_reader: fn "/home/agent/proj/mnemesh", "ses_match" -> {:ok, "completed handoff"} end
             )

    fresh_packet = task_packet("fresh-slice", "fresh prompt body")

    assert {:ok, %{session_id: "ses_fresh"}} =
             Runner.run("/tmp/symphony/workspaces/mnemesh/NER-29", issue, fresh_packet,
               runner: fn _command, _received_args, _opts ->
                 {~s({"session":{"id":"ses_fresh"}}) <> "\ncompleted\n", 0}
               end
             )
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
                      "title" => "NER-27 Implement WCB target acquisition after NER-27 benchmark",
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

  test "task prompt preserves inner markdown fences and requires slice id" do
    body = """
    <!-- symphony:opencode-task-prompt:v1 slice_id=fenced-slice -->
    ```text
    Run this command:

    ```bash
    mix test
    ```

    Then edit this markdown:

    ```markdown
    # nested doc
    ```
    ```
    """

    assert {:ok, packet} = TaskPrompt.extract_packet(body)
    assert packet.slice_id == "fenced-slice"
    assert packet.prompt =~ "```bash\nmix test\n```"
    assert packet.prompt =~ "```markdown\n# nested doc\n```"

    missing_slice = String.replace(body, " slice_id=fenced-slice", "")
    assert {:error, :opencode_task_prompt_missing_slice_id} = TaskPrompt.extract_packet(missing_slice)

    blank_slice = String.replace(body, "slice_id=fenced-slice", "slice_id=\"\"")
    assert {:error, :opencode_task_prompt_missing_slice_id} = TaskPrompt.extract_packet(blank_slice)
  end

  test "task prompt requires an explicit architect marker" do
    assert {:error, :opencode_task_prompt_not_found} =
             TaskPrompt.extract("Task: this should not be picked up implicitly")
  end

  test "task prompt rejects non-binary and malformed packet inputs" do
    assert {:error, :opencode_task_prompt_not_found} = TaskPrompt.extract(nil)
    refute TaskPrompt.marker_present?(nil)
    assert {:error, :opencode_task_prompt_not_found} = TaskPrompt.extract_packet(%{})

    assert {:error, :opencode_task_prompt_malformed_fence} =
             TaskPrompt.extract_packet("""
             <!-- symphony:opencode-task-prompt:v1 slice_id=bad-fence -->
             Prompt without an opening fence.
             ```
             """)

    assert {:ok, packet} =
             TaskPrompt.extract_packet("""
             <!-- symphony:opencode-task-prompt:v1 slice_id="quoted-slice" -->
             ```text
             quoted slice prompt
             ```
             """)

    assert packet.slice_id == "quoted-slice"
  end

  test "review decision parser handles single, invalid, and statusless payloads" do
    body = """
    <!-- symphony:review-decision:v1 -->
    ```yaml
    slice_id: sym-5
    reason: missing tests
    ```

    <!-- symphony:review-decision:v1 -->
    ```yaml
    status: APPROVED
    slice_id: sym-5
    reason: fixed
    ```
    """

    assert [%ReviewDecision{status: "approved", slice_id: "sym-5", reason: "fixed"}] =
             ReviewDecision.extract_many(body)

    assert [] = ReviewDecision.extract_many(nil)
    assert [] = ReviewDecision.extract_many([nil])
  end

  test "opencode adapter requires an architect packet in context" do
    assert {:error, :opencode_task_packet_required} = OpenCodeAdapter.run(%{})
  end

  test "process policy blocks packets with missing slice id before rejection counts" do
    packet = %TaskPrompt.Packet{prompt: "prompt", slice_id: nil, fingerprint: String.duplicate("a", 64)}

    assert {:block, %{reason: :opencode_task_prompt_missing_slice_id, rejection_count: 0}} =
             SymphonyElixir.ProcessPolicy.opencode_dispatch_decision(packet, [])
  end

  test "process policy allows nonmatching decisions and reports RCA route ownership" do
    packet = task_packet("target-slice", "prompt")

    decisions = [
      %ReviewDecision{status: "approved", slice_id: "target-slice", reason: "pass"},
      %ReviewDecision{status: "rejected", slice_id: "other-slice", reason: "different slice"},
      %{status: "rejected", slice_id: "target-slice"}
    ]

    assert :allow = SymphonyElixir.ProcessPolicy.opencode_dispatch_decision(packet, decisions)
    assert {:ok, "RCA Required"} = SymphonyElixir.ProcessPolicy.codex_owned_rca_required_state()
  end

  test "OpenCode dispatch passes through tracker and adapter errors" do
    issue = %Issue{id: "issue-dispatch", identifier: "NER-D", title: "Dispatch errors", state: "In Progress"}

    write_workflow_file!(Workflow.workflow_file_path(), tracker_kind: "linear", runner_routes: %{"In Progress" => "opencode"})
    Application.put_env(:symphony_elixir, :linear_client_module, DispatchLinearClient)

    Process.put({DispatchLinearClient, :graphql_result}, {:error, :linear_rate_limited})

    assert {:error, :linear_rate_limited} =
             OpenCodeDispatch.run(%{workspace: "/tmp/workspace", issue: issue, opts: [], emit_update: fn _ -> :ok end})

    Process.put(
      {DispatchLinearClient, :graphql_results},
      [
        {:ok,
         %{
           "data" => %{
             "issue" => %{
               "comments" => %{
                 "nodes" => [%{"body" => opencode_task_prompt_comment("dispatch-slice", "dispatch prompt"), "createdAt" => "2026-01-01T00:00:00Z"}],
                 "pageInfo" => %{"hasNextPage" => false, "endCursor" => nil}
               }
             }
           }
         }},
        {:error, :review_down}
      ]
    )

    assert {:error, :review_down} =
             OpenCodeDispatch.run(%{workspace: "/tmp/workspace", issue: issue, opts: [], emit_update: fn _ -> :ok end})

    workspace = Path.join(System.tmp_dir!(), "symphony-opencode-dispatch-workspace-#{System.unique_integer([:positive])}")
    File.mkdir_p!(workspace)
    on_exit(fn -> File.rm_rf(workspace) end)

    command = Path.join(System.tmp_dir!(), "symphony-opencode-dispatch-fail-#{System.unique_integer([:positive])}")
    File.write!(command, "#!/usr/bin/env bash\nprintf 'adapter failed'\nexit 9\n")
    File.chmod!(command, 0o755)
    on_exit(fn -> File.rm(command) end)

    write_workflow_file!(Workflow.workflow_file_path(),
      tracker_kind: "memory",
      runner_routes: %{"In Progress" => "opencode"},
      opencode_command: command
    )

    Application.put_env(:symphony_elixir, :memory_tracker_opencode_comments, %{
      "issue-dispatch" => [opencode_task_prompt_comment("dispatch-slice", "dispatch prompt")]
    })

    assert {:error, {:opencode_exit, 9, "adapter failed"}} =
             OpenCodeDispatch.run(%{workspace: workspace, issue: issue, opts: [], emit_update: fn _ -> :ok end})
  end

  test "OpenCode dispatch rejects non-Issue fallback" do
    assert {:error, :opencode_task_prompt_not_found} =
             OpenCodeDispatch.run(%{workspace: "/tmp/workspace", issue: %{id: "not-an-issue"}, opts: [], emit_update: fn _ -> :ok end})
  end

  test "agent runner reroutes missing OpenCode task prompt to Codex without running OpenCode" do
    workspace_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-opencode-missing-prompt-#{System.unique_integer([:positive])}"
      )

    command = Path.join(workspace_root, "fake-opencode")
    File.mkdir_p!(workspace_root)

    File.write!(command, """
    #!/usr/bin/env bash
    printf 'this should not run\n'
    exit 7
    """)

    File.chmod!(command, 0o755)

    write_workflow_file!(Workflow.workflow_file_path(),
      tracker_kind: "memory",
      workspace_root: Path.join(workspace_root, "workspaces"),
      runner_routes: %{"Todo" => "codex", "In Progress" => "opencode"},
      opencode_command: command,
      opencode_result_state: "In Review",
      prompt: "OpenCode prompt for {{ issue.identifier }}: {{ issue.description }}"
    )

    Application.put_env(:symphony_elixir, :memory_tracker_recipient, self())
    Application.put_env(:symphony_elixir, :memory_tracker_opencode_comments, %{"issue-1" => []})

    issue = %Issue{
      id: "issue-1",
      identifier: "NER-43",
      title: "Run post-slice benchmark",
      description: "Benchmark task accidentally left in OpenCode state",
      state: "In Progress"
    }

    assert :ok = AgentRunner.run(issue, nil)

    assert_receive {:memory_tracker_comment, "issue-1", comment}
    assert comment =~ "OpenCode task prompt missing"
    assert comment =~ "symphony:opencode-task-prompt:v1"
    assert comment =~ "Todo"
    refute comment =~ "this should not run"

    assert_receive {:memory_tracker_state_update, "issue-1", "Todo"}
    refute_received {:memory_tracker_state_update, "issue-1", "In Review"}
  end

  test "config rejects OpenCode default when RCA Required has no Codex route" do
    workspace_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-opencode-missing-prompt-no-codex-#{System.unique_integer([:positive])}"
      )

    command = Path.join(workspace_root, "fake-opencode")
    File.mkdir_p!(workspace_root)

    File.write!(command, """
    #!/usr/bin/env bash
    printf 'this should not run\n'
    exit 7
    """)

    File.chmod!(command, 0o755)

    write_workflow_file!(Workflow.workflow_file_path(),
      tracker_kind: "memory",
      workspace_root: Path.join(workspace_root, "workspaces"),
      runner_default: "opencode",
      runner_routes: %{},
      opencode_command: command,
      opencode_result_state: "In Review",
      prompt: "OpenCode prompt for {{ issue.identifier }}: {{ issue.description }}"
    )

    assert {:error, {:invalid_workflow_config, message}} = Config.settings()
    assert message =~ "process_policy.rca_required_state"
    assert message =~ "must route to codex"
    refute File.exists?(Path.join(workspace_root, "opencode.trace"))
  end

  test "agent runner durably blocks malformed OpenCode task packets before starting OpenCode" do
    workspace_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-opencode-malformed-prompt-#{System.unique_integer([:positive])}"
      )

    command = Path.join(workspace_root, "fake-opencode")
    trace_file = Path.join(workspace_root, "opencode.trace")
    File.mkdir_p!(workspace_root)

    File.write!(command, """
    #!/usr/bin/env bash
    echo started > #{trace_file}
    printf 'this should not run\n'
    exit 7
    """)

    File.chmod!(command, 0o755)

    write_workflow_file!(Workflow.workflow_file_path(),
      tracker_kind: "memory",
      workspace_root: Path.join(workspace_root, "workspaces"),
      runner_routes: %{"In Progress" => "opencode"},
      opencode_command: command,
      process_policy_rca_required_state: "RCA Required",
      prompt: "OpenCode prompt for {{ issue.identifier }}"
    )

    Application.put_env(:symphony_elixir, :memory_tracker_recipient, self())

    cases = [
      {"issue-missing-slice", "NER-45",
       """
       <!-- symphony:opencode-task-prompt:v1 -->
       ```text
       Missing required slice id
       ```
       """, :opencode_task_prompt_missing_slice_id},
      {"issue-blank-slice", "NER-46",
       """
       <!-- symphony:opencode-task-prompt:v1 slice_id=\"\" -->
       ```text
       Blank slice id
       ```
       """, :opencode_task_prompt_missing_slice_id},
      {"issue-empty-prompt", "NER-47",
       """
       <!-- symphony:opencode-task-prompt:v1 slice_id=empty-prompt -->
       ```text

       ```
       """, :opencode_task_prompt_empty},
      {"issue-malformed-fence", "NER-48",
       """
       <!-- symphony:opencode-task-prompt:v1 slice_id=bad-fence -->
       ```text
       Missing closing fence
       """, :opencode_task_prompt_malformed_fence}
    ]

    Application.put_env(
      :symphony_elixir,
      :memory_tracker_opencode_comments,
      Map.new(cases, fn {issue_id, _identifier, prompt_comment, _reason} -> {issue_id, [prompt_comment]} end)
    )

    for {issue_id, identifier, _prompt_comment, reason} <- cases do
      issue = %Issue{
        id: issue_id,
        identifier: identifier,
        title: "Malformed OpenCode prompt",
        description: "Malformed task packet",
        state: "In Progress"
      }

      assert :ok = AgentRunner.run(issue, self())

      assert_receive {:runner_worker_update, ^issue_id,
                      %{
                        event: :malformed_task_prompt_blocked,
                        phase: :policy_blocked,
                        outcome: :rerouted,
                        result_state: "RCA Required",
                        failure: %{reason: ^reason}
                      }}

      assert_receive {:memory_tracker_comment, ^issue_id, comment}
      assert comment =~ "OpenCode task prompt malformed"
      assert comment =~ inspect(reason)
      assert comment =~ "RCA Required"
      refute comment =~ "this should not run"

      assert_receive {:memory_tracker_state_update, ^issue_id, "RCA Required"}
      refute_received {:memory_tracker_state_update, ^issue_id, "In Review"}
    end

    refute File.exists?(trace_file)
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

    assert :ok = AgentRunner.run(issue, self())

    assert_receive {:runner_worker_update, "issue-1",
                    %{
                      event: :loop_breaker_blocked,
                      phase: :policy_blocked,
                      outcome: :rerouted,
                      result_state: "RCA Required",
                      failure: %{reason: :repair_loop_breaker}
                    }}

    assert_receive {:memory_tracker_comment, "issue-1", comment}
    assert comment =~ "Symphony Stop Rule"
    assert comment =~ "repair loop breaker"
    assert comment =~ "cli-test-boundary"
    refute comment =~ "this should not run"

    assert_receive {:memory_tracker_state_update, "issue-1", "RCA Required"}
    refute_received {:memory_tracker_state_update, "issue-1", "In Review"}
  end

  test "codex runner stops when refreshed issue routes to OpenCode" do
    unique_suffix = Base.url_encode64(:crypto.strong_rand_bytes(8), padding: false)

    workspace_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-codex-to-opencode-handoff-#{System.pid()}-#{unique_suffix}"
      )

    codex_binary = Path.join(workspace_root, "fake-codex")
    trace_file = Path.join(workspace_root, "codex.trace")

    File.rm_rf!(workspace_root)
    File.mkdir_p!(workspace_root)

    on_exit(fn -> File.rm_rf!(workspace_root) end)

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

  test "codex adapter uses configured project root and handles refresh empty or error outcomes" do
    workspace_root = Path.join(System.tmp_dir!(), "symphony-codex-adapter-#{System.unique_integer([:positive])}")
    codex_project_root = Path.join(workspace_root, "project")
    codex_binary = Path.join(workspace_root, "fake-codex")
    trace_file = Path.join(workspace_root, "codex.trace")

    File.mkdir_p!(codex_project_root)
    on_exit(fn -> File.rm_rf(workspace_root) end)

    write_fake_codex!(codex_binary, trace_file)

    write_workflow_file!(Workflow.workflow_file_path(),
      workspace_root: Path.join(workspace_root, "workspaces"),
      codex_command: codex_binary,
      codex_project_root: codex_project_root,
      runner_routes: %{"Todo" => "codex", "In Progress" => "opencode"},
      max_turns: 1,
      prompt: "Architect prompt for {{ issue.identifier }}"
    )

    issue = %Issue{id: "issue-codex", identifier: "NER-C", title: "Codex root", description: "Run", state: "Todo"}

    assert :ok =
             CodexAdapter.run(%{
               workspace: Path.join([workspace_root, "workspaces", "issue-codex"]),
               issue: issue,
               update_recipient: self(),
               worker_host: nil,
               emit_update: fn _ -> :ok end,
               opts: [issue_state_fetcher: fn ["issue-codex"] -> {:ok, []} end]
             })

    assert File.read!(trace_file) =~ codex_project_root

    assert {:error, {:issue_state_refresh_failed, :linear_down}} =
             CodexAdapter.run(%{
               workspace: Path.join([workspace_root, "workspaces", "issue-codex"]),
               issue: issue,
               update_recipient: self(),
               worker_host: nil,
               emit_update: fn _ -> :ok end,
               opts: [issue_state_fetcher: fn ["issue-codex"] -> {:error, :linear_down} end]
             })

    assert :ok =
             CodexAdapter.run(%{
               workspace: Path.join([workspace_root, "workspaces", "issue-codex"]),
               issue: issue,
               update_recipient: self(),
               worker_host: nil,
               emit_update: fn _ -> :ok end,
               opts: [
                 issue_state_fetcher: fn ["issue-codex"] ->
                   {:ok, [%Issue{id: "issue-codex", identifier: "NER-C", title: "No state", state: nil}]}
                 end
               ]
             })
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
        <!-- symphony:opencode-task-prompt:v1 slice_id=route-test -->
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

  test "agent runner schedules OpenCode locally when ssh worker hosts are configured" do
    workspace_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-opencode-local-schedule-#{System.unique_integer([:positive])}"
      )

    command = Path.join(workspace_root, "fake-opencode")
    cwd_file = Path.join(workspace_root, "opencode.cwd")
    File.mkdir_p!(workspace_root)

    File.write!(command, """
    #!/usr/bin/env bash
    pwd > #{cwd_file}
    printf 'opencode completed locally\n'
    """)

    File.chmod!(command, 0o755)

    write_workflow_file!(Workflow.workflow_file_path(),
      tracker_kind: "memory",
      workspace_root: Path.join(workspace_root, "workspaces"),
      worker_ssh_hosts: ["builder-1"],
      runner_routes: %{"In Progress" => "opencode"},
      opencode_command: command,
      opencode_result_state: "In Review",
      prompt: "OpenCode prompt for {{ issue.identifier }}"
    )

    Application.put_env(:symphony_elixir, :memory_tracker_recipient, self())

    Application.put_env(:symphony_elixir, :memory_tracker_opencode_comments, %{
      "issue-local" => [
        """
        <!-- symphony:opencode-task-prompt:v1 slice_id=local-schedule -->
        ```text
        Architect-authored full OpenCode prompt
        ```
        """
      ]
    })

    issue = %Issue{
      id: "issue-local",
      identifier: "NER-LOCAL",
      title: "Run OpenCode locally",
      description: "Implement via OpenCode",
      state: "In Progress"
    }

    assert :ok = AgentRunner.run(issue, self())

    assert_receive {:worker_runtime_info, "issue-local", %{worker_host: nil, workspace_path: workspace_path}}
    assert File.read!(cwd_file) |> String.trim() == workspace_path
    assert_receive {:memory_tracker_state_update, "issue-local", "In Review"}
  end

  test "opencode runner emits sanitized failure event when command runner returns an error" do
    issue = %Issue{
      id: "issue-1",
      identifier: "NER-91",
      title: "Runner returns error",
      description: "Task packet from Codex Architect",
      state: "In Progress"
    }

    test_pid = self()

    assert {:error, {:opencode_runtime_failed, "secret-token-123"}} =
             Runner.run("/tmp/workspace", issue, "prompt body",
               runner: fn _command, _args, _opts ->
                 {:error, {:opencode_runtime_failed, "secret-token-123"}}
               end,
               on_event: fn event -> send(test_pid, {:opencode_event, event}) end
             )

    assert_receive {:opencode_event, %{event: :command_prepared}}
    assert_receive {:opencode_event, event}
    assert %{event: :failed, phase: :failed, failure: %{reason: :opencode_runtime_failed}} = event
    refute inspect(event) =~ "secret-token-123"
  end

  test "opencode runner emits sanitized failure event on runner rescue path" do
    issue = %Issue{
      id: "issue-1",
      identifier: "NER-92",
      title: "Runner crashes",
      description: "Task packet from Codex Architect",
      state: "In Progress"
    }

    test_pid = self()

    assert {:error, {:opencode_failed, message}} =
             Runner.run("/tmp/workspace", issue, "prompt body",
               runner: fn _command, _args, _opts ->
                 raise RuntimeError, "secret-token-123 should stay out of events"
               end,
               on_event: fn event -> send(test_pid, {:opencode_event, event}) end
             )

    assert message =~ "secret-token-123"
    assert_receive {:opencode_event, %{event: :command_prepared}}
    assert_receive {:opencode_event, %{event: :failed, phase: :failed, failure: %{reason: :opencode_failed}} = event}
    refute inspect(event) =~ "secret-token-123"
  end

  test "opencode runner covers timeout failure without relying on other failure branches" do
    issue = %Issue{id: "issue-1", identifier: "NER-93", title: "Failure branches", state: "In Progress"}

    write_workflow_file!(Workflow.workflow_file_path(), opencode_timeout_ms: 1)

    assert {:error, {:opencode_timeout, 1}} =
             Runner.run("/tmp/workspace", issue, "prompt body",
               runner: fn _command, _args, _opts ->
                 Process.sleep(50)
                 {"late", 0}
               end
             )
  end

  test "opencode runner covers exit and non-local worker coercion failures" do
    issue = %Issue{id: "issue-1", identifier: "NER-93", title: "Failure branches", state: "In Progress"}

    write_workflow_file!(Workflow.workflow_file_path(), opencode_timeout_ms: 30_000)

    assert {:error, {:opencode_exit, 7, "failed"}} =
             Runner.run("/tmp/workspace", issue, "prompt body", runner: fn _command, _args, _opts -> {"failed", 7} end)

    assert {:error, {:opencode_remote_worker_host_unsupported, %{worker_host: "remote_atom"}}} =
             Runner.run("/tmp/workspace", issue, "prompt body",
               worker_host: :remote_atom,
               runner: fn _command, _args, _opts -> flunk("remote worker must block first") end
             )
  end

  test "opencode runner tolerates blank attach URL and malformed session reuse data" do
    issue = %Issue{id: "issue-1", identifier: "NER-94", title: "Session variants", state: "In Progress"}
    test_pid = self()

    write_workflow_file!(Workflow.workflow_file_path(), opencode_server_url: "   ")

    assert {:ok, %{session_id: "ses_nested"}} =
             Runner.run("/tmp/workspace", issue, "prompt body",
               session_lister: fn _command, _execution_dir, _title ->
                 {:ok, [%{"title" => "wrong"}, %{"title" => "NER-94 Session variants", "directory" => "/tmp/workspace"}]}
               end,
               runner: fn _command, args, _opts ->
                 send(test_pid, {:args, args})
                 {~s({"result":{"session_id":"ses_nested"}}), 0}
               end
             )

    assert_received {:args, args}
    refute "--attach" in args
  end

  test "opencode runner lists sessions through configured command when attached" do
    issue = %Issue{id: "issue-1", identifier: "NER-95", title: "List sessions", state: "In Progress"}
    test_root = Path.join(System.tmp_dir!(), "symphony-opencode-list-#{System.unique_integer([:positive])}")
    command = Path.join(test_root, "fake-opencode")
    workspace = Path.join(test_root, "workspace")

    File.mkdir_p!(test_root)
    File.mkdir_p!(workspace)

    File.write!(command, """
    #!/usr/bin/env bash
    if [ "$1" = "session" ]; then
      printf '[{"id":"ses_created","title":"NER-95 List sessions","directory":"#{workspace}","created":5}]'
      exit 0
    fi
    printf '{"sessionID":"ses_from_run"}\n'
    """)

    File.chmod!(command, 0o755)
    on_exit(fn -> File.rm_rf(test_root) end)

    write_workflow_file!(Workflow.workflow_file_path(),
      opencode_command: command,
      opencode_server_url: "http://127.0.0.1:3000"
    )

    assert {:ok, %{session_id: "ses_created", command: [^command, "session", "resume-result", "ses_created"]}} =
             Runner.run(workspace, issue, "prompt body", session_result_reader: fn _execution_dir, _session_id -> {:ok, "completed"} end)
  end

  test "opencode runner reports top-level normalization failures and trims non-binary output" do
    issue = %Issue{id: "issue-1", identifier: "NER-96", title: "Normalize failure", state: "In Progress"}

    assert {:error, {:opencode_failed, message}} = Runner.run("/tmp/workspace", issue, %{bad: :packet})
    assert message =~ "function clause"

    assert comment = Runner.handoff_comment(issue, %{output: %{not: :binary}, command: ["opencode", "run"]})
    assert comment =~ "%{not: :binary}"

    long_output = String.duplicate("a", 20_010)
    assert Runner.handoff_comment(issue, %{output: long_output, command: ["opencode", "run"]}) =~ "[truncated]"
  end

  test "opencode runner classifies task exits and error shapes without leaking details into events" do
    issue = %Issue{id: "issue-1", identifier: "NER-97", title: "Classify failures", state: "In Progress"}
    test_pid = self()
    previous_trap_exit = Process.flag(:trap_exit, true)

    on_exit(fn -> Process.flag(:trap_exit, previous_trap_exit) end)

    assert {:error, {:opencode_failed, :boom}} =
             Runner.run("/tmp/workspace", issue, "prompt body",
               runner: fn _command, _args, _opts -> exit(:boom) end,
               on_event: fn event -> send(test_pid, {:opencode_event, event}) end
             )

    assert_receive {:opencode_event, %{event: :failed, failure: %{reason: :opencode_failed}}}

    assert {:error, :plain_atom_failure} =
             Runner.run("/tmp/workspace", issue, "prompt body",
               runner: fn _command, _args, _opts -> {:error, :plain_atom_failure} end,
               on_event: fn event -> send(test_pid, {:opencode_event, event}) end
             )

    assert_receive {:opencode_event, %{event: :failed, failure: %{reason: :plain_atom_failure}}}

    assert {:error, {:classified_timeout, 25}} =
             Runner.run("/tmp/workspace", issue, "prompt body",
               runner: fn _command, _args, _opts -> {:error, {:classified_timeout, 25}} end,
               on_event: fn event -> send(test_pid, {:opencode_event, event}) end
             )

    assert_receive {:opencode_event, %{event: :failed, failure: %{reason: :classified_timeout, timeout_ms: 25}}}

    assert {:error, {:classified_exit, 9, "secret detail"}} =
             Runner.run("/tmp/workspace", issue, "prompt body",
               runner: fn _command, _args, _opts -> {:error, {:classified_exit, 9, "secret detail"}} end,
               on_event: fn event -> send(test_pid, {:opencode_event, event}) end
             )

    assert_receive {:opencode_event, %{event: :failed, failure: %{reason: :classified_exit, status: 9}}}

    assert {:error, {:non_atom, "secret detail"}} =
             Runner.run("/tmp/workspace", issue, "prompt body",
               runner: fn _command, _args, _opts -> {:error, {:non_atom, "secret detail"}} end,
               on_event: fn event -> send(test_pid, {:opencode_event, event}) end
             )

    assert_receive {:opencode_event, %{event: :failed, failure: %{reason: :non_atom}}}

    assert {:error, {:classified_extra, :not_status, "secret detail"}} =
             Runner.run("/tmp/workspace", issue, "prompt body",
               runner: fn _command, _args, _opts -> {:error, {:classified_extra, :not_status, "secret detail"}} end,
               on_event: fn event -> send(test_pid, {:opencode_event, event}) end
             )

    assert_receive {:opencode_event, %{event: :failed, failure: %{reason: :classified_extra}}}

    assert {:error, {"string_reason", "secret detail"}} =
             Runner.run("/tmp/workspace", issue, "prompt body",
               runner: fn _command, _args, _opts -> {:error, {"string_reason", "secret detail"}} end,
               on_event: fn event -> send(test_pid, {:opencode_event, event}) end
             )

    assert_receive {:opencode_event, %{event: :failed, failure: %{reason: :opencode_failed}}}
  end

  test "opencode runner falls back from session-list failures and parses fresh session ids" do
    issue = %Issue{id: "issue-1", identifier: "NER-98", title: "Session list failures", state: "In Progress"}

    for {case_name, session_output, session_status} <- [
          {"invalid-json", "not-json", 0},
          {"unexpected-json", ~s({"id":"not-a-list"}), 0},
          {"list-exit", "session list failed", 12}
        ] do
      test_root = Path.join(System.tmp_dir!(), "symphony-opencode-session-#{case_name}-#{System.unique_integer([:positive])}")
      command = Path.join(test_root, "fake-opencode")
      File.mkdir_p!(test_root)

      File.write!(command, """
      #!/usr/bin/env bash
      if [ "$1" = "session" ]; then
        printf '%s' #{inspect(session_output)}
        exit #{session_status}
      fi

      cat >/dev/null
      printf '%s\n' '{"sessionID":"ses_#{case_name}"}'
      """)

      File.chmod!(command, 0o755)
      on_exit(fn -> File.rm_rf(test_root) end)

      write_workflow_file!(Workflow.workflow_file_path(),
        opencode_command: command,
        opencode_server_url: "http://127.0.0.1:3000"
      )

      expected_session_id = "ses_#{case_name}"
      assert {:ok, %{session_id: ^expected_session_id}} = Runner.run(test_root, issue, "prompt body")
    end
  end

  test "opencode runner covers session rediscovery failures and non-map session payloads" do
    issue = %Issue{id: "issue-1", identifier: "NER-100", title: "Session rediscovery", state: "In Progress"}

    write_workflow_file!(Workflow.workflow_file_path(), opencode_server_url: "http://127.0.0.1:3000")

    assert {:ok, %{session_id: nil}} =
             Runner.run("/tmp/workspace", issue, "prompt body",
               session_lister: fn _command, _execution_dir, _title -> {:error, :rediscovery_down} end,
               runner: fn _command, _args, _opts -> {"completed without session id", 0} end
             )

    assert {:ok, %{session_id: nil}} =
             Runner.run("/tmp/workspace", issue, "prompt body", runner: fn _command, _args, _opts -> {~s({"result":"not-a-map"}) <> "\n" <> ~s({}), 0} end)

    assert {:ok, %{session_id: nil}} =
             Runner.run("/tmp/workspace", issue, "prompt body", runner: fn _command, _args, _opts -> {123, 0} end)
  end

  test "opencode runner falls back when attached session list command cannot start" do
    issue = %Issue{id: "issue-1", identifier: "NER-100B", title: "Missing command", state: "In Progress"}

    missing_command = Path.join(System.tmp_dir!(), "missing-opencode-#{System.unique_integer([:positive])}")
    workspace = Path.join(System.tmp_dir!(), "symphony-missing-opencode-workspace-#{System.unique_integer([:positive])}")
    File.mkdir_p!(workspace)
    on_exit(fn -> File.rm_rf(workspace) end)

    write_workflow_file!(Workflow.workflow_file_path(),
      opencode_command: missing_command,
      opencode_server_url: "http://127.0.0.1:3000"
    )

    assert {:error, {:opencode_exit, 127, output}} = Runner.run(workspace, issue, "prompt body")
    assert output =~ "missing-opencode"
  end

  test "opencode runner surfaces local session sqlite and handoff parsing errors" do
    issue = %Issue{id: "issue-1", identifier: "NER-101", title: "Session sqlite errors", state: "In Progress"}
    data_home = Path.join(System.tmp_dir!(), "opencode-runner-errors-#{System.unique_integer([:positive, :monotonic])}")
    db_dir = Path.join([data_home, "opencode"])
    db_path = Path.join(db_dir, "opencode.db")
    execution_dir = Path.join(data_home, "workspace")
    File.mkdir_p!(db_dir)
    File.mkdir_p!(execution_dir)

    previous_xdg_data_home = System.get_env("XDG_DATA_HOME")
    System.put_env("XDG_DATA_HOME", data_home)

    on_exit(fn ->
      if previous_xdg_data_home, do: System.put_env("XDG_DATA_HOME", previous_xdg_data_home), else: System.delete_env("XDG_DATA_HOME")
      File.rm_rf(data_home)
    end)

    write_workflow_file!(Workflow.workflow_file_path(), opencode_server_url: "http://127.0.0.1:3000")

    assert {:error, {:opencode_db_not_found, ^db_path}} =
             Runner.run(execution_dir, issue, "prompt body",
               session_lister: fn _command, _execution_dir, _title ->
                 {:ok, [%{"id" => "ses_missing_db", "title" => "NER-101 Session sqlite errors", "directory" => execution_dir, "updated" => 20}]}
               end
             )

    sql = """
    create table session (id text primary key,title text,directory text,time_created integer,time_updated integer,summary_files integer,summary_additions integer,summary_deletions integer,tokens_input integer,tokens_output integer);
    create table message (id text primary key,session_id text,time_created integer,data text);
    create table part (id text primary key,message_id text,time_created integer,data text);
    insert into session (id,title,directory,time_created,time_updated,summary_files,summary_additions,summary_deletions,tokens_input,tokens_output)
      values ('ses_wrong_dir','NER-101 Session sqlite errors','/tmp/other',1,2,0,0,0,0,0);
    """

    assert {"", 0} = System.cmd("sqlite3", [db_path, sql], stderr_to_stdout: true)

    assert {:error, {:opencode_session_not_found, "ses_absent"}} =
             Runner.run(execution_dir, issue, "prompt body",
               session_lister: fn _command, _execution_dir, _title ->
                 {:ok, [%{"id" => "ses_absent", "title" => "NER-101 Session sqlite errors", "directory" => execution_dir, "updated" => 20}]}
               end
             )

    assert {:error, {:opencode_session_directory_mismatch, "/tmp/other", ^execution_dir}} =
             Runner.run(execution_dir, issue, "prompt body",
               session_lister: fn _command, _execution_dir, _title ->
                 {:ok, [%{"id" => "ses_wrong_dir", "title" => "NER-101 Session sqlite errors", "directory" => execution_dir, "updated" => 20}]}
               end
             )

    File.rm!(db_path)

    sql_without_message = """
    create table session (id text primary key,title text,directory text,time_created integer,time_updated integer,summary_files integer,summary_additions integer,summary_deletions integer,tokens_input integer,tokens_output integer);
    insert into session (id,title,directory,time_created,time_updated,summary_files,summary_additions,summary_deletions,tokens_input,tokens_output)
      values ('ses_no_message_table','NER-101 Session sqlite errors','#{execution_dir}',1,2,0,0,0,0,0);
    """

    assert {"", 0} = System.cmd("sqlite3", [db_path, sql_without_message], stderr_to_stdout: true)

    assert {:error, {:opencode_sqlite_failed, _status, _output}} =
             Runner.run(execution_dir, issue, "prompt body",
               session_lister: fn _command, _execution_dir, _title ->
                 {:ok, [%{"id" => "ses_no_message_table", "title" => "NER-101 Session sqlite errors", "directory" => execution_dir, "updated" => 20}]}
               end
             )
  end

  test "opencode runner covers session reader rescue, default data home, sqlite failures, and incomplete sections" do
    issue = %Issue{id: "issue-1", identifier: "NER-102", title: "Session defensive paths", state: "In Progress"}
    data_home = Path.join(System.tmp_dir!(), "opencode-runner-defensive-#{System.unique_integer([:positive, :monotonic])}")
    db_dir = Path.join([data_home, "opencode"])
    db_path = Path.join(db_dir, "opencode.db")
    execution_dir = Path.join(data_home, "workspace")
    File.mkdir_p!(db_dir)
    File.mkdir_p!(execution_dir)

    previous_xdg_data_home = System.get_env("XDG_DATA_HOME")
    System.put_env("XDG_DATA_HOME", data_home)

    on_exit(fn ->
      if previous_xdg_data_home, do: System.put_env("XDG_DATA_HOME", previous_xdg_data_home), else: System.delete_env("XDG_DATA_HOME")
      File.rm_rf(data_home)
    end)

    write_workflow_file!(Workflow.workflow_file_path(), opencode_server_url: "http://127.0.0.1:3000")

    assert {:error, {:opencode_failed, "reader exploded"}} =
             Runner.run(execution_dir, issue, "prompt body",
               session_lister: fn _command, _execution_dir, _title ->
                 {:ok, [%{"id" => "ses_raise", "title" => "NER-102 Session defensive paths", "directory" => execution_dir, "updated" => 20}]}
               end,
               session_result_reader: fn _execution_dir, _session_id -> raise RuntimeError, "reader exploded" end
             )

    System.delete_env("XDG_DATA_HOME")

    assert {:error, default_home_error} =
             Runner.run(execution_dir, issue, "prompt body",
               session_lister: fn _command, _execution_dir, _title ->
                 {:ok, [%{"id" => "ses_default_home", "title" => "NER-102 Session defensive paths", "directory" => execution_dir, "updated" => 20}]}
               end
             )

    assert default_home_error in [
             {:opencode_db_not_found, Path.join([System.user_home!(), ".local/share", "opencode", "opencode.db"])},
             {:opencode_session_not_found, "ses_default_home"}
           ]

    System.put_env("XDG_DATA_HOME", data_home)

    File.mkdir_p!(db_path)

    assert {:error, {:opencode_sqlite_failed, _status, _output}} =
             Runner.run(execution_dir, issue, "prompt body",
               session_lister: fn _command, _execution_dir, _title ->
                 {:ok, [%{"id" => "ses_sqlite_dir", "title" => "NER-102 Session defensive paths", "directory" => execution_dir, "updated" => 20}]}
               end
             )

    File.rm_rf!(db_path)

    blocked_text = "### In Progress\n- remaining work\n"
    none_text = "### In Progress\n- `(none)`\n"
    message_data = Jason.encode!(%{"role" => "assistant", "finish" => "stop"})
    blocked_part = Jason.encode!(%{"type" => "text", "text" => blocked_text})
    none_part = Jason.encode!(%{"type" => "text", "text" => none_text})
    missing_text_part = Jason.encode!(%{"type" => "text"})

    sql = """
    create table session (id text primary key,title text,directory text,time_created integer,time_updated integer,summary_files integer,summary_additions integer,summary_deletions integer,tokens_input integer,tokens_output integer);
    create table message (id text primary key,session_id text,time_created integer,data text);
    create table part (id text primary key,message_id text,time_created integer,data text);
    insert into session (id,title,directory,time_created,time_updated,summary_files,summary_additions,summary_deletions,tokens_input,tokens_output)
      values ('ses_blocked','NER-102 Session defensive paths','#{execution_dir}',1,2,0,0,0,0,0),
             ('ses_none','NER-102 Session defensive paths','#{execution_dir}',1,2,0,0,0,0,0),
             ('ses_empty_part','NER-102 Session defensive paths','#{execution_dir}',1,2,0,0,0,0,0);
    insert into message (id,session_id,time_created,data)
      values ('msg_blocked','ses_blocked',2,#{sql_quote(message_data)}),
             ('msg_none','ses_none',2,#{sql_quote(message_data)}),
             ('msg_empty_part','ses_empty_part',2,#{sql_quote(message_data)});
    insert into part (id,message_id,time_created,data)
      values ('part_blocked','msg_blocked',3,#{sql_quote(blocked_part)}),
             ('part_none','msg_none',3,#{sql_quote(none_part)}),
             ('part_missing_text','msg_empty_part',3,#{sql_quote(missing_text_part)});
    """

    assert {"", 0} = System.cmd("sqlite3", [db_path, sql], stderr_to_stdout: true)

    assert {:ok, %{output: "continued\n"}} =
             Runner.run(execution_dir, issue, "prompt body",
               session_lister: fn _command, _execution_dir, _title ->
                 {:ok, [%{"id" => "ses_blocked", "title" => "NER-102 Session defensive paths", "directory" => execution_dir, "updated" => 20}]}
               end,
               runner: fn _command, _args, _opts -> {"continued\n", 0} end
             )

    assert {:ok, %{output: output}} =
             Runner.run(execution_dir, issue, "prompt body",
               session_lister: fn _command, _execution_dir, _title ->
                 {:ok, [%{"id" => "ses_none", "title" => "NER-102 Session defensive paths", "directory" => execution_dir, "updated" => 20}]}
               end
             )

    assert output =~ "Latest assistant handoff"

    assert {:ok, %{output: "continued empty\n"}} =
             Runner.run(execution_dir, issue, "prompt body",
               session_lister: fn _command, _execution_dir, _title ->
                 {:ok, [%{"id" => "ses_empty_part", "title" => "NER-102 Session defensive paths", "directory" => execution_dir, "updated" => 20}]}
               end,
               runner: fn _command, _args, _opts -> {"continued empty\n", 0} end
             )
  end

  test "opencode runner reads completed local session handoff from sqlite" do
    issue = %Issue{id: "issue-1", identifier: "NER-99", title: "Completed session", state: "In Progress"}
    data_home = Path.join(System.tmp_dir!(), "opencode-runner-completed-#{System.unique_integer([:positive, :monotonic])}")
    db_dir = Path.join([data_home, "opencode"])
    db_path = Path.join(db_dir, "opencode.db")
    execution_dir = Path.join(data_home, "workspace")
    File.mkdir_p!(db_dir)
    File.mkdir_p!(execution_dir)

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

    message_data = Jason.encode!(%{"role" => "assistant", "finish" => "stop"})
    part_data = Jason.encode!(%{"type" => "text", "text" => "Final handoff body"})

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

    create table message (id text primary key, session_id text, time_created integer, data text);
    create table part (id text primary key, message_id text, time_created integer, data text);

    insert into session
      (id,title,directory,time_created,time_updated,summary_files,summary_additions,summary_deletions,tokens_input,tokens_output)
      values ('ses_done','NER-99 Completed session','#{execution_dir}',1,2,3,4,5,6,7);
    insert into message (id,session_id,time_created,data) values ('msg_done','ses_done',2,#{sql_quote(message_data)});
    insert into part (id,message_id,time_created,data) values ('part_done','msg_done',3,#{sql_quote(part_data)});
    """

    assert {"", 0} = System.cmd("sqlite3", [db_path, sql], stderr_to_stdout: true)

    write_workflow_file!(Workflow.workflow_file_path(), opencode_server_url: "http://127.0.0.1:3000")

    assert {:ok, %{output: output, command: ["opencode", "session", "resume-result", "ses_done"]}} =
             Runner.run(execution_dir, issue, "prompt body",
               session_lister: fn _command, _execution_dir, _title ->
                 {:ok, [%{"id" => "ses_done", "title" => "NER-99 Completed session", "directory" => execution_dir, "updated" => 20}]}
               end
             )

    assert output =~ "Resumed completed OpenCode session"
    assert output =~ "Summary: 3 files, +4 -5"
    assert output =~ "Tokens: input 6, output 7"
    assert output =~ "Final handoff body"
  end

  defp opencode_task_prompt_comment(slice_id, prompt) do
    """
    <!-- symphony:opencode-task-prompt:v1 slice_id=#{slice_id} -->
    ```text
    #{prompt}
    ```
    """
  end

  defp write_fake_codex!(codex_binary, trace_file) do
    File.write!(codex_binary, """
    #!/usr/bin/env bash
    trace_file=#{inspect(trace_file)}
    count=0

    while IFS= read -r line; do
      count=$((count + 1))
      echo "$line" >> "$trace_file"

      case "$count" in
        1) printf '%s\n' '{"id":1,"result":{}}' ;;
        2) printf '%s\n' '{"id":2,"result":{"thread":{"id":"codex-thread"}}}' ;;
        3) printf '%s\n' '{"id":3,"result":{"turn":{"id":"codex-turn"}}}' ;;
        4) printf '%s\n' '{"method":"turn/completed"}'; exit 0 ;;
        *) printf '%s\n' '{"id":999,"error":{"message":"unexpected"}}'; exit 2 ;;
      esac
    done
    """)

    File.chmod!(codex_binary, 0o755)
  end

  defp task_packet(slice_id, prompt) do
    %TaskPrompt.Packet{
      prompt: prompt,
      slice_id: slice_id,
      fingerprint: :crypto.hash(:sha256, prompt) |> Base.encode16(case: :lower)
    }
  end

  defp sql_quote(value), do: "'" <> String.replace(value, "'", "''") <> "'"
end
