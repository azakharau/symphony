defmodule SymphonyElixir.ExtensionsTest do
  use SymphonyElixir.TestSupport

  import Phoenix.ConnTest
  import Phoenix.LiveViewTest

  alias SymphonyElixir.Linear.Adapter
  alias SymphonyElixir.Tracker.Memory

  @endpoint SymphonyElixirWeb.Endpoint

  defmodule FakeLinearClient do
    def fetch_candidate_issues do
      send(self(), :fetch_candidate_issues_called)
      {:ok, [:candidate]}
    end

    def fetch_issues_by_states(states) do
      send(self(), {:fetch_issues_by_states_called, states})
      {:ok, states}
    end

    def fetch_issue_states_by_ids(issue_ids) do
      send(self(), {:fetch_issue_states_by_ids_called, issue_ids})
      {:ok, issue_ids}
    end

    def fetch_project_milestones do
      send(self(), :fetch_project_milestones_called)
      {:ok, [%{"name" => "SYM-5"}]}
    end

    def graphql(query, variables) do
      send(self(), {:graphql_called, query, variables})

      case Process.get({__MODULE__, :graphql_results}) do
        [result | rest] ->
          Process.put({__MODULE__, :graphql_results}, rest)
          result

        _ ->
          Process.get({__MODULE__, :graphql_result})
      end
    end
  end

  defmodule SlowOrchestrator do
    use GenServer

    def start_link(opts) do
      GenServer.start_link(__MODULE__, :ok, opts)
    end

    def init(:ok), do: {:ok, :ok}

    def handle_call(:snapshot, _from, state) do
      Process.sleep(25)
      {:reply, %{}, state}
    end

    def handle_call(:request_refresh, _from, state) do
      {:reply, :unavailable, state}
    end
  end

  defmodule StaticOrchestrator do
    use GenServer

    def start_link(opts) do
      name = Keyword.fetch!(opts, :name)
      GenServer.start_link(__MODULE__, opts, name: name)
    end

    def init(opts), do: {:ok, opts}

    def handle_call(:snapshot, _from, state) do
      {:reply, Keyword.fetch!(state, :snapshot), state}
    end

    def handle_call(:request_refresh, _from, state) do
      {:reply, Keyword.get(state, :refresh, :unavailable), state}
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

  setup do
    endpoint_config = Application.get_env(:symphony_elixir, SymphonyElixirWeb.Endpoint, [])

    on_exit(fn ->
      Application.put_env(:symphony_elixir, SymphonyElixirWeb.Endpoint, endpoint_config)
    end)

    :ok
  end

  test "workflow store reloads changes, keeps last good workflow, and falls back when stopped" do
    ensure_workflow_store_running()
    assert {:ok, %{prompt: "Repository execution request."}} = Workflow.current()

    write_workflow_file!(Workflow.workflow_file_path(), prompt: "Second prompt")
    send(WorkflowStore, :poll)

    assert_eventually(fn ->
      match?({:ok, %{prompt: "Second prompt"}}, Workflow.current())
    end)

    File.write!(Workflow.workflow_file_path(), "---\ntracker: [\n---\nBroken prompt\n")
    assert {:error, _reason} = WorkflowStore.force_reload()
    assert {:ok, %{prompt: "Second prompt"}} = Workflow.current()

    third_workflow = Path.join(Path.dirname(Workflow.workflow_file_path()), "THIRD_WORKFLOW.md")
    write_workflow_file!(third_workflow, prompt: "Third prompt")
    Workflow.set_workflow_file_path(third_workflow)
    assert {:ok, %{prompt: "Third prompt"}} = Workflow.current()

    assert :ok = Supervisor.terminate_child(SymphonyElixir.Supervisor, WorkflowStore)
    assert {:ok, %{prompt: "Third prompt"}} = WorkflowStore.current()
    assert :ok = WorkflowStore.force_reload()
    assert {:ok, _pid} = Supervisor.restart_child(SymphonyElixir.Supervisor, WorkflowStore)
  end

  test "workflow store init stops on missing workflow file" do
    missing_path = Path.join(Path.dirname(Workflow.workflow_file_path()), "MISSING_WORKFLOW.md")
    Workflow.set_workflow_file_path(missing_path)

    assert {:stop, {:missing_workflow_file, ^missing_path, :enoent}} = WorkflowStore.init([])
  end

  test "workflow store start_link and poll callback cover missing-file error paths" do
    ensure_workflow_store_running()
    existing_path = Workflow.workflow_file_path()
    manual_path = Path.join(Path.dirname(existing_path), "MANUAL_WORKFLOW.md")
    missing_path = Path.join(Path.dirname(existing_path), "MANUAL_MISSING_WORKFLOW.md")

    assert :ok = Supervisor.terminate_child(SymphonyElixir.Supervisor, WorkflowStore)

    Workflow.set_workflow_file_path(missing_path)

    assert {:error, {:missing_workflow_file, ^missing_path, :enoent}} =
             WorkflowStore.force_reload()

    write_workflow_file!(manual_path, prompt: "Manual workflow prompt")
    Workflow.set_workflow_file_path(manual_path)

    assert {:ok, manual_pid} = WorkflowStore.start_link()
    assert Process.alive?(manual_pid)

    state = :sys.get_state(manual_pid)
    File.write!(manual_path, "---\ntracker: [\n---\nBroken prompt\n")
    assert {:noreply, returned_state} = WorkflowStore.handle_info(:poll, state)
    assert returned_state.workflow.prompt == "Manual workflow prompt"
    refute returned_state.stamp == nil
    assert_receive :poll, 2_000

    Workflow.set_workflow_file_path(missing_path)
    assert {:noreply, path_error_state} = WorkflowStore.handle_info(:poll, returned_state)
    assert path_error_state.workflow.prompt == "Manual workflow prompt"
    assert_receive :poll, 2_000

    Workflow.set_workflow_file_path(manual_path)
    File.rm!(manual_path)
    assert {:noreply, removed_state} = WorkflowStore.handle_info(:poll, path_error_state)
    assert removed_state.workflow.prompt == "Manual workflow prompt"
    assert_receive :poll, 2_000

    Process.exit(manual_pid, :normal)
    restart_result = Supervisor.restart_child(SymphonyElixir.Supervisor, WorkflowStore)

    assert match?({:ok, _pid}, restart_result) or
             match?({:error, {:already_started, _pid}}, restart_result)

    Workflow.set_workflow_file_path(existing_path)
    WorkflowStore.force_reload()
  end

  test "tracker delegates to memory and linear adapters" do
    issue = %Issue{id: "issue-1", identifier: "MT-1", state: "In Progress"}
    Application.put_env(:symphony_elixir, :memory_tracker_issues, [issue, %{id: "ignored"}])
    Application.put_env(:symphony_elixir, :memory_tracker_recipient, self())
    write_workflow_file!(Workflow.workflow_file_path(), tracker_kind: "memory")

    assert Config.settings!().tracker.kind == "memory"
    assert SymphonyElixir.Tracker.adapter() == Memory
    assert {:ok, [^issue]} = SymphonyElixir.Tracker.fetch_candidate_issues()
    assert {:ok, [^issue]} = SymphonyElixir.Tracker.fetch_issues_by_states([" in progress ", 42])
    assert {:ok, [^issue]} = SymphonyElixir.Tracker.fetch_issue_states_by_ids(["issue-1"])
    assert :ok = SymphonyElixir.Tracker.create_comment("issue-1", "comment")
    assert :ok = SymphonyElixir.Tracker.update_issue_state("issue-1", "Done")
    assert_receive {:memory_tracker_comment, "issue-1", "comment"}
    assert_receive {:memory_tracker_state_update, "issue-1", "Done"}

    Application.delete_env(:symphony_elixir, :memory_tracker_recipient)
    assert :ok = Memory.create_comment("issue-1", "quiet")
    assert :ok = Memory.update_issue_state("issue-1", "Quiet")

    write_workflow_file!(Workflow.workflow_file_path(), tracker_kind: "linear")
    assert SymphonyElixir.Tracker.adapter() == Adapter
  end

  test "linear adapter delegates reads and validates mutation responses" do
    Application.put_env(:symphony_elixir, :linear_client_module, FakeLinearClient)

    assert {:ok, [:candidate]} = Adapter.fetch_candidate_issues()
    assert_receive :fetch_candidate_issues_called

    assert {:ok, ["Todo"]} = Adapter.fetch_issues_by_states(["Todo"])
    assert_receive {:fetch_issues_by_states_called, ["Todo"]}

    assert {:ok, ["issue-1"]} = Adapter.fetch_issue_states_by_ids(["issue-1"])
    assert_receive {:fetch_issue_states_by_ids_called, ["issue-1"]}

    Process.put(
      {FakeLinearClient, :graphql_result},
      {:ok, %{"data" => %{"commentCreate" => %{"success" => true}}}}
    )

    assert :ok = Adapter.create_comment("issue-1", "hello")
    assert_receive {:graphql_called, create_comment_query, %{body: "hello", issueId: "issue-1"}}
    assert create_comment_query =~ "commentCreate"

    Process.put(
      {FakeLinearClient, :graphql_result},
      {:ok, %{"data" => %{"commentCreate" => %{"success" => false}}}}
    )

    assert {:error, :comment_create_failed} =
             Adapter.create_comment("issue-1", "broken")

    Process.put({FakeLinearClient, :graphql_result}, {:error, :boom})

    assert {:error, :boom} = Adapter.create_comment("issue-1", "boom")

    Process.put({FakeLinearClient, :graphql_result}, {:ok, %{"data" => %{}}})
    assert {:error, :comment_create_failed} = Adapter.create_comment("issue-1", "weird")

    Process.put({FakeLinearClient, :graphql_result}, :unexpected)
    assert {:error, :comment_create_failed} = Adapter.create_comment("issue-1", "odd")

    Process.put(
      {FakeLinearClient, :graphql_results},
      [
        {:ok,
         %{
           "data" => %{
             "issue" => %{"team" => %{"states" => %{"nodes" => [%{"id" => "state-1"}]}}}
           }
         }},
        {:ok, %{"data" => %{"issueUpdate" => %{"success" => true}}}}
      ]
    )

    assert :ok = Adapter.update_issue_state("issue-1", "Done")
    assert_receive {:graphql_called, state_lookup_query, %{issueId: "issue-1", stateName: "Done"}}
    assert state_lookup_query =~ "states"

    assert_receive {:graphql_called, update_issue_query, %{issueId: "issue-1", stateId: "state-1"}}

    assert update_issue_query =~ "issueUpdate"

    Process.put(
      {FakeLinearClient, :graphql_results},
      [
        {:ok,
         %{
           "data" => %{
             "issue" => %{"team" => %{"states" => %{"nodes" => [%{"id" => "state-1"}]}}}
           }
         }},
        {:ok, %{"data" => %{"issueUpdate" => %{"success" => false}}}}
      ]
    )

    assert {:error, :issue_update_failed} =
             Adapter.update_issue_state("issue-1", "Broken")

    Process.put({FakeLinearClient, :graphql_results}, [{:error, :boom}])

    assert {:error, :boom} = Adapter.update_issue_state("issue-1", "Boom")

    Process.put({FakeLinearClient, :graphql_results}, [{:ok, %{"data" => %{}}}])
    assert {:error, :state_not_found} = Adapter.update_issue_state("issue-1", "Missing")

    Process.put(
      {FakeLinearClient, :graphql_results},
      [
        {:ok,
         %{
           "data" => %{
             "issue" => %{"team" => %{"states" => %{"nodes" => [%{"id" => "state-1"}]}}}
           }
         }},
        {:ok, %{"data" => %{}}}
      ]
    )

    assert {:error, :issue_update_failed} = Adapter.update_issue_state("issue-1", "Weird")

    Process.put(
      {FakeLinearClient, :graphql_results},
      [
        {:ok,
         %{
           "data" => %{
             "issue" => %{"team" => %{"states" => %{"nodes" => [%{"id" => "state-1"}]}}}
           }
         }},
        :unexpected
      ]
    )

    assert {:error, :issue_update_failed} = Adapter.update_issue_state("issue-1", "Odd")
  end

  test "linear adapter selects task prompt from newest comment page" do
    Application.put_env(:symphony_elixir, :linear_client_module, FakeLinearClient)

    Process.put(
      {FakeLinearClient, :graphql_result},
      {:ok,
       %{
         "data" => %{
           "issue" => %{
             "comments" => %{
               "nodes" => [
                 %{
                   "body" => opencode_task_prompt_comment("old-slice", "old prompt"),
                   "createdAt" => "2026-01-01T00:00:00Z"
                 },
                 %{"body" => "plain newest chatter", "createdAt" => "2026-01-03T00:00:00Z"},
                 %{
                   "body" => opencode_task_prompt_comment("new-slice", "new prompt"),
                   "createdAt" => "2026-01-02T00:00:00Z"
                 }
               ],
               "pageInfo" => %{"hasNextPage" => false, "endCursor" => "cursor-1"}
             }
           }
         }
       }}
    )

    assert {:ok, packet} = Adapter.latest_opencode_task_packet("issue-1")
    assert packet.prompt == "new prompt"
    assert packet.slice_id == "new-slice"

    assert_receive {:graphql_called, query, %{issueId: "issue-1", first: 50, after: nil}}
    assert query =~ "comments(first: $first, after: $after, orderBy: createdAt)"
    assert query =~ "pageInfo"
    refute query =~ "comments(last:"
  end

  test "linear adapter paginates task prompts so older marked packets are not missed" do
    Application.put_env(:symphony_elixir, :linear_client_module, FakeLinearClient)

    Process.put(
      {FakeLinearClient, :graphql_results},
      [
        {:ok,
         %{
           "data" => %{
             "issue" => %{
               "comments" => %{
                 "nodes" => [%{"body" => "newest chatter", "createdAt" => "2026-01-03T00:00:00Z"}],
                 "pageInfo" => %{"hasNextPage" => true, "endCursor" => "cursor-page-1"}
               }
             }
           }
         }},
        {:ok,
         %{
           "data" => %{
             "issue" => %{
               "comments" => %{
                 "nodes" => [
                   %{
                     "body" => opencode_task_prompt_comment("older-slice", "older prompt"),
                     "createdAt" => "2026-01-01T00:00:00Z"
                   }
                 ],
                 "pageInfo" => %{"hasNextPage" => false, "endCursor" => nil}
               }
             }
           }
         }}
      ]
    )

    assert {:ok, packet} = Adapter.latest_opencode_task_packet("issue-1")
    assert packet.slice_id == "older-slice"
    assert packet.prompt == "older prompt"

    assert_receive {:graphql_called, query, %{issueId: "issue-1", first: 50, after: nil}}
    assert query =~ "pageInfo"
    assert_receive {:graphql_called, _query, %{issueId: "issue-1", first: 50, after: "cursor-page-1"}}
  end

  test "linear adapter reports task-prompt pagination cursor and page-limit errors" do
    Application.put_env(:symphony_elixir, :linear_client_module, FakeLinearClient)

    Process.put(
      {FakeLinearClient, :graphql_result},
      {:ok,
       %{
         "data" => %{
           "issue" => %{
             "comments" => %{
               "nodes" => [%{"body" => "newest chatter", "createdAt" => "2026-01-03T00:00:00Z"}],
               "pageInfo" => %{"hasNextPage" => true, "endCursor" => nil}
             }
           }
         }
       }}
    )

    assert {:error, :opencode_task_prompt_missing_end_cursor} = Adapter.latest_opencode_task_packet("issue-1")

    Process.put(
      {FakeLinearClient, :graphql_results},
      List.duplicate(
        {:ok,
         %{
           "data" => %{
             "issue" => %{
               "comments" => %{
                 "nodes" => [%{"body" => "older chatter", "createdAt" => "2026-01-01T00:00:00Z"}],
                 "pageInfo" => %{"hasNextPage" => true, "endCursor" => "same-cursor"}
               }
             }
           }
         }},
        20
      )
    )

    assert {:error, :opencode_task_prompt_comment_page_limit_exceeded} = Adapter.latest_opencode_task_packet("issue-1")
  end

  test "linear adapter delegates project milestones and prompt text extraction" do
    Application.put_env(:symphony_elixir, :linear_client_module, FakeLinearClient)

    Process.put(
      {FakeLinearClient, :graphql_result},
      {:ok,
       %{
         "data" => %{
           "issue" => %{
             "comments" => %{
               "nodes" => [
                 %{
                   "body" => opencode_task_prompt_comment("prompt-slice", "prompt text"),
                   "createdAt" => "2026-01-01T00:00:00Z"
                 }
               ],
               "pageInfo" => %{"hasNextPage" => false, "endCursor" => nil}
             }
           }
         }
       }}
    )

    assert {:ok, [%{"name" => "SYM-5"}]} = Adapter.fetch_project_milestones()
    assert {:ok, "prompt text"} = Adapter.latest_opencode_task_prompt("issue-1")
    assert_receive :fetch_project_milestones_called
  end

  test "memory tracker finds OpenCode task prompts in comments and issue descriptions" do
    Application.put_env(:symphony_elixir, :memory_tracker_opencode_comments, %{
      "comment-issue" => [nil, opencode_task_prompt_comment("comment-slice", "comment prompt")],
      "plain-issue" => ["plain comment"]
    })

    Application.put_env(:symphony_elixir, :memory_tracker_issues, [
      %Issue{
        id: "description-issue",
        identifier: "SYM-5",
        title: "Prompt in description",
        description: opencode_task_prompt_comment("description-slice", "description prompt"),
        state: "In Progress"
      }
    ])

    assert {:ok, "comment prompt"} = Memory.latest_opencode_task_prompt("comment-issue")
    assert {:ok, packet} = Memory.latest_opencode_task_packet("description-issue")
    assert packet.slice_id == "description-slice"
    assert {:error, :opencode_task_prompt_not_found} = Memory.latest_opencode_task_packet("plain-issue")
  end

  test "memory tracker ignores non-binary prompt comments" do
    Application.put_env(:symphony_elixir, :memory_tracker_opencode_comments, %{
      "mixed-issue" => [opencode_task_prompt_comment("mixed-slice", "mixed prompt"), %{bad: :shape}]
    })

    assert {:ok, packet} = Memory.latest_opencode_task_packet("mixed-issue")
    assert packet.slice_id == "mixed-slice"
  end

  test "tracker facade delegates latest OpenCode prompt to memory adapter" do
    write_workflow_file!(Workflow.workflow_file_path(), tracker_kind: "memory")

    Application.put_env(:symphony_elixir, :memory_tracker_opencode_comments, %{
      "issue-1" => [opencode_task_prompt_comment("facade-slice", "facade prompt")]
    })

    assert {:ok, "facade prompt"} = Tracker.latest_opencode_task_prompt("issue-1")
  end

  test "linear adapter surfaces task packet and review decision lookup errors" do
    Application.put_env(:symphony_elixir, :linear_client_module, FakeLinearClient)

    Process.put({FakeLinearClient, :graphql_result}, {:error, :rate_limited})
    assert {:error, :rate_limited} = Adapter.latest_opencode_task_packet("issue-1")

    Process.put({FakeLinearClient, :graphql_result}, {:ok, %{"data" => %{"issue" => nil}}})
    assert {:error, :opencode_task_prompt_not_found} = Adapter.latest_opencode_task_packet("issue-1")

    Process.put(
      {FakeLinearClient, :graphql_result},
      {:ok,
       %{
         "data" => %{
           "issue" => %{
             "comments" => %{
               "nodes" => [%{"body" => "<!-- symphony:opencode-task-prompt:v1 slice_id=bad -->"}],
               "pageInfo" => %{"hasNextPage" => false}
             }
           }
         }
       }}
    )

    assert {:error, :opencode_task_prompt_malformed_fence} = Adapter.latest_opencode_task_packet("issue-1")

    Process.put({FakeLinearClient, :graphql_result}, {:error, :review_api_down})
    assert {:error, :review_api_down} = Adapter.review_decisions("issue-1")

    Process.put({FakeLinearClient, :graphql_result}, {:ok, %{"data" => %{"issue" => nil}}})
    assert {:error, :review_decisions_not_found} = Adapter.review_decisions("issue-1")
  end

  test "linear adapter ignores unmarked and malformed comment shapes while finding task packets" do
    Application.put_env(:symphony_elixir, :linear_client_module, FakeLinearClient)

    Process.put(
      {FakeLinearClient, :graphql_result},
      {:ok,
       %{
         "data" => %{
           "issue" => %{
             "comments" => %{
               "nodes" => [
                 %{"createdAt" => "2026-01-04T00:00:00Z"},
                 %{"body" => "plain comment", "createdAt" => "2026-01-03T00:00:00Z"},
                 %{"body" => opencode_task_prompt_comment("shape-slice", "shape prompt"), "createdAt" => "2026-01-02T00:00:00Z"}
               ],
               "pageInfo" => %{"hasNextPage" => false, "endCursor" => nil}
             }
           }
         }
       }}
    )

    assert {:ok, packet} = Adapter.latest_opencode_task_packet("issue-1")
    assert packet.slice_id == "shape-slice"

    Process.put(
      {FakeLinearClient, :graphql_result},
      {:ok,
       %{
         "data" => %{
           "issue" => %{
             "comments" => %{
               "nodes" => [%{"createdAt" => "2026-01-04T00:00:00Z"}, %{"body" => "plain comment", "createdAt" => "2026-01-03T00:00:00Z"}],
               "pageInfo" => %{"hasNextPage" => false, "endCursor" => nil}
             }
           }
         }
       }}
    )

    assert {:error, :opencode_task_prompt_not_found} = Adapter.latest_opencode_task_packet("issue-1")
  end

  test "linear adapter reports review-decision pagination cursor and page-limit errors" do
    Application.put_env(:symphony_elixir, :linear_client_module, FakeLinearClient)

    Process.put(
      {FakeLinearClient, :graphql_result},
      {:ok,
       %{
         "data" => %{
           "issue" => %{
             "comments" => %{
               "nodes" => [%{"body" => "newest chatter", "createdAt" => "2026-01-03T00:00:00Z"}],
               "pageInfo" => %{"hasNextPage" => true, "endCursor" => ""}
             }
           }
         }
       }}
    )

    assert {:error, :review_decisions_missing_end_cursor} = Adapter.review_decisions("issue-1")

    Process.put(
      {FakeLinearClient, :graphql_results},
      List.duplicate(
        {:ok,
         %{
           "data" => %{
             "issue" => %{
               "comments" => %{
                 "nodes" => [%{"body" => "older chatter", "createdAt" => "2026-01-01T00:00:00Z"}],
                 "pageInfo" => %{"hasNextPage" => true, "endCursor" => "same-cursor"}
               }
             }
           }
         }},
        20
      )
    )

    assert {:error, :review_decisions_comment_page_limit_exceeded} = Adapter.review_decisions("issue-1")
  end

  test "linear adapter extracts review decisions from newest comment page" do
    Application.put_env(:symphony_elixir, :linear_client_module, FakeLinearClient)

    Process.put(
      {FakeLinearClient, :graphql_result},
      {:ok,
       %{
         "data" => %{
           "issue" => %{
             "comments" => %{
               "nodes" => [
                 %{"body" => "older unmarked comment", "createdAt" => "2026-01-01T00:00:00Z"},
                 %{
                   "body" => review_decision_comment("rejected", "same-slice", "first miss"),
                   "createdAt" => "2026-01-02T00:00:00Z"
                 },
                 %{
                   "body" => review_decision_comment("rejected", "same-slice", "second miss"),
                   "createdAt" => "2026-01-03T00:00:00Z"
                 }
               ]
             }
           }
         }
       }}
    )

    assert {:ok, decisions} = Adapter.review_decisions("issue-1")
    assert Enum.count(decisions, &(&1.status == "rejected" and &1.slice_id == "same-slice")) == 2

    assert_receive {:graphql_called, query, %{issueId: "issue-1", first: 100, after: nil}}
    assert query =~ "comments(first: $first, after: $after, orderBy: createdAt)"
    refute query =~ "comments(last:"
  end

  test "linear adapter paginates review decisions so older same-slice rejections are not missed" do
    Application.put_env(:symphony_elixir, :linear_client_module, FakeLinearClient)

    Process.put(
      {FakeLinearClient, :graphql_results},
      [
        {:ok,
         %{
           "data" => %{
             "issue" => %{
               "comments" => %{
                 "nodes" => [
                   %{"body" => "newest chatter", "createdAt" => "2026-01-03T00:00:00Z"}
                 ],
                 "pageInfo" => %{"hasNextPage" => true, "endCursor" => "cursor-page-1"}
               }
             }
           }
         }},
        {:ok,
         %{
           "data" => %{
             "issue" => %{
               "comments" => %{
                 "nodes" => [
                   %{
                     "body" => review_decision_comment("rejected", "sym-5-opencode-runner-contract-redesign-v1", "older first rejection"),
                     "createdAt" => "2026-01-01T00:00:00Z"
                   },
                   %{
                     "body" => review_decision_comment("rejected", "sym-5-opencode-runner-contract-redesign-v1", "older second rejection"),
                     "createdAt" => "2026-01-02T00:00:00Z"
                   }
                 ],
                 "pageInfo" => %{"hasNextPage" => false, "endCursor" => "cursor-page-2"}
               }
             }
           }
         }}
      ]
    )

    assert {:ok, decisions} = Adapter.review_decisions("issue-1")

    assert Enum.count(
             decisions,
             &(&1.status == "rejected" and &1.slice_id == "sym-5-opencode-runner-contract-redesign-v1")
           ) == 2

    assert_receive {:graphql_called, query, %{issueId: "issue-1", first: 100, after: nil}}
    assert query =~ "pageInfo"
    assert_receive {:graphql_called, _query, %{issueId: "issue-1", first: 100, after: "cursor-page-1"}}
  end

  test "phoenix observability api preserves state, issue, and refresh responses" do
    snapshot = static_snapshot()
    orchestrator_name = Module.concat(__MODULE__, :ObservabilityApiOrchestrator)

    {:ok, _pid} =
      StaticOrchestrator.start_link(
        name: orchestrator_name,
        snapshot: snapshot,
        refresh: %{
          queued: true,
          coalesced: false,
          requested_at: DateTime.utc_now(),
          operations: ["poll", "reconcile"]
        }
      )

    start_test_endpoint(orchestrator: orchestrator_name, snapshot_timeout_ms: 50)

    conn = get(build_conn(), "/api/v1/state")
    state_payload = json_response(conn, 200)

    assert state_payload == %{
             "generated_at" => state_payload["generated_at"],
             "counts" => %{"running" => 1, "retrying" => 1, "blocked" => 1},
             "attention" => %{
               "active_projects" => 1,
               "blocked" => 1,
               "in_review" => 0,
               "owner_input" => 0,
               "rca_required" => 0,
               "recent_failures" => 0,
               "retrying" => 1,
               "runnable_todo" => 0,
               "running" => 1,
               "stale" => 0
             },
             "issue_queue" => [],
             "review_items" => [],
             "owner_input_items" => [],
             "rca_required_items" => [],
             "stale_states" => [],
             "recent_failures" => [],
             "cleanup_status" => %{},
             "recent_activity" => [],
             "stewardship" => %{
               "active_milestone" => nil,
               "active_project_milestone_id" => nil,
               "eligible_issue_count" => 0,
               "running_count" => 1,
               "retrying_count" => 1,
               "blocked_count" => 1,
               "owner_input_count" => 0,
               "recent_suppression_reasons" => []
             },
             "dispatch_summary" => %{
               "active_milestone" => nil,
               "active_project_milestone_id" => nil,
               "eligible_issue_count" => 0,
               "running_count" => 1,
               "retrying_count" => 1,
               "blocked_count" => 1,
               "owner_input_count" => 0,
               "recent_suppression_reasons" => [],
               "dispatch_state" => "owner_blocked",
               "reason" => "Owner input or runtime block is preventing dispatch."
             },
             "running" => [
               %{
                 "issue_id" => "issue-http",
                 "issue_identifier" => "MT-HTTP",
                 "state" => "In Progress",
                 "worker_host" => nil,
                 "workspace_path" => nil,
                 "project" => %{
                   "id" => nil,
                   "name" => nil,
                   "root" => "/home/agent/proj/symphony"
                 },
                 "runner" => %{
                   "kind" => "opencode",
                   "owner" => "opencode",
                   "phase" => "command",
                   "project_root" => "/home/agent/proj/symphony",
                   "command" => ["opencode", "run", "--session", "thread-http"],
                   "attach_url" => "http://127.0.0.1:3000",
                   "result_state" => "running",
                   "failure" => nil,
                   "session_id" => "thread-http"
                 },
                 "session_id" => "thread-http",
                 "turn_count" => 7,
                 "last_runner_event" => "notification",
                 "last_runner_message" => "rendered",
                 "started_at" => state_payload["running"] |> List.first() |> Map.fetch!("started_at"),
                 "last_runner_event_at" => nil,
                 "tokens" => %{"input_tokens" => 4, "output_tokens" => 8, "total_tokens" => 12}
               }
             ],
             "retrying" => [
               %{
                 "issue_id" => "issue-retry",
                 "issue_identifier" => "MT-RETRY",
                 "attempt" => 2,
                 "due_at" => state_payload["retrying"] |> List.first() |> Map.fetch!("due_at"),
                 "error" => "boom",
                 "session_id" => nil,
                 "last_runner_event" => nil,
                 "last_runner_message" => nil,
                 "last_runner_event_at" => nil,
                 "runner" => %{
                   "kind" => "codex",
                   "owner" => "codex",
                   "phase" => "retry_wait",
                   "project_root" => nil,
                   "command" => nil,
                   "attach_url" => nil,
                   "result_state" => "retrying",
                   "failure" => "boom",
                   "session_id" => nil
                 },
                 "worker_host" => nil,
                 "workspace_path" => nil,
                 "project" => %{"id" => nil, "name" => nil, "root" => nil}
               }
             ],
             "blocked" => [
               %{
                 "issue_id" => "issue-blocked",
                 "issue_identifier" => "MT-BLOCKED",
                 "state" => "In Progress",
                 "error" => "codex turn requires operator input",
                 "worker_host" => "dm-dev2",
                 "workspace_path" => "/workspaces/MT-BLOCKED",
                 "project" => %{
                   "id" => nil,
                   "name" => nil,
                   "root" => "/workspaces/MT-BLOCKED"
                 },
                 "runner" => %{
                   "kind" => "codex",
                   "owner" => "codex",
                   "phase" => "blocked",
                   "project_root" => "/workspaces/MT-BLOCKED",
                   "command" => nil,
                   "attach_url" => nil,
                   "result_state" => "blocked",
                   "failure" => "codex turn requires operator input",
                   "session_id" => "thread-blocked"
                 },
                 "session_id" => "thread-blocked",
                 "blocked_at" => state_payload["blocked"] |> List.first() |> Map.fetch!("blocked_at"),
                 "last_runner_event" => "turn_input_required",
                 "last_runner_message" => "turn blocked: waiting for user input",
                 "last_runner_event_at" => state_payload["blocked"] |> List.first() |> Map.fetch!("last_runner_event_at")
               }
             ],
             "codex_totals" => %{
               "input_tokens" => 4,
               "output_tokens" => 8,
               "total_tokens" => 12,
               "seconds_running" => 42.5
             },
             "runner_runtime_totals" => %{"seconds_running" => 84.5},
             "suppression_events" => [],
             "suppression_counts" => %{},
             "rate_limits" => %{"primary" => %{"remaining" => 11}},
             "polling" => %{
               "checking?" => false,
               "next_poll_in_ms" => 5_000,
               "poll_interval_ms" => 2_000
             },
             "active_milestone" => nil
           }

    conn = get(build_conn(), "/api/v1/MT-HTTP")
    issue_payload = json_response(conn, 200)

    assert issue_payload == %{
             "issue_identifier" => "MT-HTTP",
             "issue_id" => "issue-http",
             "status" => "running",
             "workspace" => %{
               "path" => Path.join(Config.settings!().workspace.root, "MT-HTTP"),
               "host" => nil
             },
             "project" => %{
               "id" => nil,
               "name" => nil,
               "root" => "/home/agent/proj/symphony"
             },
             "attempts" => %{"restart_count" => 0, "current_retry_attempt" => 0},
             "running" => %{
               "worker_host" => nil,
               "workspace_path" => nil,
               "project" => %{
                 "id" => nil,
                 "name" => nil,
                 "root" => "/home/agent/proj/symphony"
               },
               "runner" => %{
                 "kind" => "opencode",
                 "owner" => "opencode",
                 "phase" => "command",
                 "project_root" => "/home/agent/proj/symphony",
                 "command" => ["opencode", "run", "--session", "thread-http"],
                 "attach_url" => "http://127.0.0.1:3000",
                 "result_state" => "running",
                 "failure" => nil,
                 "session_id" => "thread-http"
               },
               "session_id" => "thread-http",
               "turn_count" => 7,
               "state" => "In Progress",
               "started_at" => issue_payload["running"]["started_at"],
               "last_runner_event" => "notification",
               "last_runner_message" => "rendered",
               "last_runner_event_at" => nil,
               "tokens" => %{"input_tokens" => 4, "output_tokens" => 8, "total_tokens" => 12}
             },
             "retry" => nil,
             "blocked" => nil,
             "logs" => %{"codex_session_logs" => []},
             "matches" => [
               %{
                 "issue_id" => "issue-http",
                 "issue_identifier" => "MT-HTTP",
                 "project" => %{
                   "id" => nil,
                   "name" => nil,
                   "root" => "/home/agent/proj/symphony"
                 },
                 "runner" => %{
                   "kind" => "opencode",
                   "owner" => "opencode",
                   "phase" => "command",
                   "project_root" => "/home/agent/proj/symphony",
                   "command" => ["opencode", "run", "--session", "thread-http"],
                   "attach_url" => "http://127.0.0.1:3000",
                   "result_state" => "running",
                   "failure" => nil,
                   "session_id" => "thread-http"
                 },
                 "session_id" => "thread-http",
                 "status" => "running",
                 "workspace_path" => nil
               }
             ],
             "recent_events" => [],
             "last_error" => nil,
             "tracked" => %{}
           }

    conn = get(build_conn(), "/api/v1/MT-RETRY")

    assert %{
             "status" => "retrying",
             "retry" => %{
               "attempt" => 2,
               "error" => "boom",
               "runner" => %{
                 "kind" => "codex",
                 "owner" => "codex",
                 "phase" => "retry_wait",
                 "result_state" => "retrying",
                 "failure" => "boom"
               }
             }
           } =
             json_response(conn, 200)

    conn = get(build_conn(), "/api/v1/MT-BLOCKED")

    assert %{
             "status" => "blocked",
             "last_error" => "codex turn requires operator input",
             "blocked" => %{
               "session_id" => "thread-blocked",
               "state" => "In Progress",
               "error" => "codex turn requires operator input",
               "runner" => %{
                 "kind" => "codex",
                 "owner" => "codex",
                 "phase" => "blocked",
                 "result_state" => "blocked",
                 "failure" => "codex turn requires operator input"
               }
             }
           } = json_response(conn, 200)

    conn = get(build_conn(), "/api/v1/MT-MISSING")

    assert json_response(conn, 404) == %{
             "error" => %{"code" => "issue_not_found", "message" => "Issue not found"}
           }

    conn = post(build_conn(), "/api/v1/refresh", %{})

    assert %{"queued" => true, "coalesced" => false, "operations" => ["poll", "reconcile"]} =
             json_response(conn, 202)
  end

  test "orchestrator snapshot exposes active issue attention fields from last poll" do
    write_workflow_file!(Workflow.workflow_file_path(),
      tracker_active_states: ["Todo", "Preparing", "In Progress", "Need Owner Input", "In Review", "RCA Required"]
    )

    orchestrator_name = Module.concat(__MODULE__, :AttentionSnapshotOrchestrator)
    {:ok, pid} = Orchestrator.start_link(name: orchestrator_name, dispatch_paused?: true)

    on_exit(fn ->
      if Process.alive?(pid), do: Process.exit(pid, :normal)
    end)

    polled_at = DateTime.utc_now() |> DateTime.truncate(:second)

    :sys.replace_state(pid, fn state ->
      %{
        state
        | last_poll_at: polled_at,
          last_poll_issues: [
            %Issue{id: "issue-todo", identifier: "SYM-TODO", title: "Queued", state: "Todo"},
            %Issue{id: "issue-preparing", identifier: "SYM-PREP", title: "Preparing", state: "Preparing"},
            %Issue{id: "issue-review", identifier: "SYM-REVIEW", title: "Review", state: "In Review"},
            %Issue{id: "issue-owner", identifier: "SYM-OWNER", title: "Owner", state: "Need Owner Input"},
            %Issue{id: "issue-rca", identifier: "SYM-RCA", title: "RCA", state: "RCA Required"}
          ]
      }
    end)

    snapshot = GenServer.call(pid, :snapshot)

    assert Enum.map(snapshot.issue_queue, & &1.identifier) |> Enum.sort() == ["SYM-PREP", "SYM-TODO"]
    assert Enum.map(snapshot.review_items, & &1.identifier) == ["SYM-REVIEW"]
    assert Enum.map(snapshot.owner_input_items, & &1.identifier) == ["SYM-OWNER"]
    assert Enum.map(snapshot.rca_required_items, & &1.identifier) == ["SYM-RCA"]
    assert snapshot.cleanup_status == %{last_poll_at: polled_at, attempts: [], last_attempt: nil}
    assert [%{event: "poll", at: ^polled_at}] = snapshot.recent_activity
  end

  test "orchestrator recent activity excludes running entries without runner evidence" do
    orchestrator_name = Module.concat(__MODULE__, :RecentActivityEvidenceOrchestrator)
    {:ok, pid} = Orchestrator.start_link(name: orchestrator_name, dispatch_paused?: true)

    on_exit(fn ->
      if Process.alive?(pid), do: Process.exit(pid, :normal)
    end)

    event_at = ~U[2026-06-09 12:00:00Z]

    :sys.replace_state(pid, fn state ->
      %{
        state
        | running: %{
            "issue-empty" => %{
              identifier: "SYM-EMPTY",
              issue: %Issue{id: "issue-empty", identifier: "SYM-EMPTY", state: "In Progress"},
              session_id: "session-empty",
              runner_result_state: nil,
              last_codex_event: nil,
              last_codex_message: nil,
              last_codex_timestamp: nil,
              started_at: event_at
            },
            "issue-event" => %{
              identifier: "SYM-EVENT",
              issue: %Issue{id: "issue-event", identifier: "SYM-EVENT", state: "In Progress"},
              session_id: "session-event",
              runner_result_state: nil,
              last_codex_event: :notification,
              last_codex_message: "runner emitted an update",
              last_codex_timestamp: event_at,
              started_at: event_at
            }
          }
      }
    end)

    snapshot = GenServer.call(pid, :snapshot)

    assert Enum.map(snapshot.running, & &1.identifier) |> Enum.sort() == ["SYM-EMPTY", "SYM-EVENT"]
    assert [%{identifier: "SYM-EVENT", event: :notification, message: "runner emitted an update", at: ^event_at}] = snapshot.recent_activity
  end

  test "phoenix observability api exposes aggregate and project-scoped projections" do
    alpha_orchestrator = Module.concat(__MODULE__, :AggregateAlphaOrchestrator)
    beta_orchestrator = Module.concat(__MODULE__, :AggregateBetaOrchestrator)
    delta_orchestrator_key = SymphonyElixir.ProjectContext.process_names("delta").orchestrator

    unless Process.whereis(SymphonyElixir.ProjectRegistry) do
      start_supervised!(SymphonyElixir.ProjectRegistry)
    end

    {:ok, _alpha_pid} =
      StaticOrchestrator.start_link(
        name: alpha_orchestrator,
        snapshot:
          project_snapshot("alpha", "Alpha", "/projects/alpha", "SYM-6", "thread-alpha",
            issue_queue: [
              %{
                identifier: "SYM-23",
                title: "Prepare release",
                state: "Todo",
                blocked: true,
                blocked_by: [
                  %{identifier: "SYM-10", title: "Finish schema review", state: "In Review"}
                ],
                reason: "waiting for dependency SYM-10"
              }
            ],
            review_items: [%{identifier: "SYM-21", state: "In Review"}],
            owner_input_items: [%{identifier: "SYM-22", state: "Need Owner Input"}],
            rca_required_items: [%{identifier: "SYM-20", state: "RCA Required"}],
            stale_states: [%{identifier: "SYM-19", state: "In Progress", age_ms: 3_600_000, timeout_ms: 1_800_000}],
            recent_failures: [%{identifier: "SYM-18", error: "failed"}],
            cleanup_status: %{
              status: "failed",
              error: "workspace prune failed",
              attempts: [
                %{result: "failed", error: "permission denied", removed_count: 0}
              ],
              last_attempt: %{result: "failed", error: "permission denied", removed_count: 0}
            },
            recent_activity: [%{identifier: "SYM-17", event: "accepted"}],
            active_milestone: %{name: "SYM-23"}
          ),
        refresh: %{queued: true, coalesced: false, requested_at: DateTime.utc_now(), operations: ["poll"]}
      )

    beta_cleanup_status = %{last_poll_at: DateTime.utc_now(), attempts: [], last_attempt: nil}
    beta_root = "/projects/beta"
    beta_thread_id = "thread-beta"

    beta_snapshot =
      project_snapshot("beta", "Beta", beta_root, "SYM-6", beta_thread_id, cleanup_status: beta_cleanup_status)

    {:ok, _beta_pid} =
      StaticOrchestrator.start_link(
        name: beta_orchestrator,
        snapshot: beta_snapshot,
        refresh: %{queued: true, coalesced: true, requested_at: DateTime.utc_now(), operations: ["reconcile"]}
      )

    {:ok, _delta_pid} =
      StaticOrchestrator.start_link(
        name: SymphonyElixir.ProjectRegistry.via_name(delta_orchestrator_key),
        snapshot: project_snapshot("delta", "Delta", "/projects/delta", "SYM-7", "thread-delta"),
        refresh: %{queued: true, coalesced: false, requested_at: DateTime.utc_now(), operations: ["poll"]}
      )

    start_test_endpoint(
      project_states_provider: fn ->
        project_states(%{
          "alpha" => alpha_orchestrator,
          "beta" => beta_orchestrator,
          "delta" => delta_orchestrator_key,
          "gamma" => {:disabled, nil}
        })
      end,
      snapshot_timeout_ms: 50
    )

    aggregate_payload = json_response(get(build_conn(), "/api/v1/state"), 200)

    assert aggregate_payload["counts"] == %{"running" => 3, "retrying" => 0, "blocked" => 0}
    assert aggregate_payload["stewardship"]["running_count"] == 3
    assert aggregate_payload["stewardship"]["eligible_issue_count"] == 0
    assert aggregate_payload["dispatch_summary"]["dispatch_state"] == "no_eligible_work"

    assert aggregate_payload["attention"] == %{
             "active_projects" => 3,
             "blocked" => 0,
             "cleanup_problems" => 1,
             "dependency_blocked" => 1,
             "in_review" => 1,
             "owner_input" => 1,
             "rca_required" => 1,
             "recent_failures" => 1,
             "retrying" => 0,
             "runnable_todo" => 0,
             "running" => 3,
             "stale" => 1
           }

    assert Enum.map(aggregate_payload["projects"], & &1["id"]) == ["alpha", "beta", "delta", "gamma"]
    assert Enum.map(aggregate_payload["running"], & &1["project"]["id"]) == ["alpha", "beta", "delta"]
    assert Enum.map(aggregate_payload["running"], & &1["runner"]["owner"]) == ["opencode", "opencode", "opencode"]
    assert Enum.map(aggregate_payload["issue_queue"], & &1["project_id"]) == ["alpha"]
    assert [%{"identifier" => "SYM-23", "blockers" => [%{"identifier" => "SYM-10"}]}] = aggregate_payload["dependency_blocked_items"]
    assert [%{"reason" => "workspace prune failed"}] = aggregate_payload["cleanup_problem_items"]
    assert Enum.find(aggregate_payload["projects"], &(&1["id"] == "alpha"))["queue_depth"] == 1
    assert Enum.find(aggregate_payload["projects"], &(&1["id"] == "alpha"))["dependency_blocked_count"] == 1
    assert Enum.find(aggregate_payload["projects"], &(&1["id"] == "alpha"))["cleanup_problem_count"] == 1
    assert Enum.find(aggregate_payload["projects"], &(&1["id"] == "beta"))["cleanup_problem_count"] == 0
    assert Enum.find(aggregate_payload["projects"], &(&1["id"] == "alpha"))["active_milestone"] == %{"name" => "SYM-23"}
    assert Enum.find(aggregate_payload["projects"], &(&1["id"] == "gamma"))["status"] == "disabled"

    assert %{"counts" => %{"running" => 1}, "projects" => [%{"id" => "beta"}]} =
             json_response(get(build_conn(), "/api/v1/projects/beta/state"), 200)

    assert %{"dispatch_summary" => %{"running_count" => 1, "dispatch_state" => "no_eligible_work"}} =
             json_response(get(build_conn(), "/api/v1/projects/beta/state"), 200)

    {:ok, _metadata_view, metadata_html} = live(build_conn(), "/projects/beta")
    assert metadata_html =~ "No cleanup warnings in runtime state."
    refute metadata_html =~ "reported"

    {:ok, _cleanup_view, cleanup_html} = live(build_conn(), "/projects/alpha")
    assert cleanup_html =~ "permission denied"
    assert cleanup_html =~ "workspace prune failed"

    assert %{
             "issue_queue" => [%{"identifier" => "SYM-23"}],
             "dependency_blocked_items" => [%{"identifier" => "SYM-23", "blockers" => [%{"identifier" => "SYM-10"}]}],
             "projects" => [%{"id" => "alpha", "queue_depth" => 1}]
           } = json_response(get(build_conn(), "/api/v1/projects/alpha/state"), 200)

    assert %{
             "status" => "running",
             "matches" => [
               %{"project" => %{"id" => "alpha"}},
               %{"project" => %{"id" => "beta"}}
             ]
           } = json_response(get(build_conn(), "/api/v1/SYM-6"), 200)

    assert %{"issue_id" => "issue-beta"} =
             json_response(get(build_conn(), "/api/v1/projects/beta/issues/SYM-6"), 200)

    assert %{"issue_id" => "issue-alpha", "project" => %{"id" => "alpha"}} =
             json_response(get(build_conn(), "/api/v1/projects/alpha/issues/SYM-6"), 200)

    assert %{"error" => %{"code" => "issue_not_found"}} =
             json_response(get(build_conn(), "/api/v1/projects/alpha/issues/SYM-7"), 404)

    assert %{"issue_id" => "issue-delta"} =
             json_response(get(build_conn(), "/api/v1/projects/delta/issues/SYM-7"), 200)

    assert %{"error" => %{"code" => "project_unavailable"}} =
             json_response(get(build_conn(), "/api/v1/projects/gamma/state"), 503)

    assert %{"error" => %{"code" => "project_not_found"}} =
             json_response(get(build_conn(), "/api/v1/projects/missing/state"), 404)

    assert %{"queued" => true, "coalesced" => false, "operations" => ["poll"]} =
             json_response(post(build_conn(), "/api/v1/projects/alpha/refresh", %{}), 202)

    root_refresh_payload = json_response(post(build_conn(), "/api/v1/refresh", %{}), 202)

    assert %{"queued" => true, "coalesced" => false, "operations" => ["poll", "reconcile"]} =
             root_refresh_payload

    assert Enum.map(root_refresh_payload["projects"], & &1["id"]) == ["alpha", "beta", "delta"]
    assert Enum.map(root_refresh_payload["projects"], &get_in(&1, ["refresh", "operations"])) == [["poll"], ["reconcile"], ["poll"]]
    refute Enum.any?(root_refresh_payload["projects"], &(&1["id"] == "gamma"))

    html = html_response(get(build_conn(), "/"), 200)
    assert html =~ "Active project overview"
    assert html =~ "Needs attention"
    assert html =~ "Alpha"
    assert html =~ "Beta"
    assert html =~ "Delta"
    assert html =~ "Gamma"
    assert html =~ "/projects/alpha"
    assert html =~ "/api/v1/projects/alpha/state"
    refute html =~ ~s(href="/api/v1/projects/alpha/refresh")
    assert html =~ "Refresh API: POST only"

    project_html = html_response(get(build_conn(), "/projects/alpha"), 200)
    assert project_html =~ "Project drilldown"
    assert project_html =~ "SYM-23"
    assert project_html =~ "SYM-10"
    assert project_html =~ "Finish schema review"
    assert project_html =~ "waiting for dependency SYM-10"
    assert project_html =~ "workspace prune failed"
    assert project_html =~ "permission denied"
    assert project_html =~ "Running 1 active session(s): 1 running; 1 dependency blocked; 1 waiting review; 1 owner input; 1 RCA; 1 stale; 1 recent failure; cleanup workspace prune failed."
    assert project_html =~ "state: In Review"
    assert project_html =~ "state: Need Owner Input"
    assert project_html =~ "state: RCA Required"
    assert project_html =~ "age: 1h / timeout 30m"
    assert project_html =~ "error: failed"
    assert project_html =~ "SYM-17"

    unavailable_project_html = html_response(get(build_conn(), "/projects/gamma"), 200)
    assert unavailable_project_html =~ "Project drilldown"
    assert unavailable_project_html =~ "Gamma"
    assert unavailable_project_html =~ "0 running / 0 retrying / 0 blocked"
    assert unavailable_project_html =~ "No queued issues in runtime state."
  end

  test "phoenix observability api preserves 405, 404, and unavailable behavior" do
    unavailable_orchestrator = Module.concat(__MODULE__, :UnavailableOrchestrator)
    start_test_endpoint(orchestrator: unavailable_orchestrator, snapshot_timeout_ms: 5)

    assert json_response(post(build_conn(), "/api/v1/state", %{}), 405) ==
             %{"error" => %{"code" => "method_not_allowed", "message" => "Method not allowed"}}

    assert json_response(get(build_conn(), "/api/v1/refresh"), 405) ==
             %{"error" => %{"code" => "method_not_allowed", "message" => "Method not allowed"}}

    assert json_response(post(build_conn(), "/", %{}), 405) ==
             %{"error" => %{"code" => "method_not_allowed", "message" => "Method not allowed"}}

    assert json_response(post(build_conn(), "/api/v1/MT-1", %{}), 405) ==
             %{"error" => %{"code" => "method_not_allowed", "message" => "Method not allowed"}}

    assert json_response(get(build_conn(), "/unknown"), 404) ==
             %{"error" => %{"code" => "not_found", "message" => "Route not found"}}

    state_payload = json_response(get(build_conn(), "/api/v1/state"), 200)

    assert state_payload ==
             %{
               "generated_at" => state_payload["generated_at"],
               "error" => %{"code" => "snapshot_unavailable", "message" => "Snapshot unavailable"}
             }

    assert json_response(post(build_conn(), "/api/v1/refresh", %{}), 503) ==
             %{
               "error" => %{
                 "code" => "orchestrator_unavailable",
                 "message" => "Orchestrator is unavailable"
               }
             }
  end

  test "phoenix observability api preserves snapshot timeout behavior" do
    timeout_orchestrator = Module.concat(__MODULE__, :TimeoutOrchestrator)
    {:ok, _pid} = SlowOrchestrator.start_link(name: timeout_orchestrator)
    start_test_endpoint(orchestrator: timeout_orchestrator, snapshot_timeout_ms: 1)

    timeout_payload = json_response(get(build_conn(), "/api/v1/state"), 200)

    assert timeout_payload ==
             %{
               "generated_at" => timeout_payload["generated_at"],
               "error" => %{"code" => "snapshot_timeout", "message" => "Snapshot timed out"}
             }
  end

  test "project-scoped observability api returns 503 when project snapshot times out" do
    timeout_orchestrator = Module.concat(__MODULE__, :ProjectStateTimeoutOrchestrator)
    {:ok, _pid} = SlowOrchestrator.start_link(name: timeout_orchestrator)

    start_test_endpoint(
      project_states_provider: fn ->
        project_states(%{"epsilon" => timeout_orchestrator})
      end,
      snapshot_timeout_ms: 1
    )

    assert %{"error" => %{"code" => "project_unavailable"}} =
             json_response(get(build_conn(), "/api/v1/projects/epsilon/state"), 503)
  end

  test "dashboard project drilldown falls back to aggregate metadata when scoped snapshot times out" do
    timeout_orchestrator = Module.concat(__MODULE__, :ProjectDrilldownTimeoutOrchestrator)
    {:ok, _pid} = SlowOrchestrator.start_link(name: timeout_orchestrator)

    start_test_endpoint(
      project_states_provider: fn ->
        project_states(%{"epsilon" => timeout_orchestrator})
      end,
      snapshot_timeout_ms: 1
    )

    project_html = html_response(get(build_conn(), "/projects/epsilon"), 200)

    assert project_html =~ "Project drilldown"
    assert project_html =~ "Epsilon"
    assert project_html =~ "Project snapshot unavailable"
    assert project_html =~ "snapshot_timeout"
    assert project_html =~ "No queued issues in runtime state."
  end

  test "dashboard bootstraps liveview from embedded static assets" do
    orchestrator_name = Module.concat(__MODULE__, :AssetOrchestrator)

    {:ok, _pid} =
      StaticOrchestrator.start_link(
        name: orchestrator_name,
        snapshot: static_snapshot(),
        refresh: %{
          queued: true,
          coalesced: false,
          requested_at: DateTime.utc_now(),
          operations: ["poll"]
        }
      )

    start_test_endpoint(orchestrator: orchestrator_name, snapshot_timeout_ms: 50)

    html = html_response(get(build_conn(), "/"), 200)
    assert html =~ "/dashboard.css"
    assert html =~ "/vendor/phoenix_html/phoenix_html.js"
    assert html =~ "/vendor/phoenix/phoenix.js"
    assert html =~ "/vendor/phoenix_live_view/phoenix_live_view.js"
    refute html =~ "/assets/app.js"
    refute html =~ "<style>"

    dashboard_css = response(get(build_conn(), "/dashboard.css"), 200)
    assert dashboard_css =~ ":root {"
    assert dashboard_css =~ ".status-badge-live"
    assert dashboard_css =~ "[data-phx-main].phx-connected .status-badge-live"
    assert dashboard_css =~ "[data-phx-main].phx-connected .status-badge-offline"

    phoenix_html_js = response(get(build_conn(), "/vendor/phoenix_html/phoenix_html.js"), 200)
    assert phoenix_html_js =~ "phoenix.link.click"

    phoenix_js = response(get(build_conn(), "/vendor/phoenix/phoenix.js"), 200)
    assert phoenix_js =~ "var Phoenix = (() => {"

    live_view_js =
      response(get(build_conn(), "/vendor/phoenix_live_view/phoenix_live_view.js"), 200)

    assert live_view_js =~ "var LiveView = (() => {"
  end

  test "dashboard liveview renders and refreshes over pubsub" do
    orchestrator_name = Module.concat(__MODULE__, :DashboardOrchestrator)
    snapshot = static_snapshot()

    {:ok, orchestrator_pid} =
      StaticOrchestrator.start_link(
        name: orchestrator_name,
        snapshot: snapshot,
        refresh: %{
          queued: true,
          coalesced: true,
          requested_at: DateTime.utc_now(),
          operations: ["poll"]
        }
      )

    start_test_endpoint(orchestrator: orchestrator_name, snapshot_timeout_ms: 50)

    {:ok, view, html} = live(build_conn(), "/")
    assert html =~ "Operations Dashboard"
    assert html =~ "MT-HTTP"
    assert html =~ "MT-RETRY"
    assert html =~ "MT-BLOCKED"
    assert html =~ "rendered"
    assert html =~ "turn blocked: waiting for user input"
    assert html =~ "Runtime"
    assert html =~ "Live"
    assert html =~ "Offline"
    assert html =~ "Copy ID"
    assert html =~ "Runner update"
    assert html =~ "opencode"
    assert html =~ "command"
    refute html =~ "data-runtime-clock="
    refute html =~ "setInterval(refreshRuntimeClocks"
    refute html =~ "Refresh now"
    refute html =~ "Transport"
    assert html =~ "status-badge-live"
    assert html =~ "status-badge-offline"

    updated_snapshot =
      put_in(snapshot.running, [
        %{
          issue_id: "issue-http",
          identifier: "MT-HTTP",
          state: "In Progress",
          session_id: "thread-http",
          turn_count: 8,
          last_codex_event: :notification,
          last_codex_message: %{
            event: :notification,
            message: %{
              payload: %{
                "method" => "codex/event/agent_message_content_delta",
                "params" => %{
                  "msg" => %{
                    "content" => "structured update"
                  }
                }
              }
            }
          },
          last_codex_timestamp: DateTime.utc_now(),
          codex_input_tokens: 10,
          codex_output_tokens: 12,
          codex_total_tokens: 22,
          started_at: DateTime.utc_now()
        }
      ])

    :sys.replace_state(orchestrator_pid, fn state ->
      Keyword.put(state, :snapshot, updated_snapshot)
    end)

    StatusDashboard.notify_update()

    assert_eventually(fn ->
      render(view) =~ "agent message content streaming: structured update"
    end)
  end

  test "dashboard liveview renders an unavailable state without crashing" do
    start_test_endpoint(
      orchestrator: Module.concat(__MODULE__, :MissingDashboardOrchestrator),
      snapshot_timeout_ms: 5
    )

    {:ok, _view, html} = live(build_conn(), "/")
    assert html =~ "Snapshot unavailable"
    assert html =~ "snapshot_unavailable"
  end

  test "http server serves embedded assets, accepts form posts, and rejects invalid hosts" do
    spec = HttpServer.child_spec(port: 0)
    assert spec.id == HttpServer
    assert spec.start == {HttpServer, :start_link, [[port: 0]]}

    assert :ignore = HttpServer.start_link(port: nil)
    assert HttpServer.bound_port() == nil

    snapshot = static_snapshot()
    orchestrator_name = Module.concat(__MODULE__, :BoundPortOrchestrator)

    refresh = %{
      queued: true,
      coalesced: false,
      requested_at: DateTime.utc_now(),
      operations: ["poll"]
    }

    server_opts = [
      host: "127.0.0.1",
      port: 0,
      orchestrator: orchestrator_name,
      snapshot_timeout_ms: 50
    ]

    start_supervised!({StaticOrchestrator, name: orchestrator_name, snapshot: snapshot, refresh: refresh})

    start_supervised!({HttpServer, server_opts})

    port = wait_for_bound_port()
    assert port == HttpServer.bound_port()

    response = Req.get!("http://127.0.0.1:#{port}/api/v1/state")
    assert response.status == 200
    assert response.body["counts"] == %{"running" => 1, "retrying" => 1, "blocked" => 1}

    dashboard_css = Req.get!("http://127.0.0.1:#{port}/dashboard.css")
    assert dashboard_css.status == 200
    assert dashboard_css.body =~ ":root {"

    phoenix_js = Req.get!("http://127.0.0.1:#{port}/vendor/phoenix/phoenix.js")
    assert phoenix_js.status == 200
    assert phoenix_js.body =~ "var Phoenix = (() => {"

    refresh_response =
      Req.post!("http://127.0.0.1:#{port}/api/v1/refresh",
        headers: [{"content-type", "application/x-www-form-urlencoded"}],
        body: ""
      )

    assert refresh_response.status == 202
    assert refresh_response.body["queued"] == true

    method_not_allowed_response =
      Req.post!("http://127.0.0.1:#{port}/api/v1/state",
        headers: [{"content-type", "application/x-www-form-urlencoded"}],
        body: ""
      )

    assert method_not_allowed_response.status == 405
    assert method_not_allowed_response.body["error"]["code"] == "method_not_allowed"

    assert {:error, _reason} = HttpServer.start_link(host: "bad host", port: 0)
  end

  defp start_test_endpoint(overrides) do
    endpoint_config =
      :symphony_elixir
      |> Application.get_env(SymphonyElixirWeb.Endpoint, [])
      |> Keyword.merge(server: false, secret_key_base: String.duplicate("s", 64))
      |> Keyword.merge(overrides)

    Application.put_env(:symphony_elixir, SymphonyElixirWeb.Endpoint, endpoint_config)
    start_supervised!({SymphonyElixirWeb.Endpoint, []})
  end

  defp project_states(project_orchestrators) do
    Map.new(project_orchestrators, fn {project_id, project_status} ->
      {status, orchestrator} = project_status(project_status)

      {project_id,
       %{
         status: status,
         context: %SymphonyElixir.ProjectContext{
           id: project_id,
           project_id: project_id,
           name: String.capitalize(project_id),
           enabled: status == :running,
           status: if(status == :running, do: :valid, else: :disabled),
           repo_root: "/projects/#{project_id}",
           app_root: "/projects/#{project_id}/elixir",
           workflow_path: "/projects/#{project_id}/WORKFLOW.md",
           dashboard_order: if(project_id == "alpha", do: 1, else: 2),
           logs_root: "/projects/#{project_id}/logs",
           linear: %{},
           mnemesh: %{},
           runner: %{"default" => "opencode"},
           execution: %{"enabled" => true},
           gates: %{"dispatch_enabled" => true},
           errors: [],
           process_names: %{orchestrator: orchestrator}
         },
         pid: if(status == :running, do: self()),
         error: nil
       }}
    end)
  end

  defp project_status({status, orchestrator}), do: {status, orchestrator}
  defp project_status(orchestrator), do: {:running, orchestrator}

  defp project_snapshot(project_id, project_name, project_root, issue_identifier, session_id, overrides \\ []) do
    Map.merge(
      %{
        running: [
          %{
            issue_id: "issue-#{project_id}",
            identifier: issue_identifier,
            project_id: project_id,
            project_name: project_name,
            project_root: project_root,
            state: "In Progress",
            session_id: session_id,
            turn_count: 3,
            codex_input_tokens: 11,
            codex_output_tokens: 5,
            codex_total_tokens: 16,
            last_codex_message: "project #{project_id} update",
            last_codex_timestamp: nil,
            last_codex_event: :notification,
            runner_kind: "opencode",
            runner_owner: "opencode",
            runner_phase: :command,
            runner_project_root: project_root,
            runner_command: ["opencode", "run", "--session", session_id],
            runner_attach_url: "http://127.0.0.1:3000/session/#{session_id}",
            runner_result_state: "running",
            runner_failure: nil,
            workspace_path: "/workspaces/#{project_id}/#{issue_identifier}",
            started_at: DateTime.utc_now()
          }
        ],
        retrying: [],
        blocked: [],
        codex_totals: %{input_tokens: 11, output_tokens: 5, total_tokens: 16, seconds_running: 12},
        runner_runtime_totals: %{seconds_running: 12},
        suppression_events: [],
        suppression_counts: %{},
        stewardship: %{
          active_milestone: nil,
          active_project_milestone_id: nil,
          eligible_issue_count: 0,
          running_count: 1,
          retrying_count: 0,
          blocked_count: 0,
          owner_input_count: 0,
          recent_suppression_reasons: []
        },
        dispatch_summary: %{
          active_milestone: nil,
          active_project_milestone_id: nil,
          eligible_issue_count: 0,
          running_count: 1,
          retrying_count: 0,
          blocked_count: 0,
          owner_input_count: 0,
          recent_suppression_reasons: [],
          dispatch_state: :no_eligible_work,
          reason: "No eligible issue is available for the current Linear ordering and worker policy."
        },
        rate_limits: nil,
        polling: %{checking?: false, next_poll_in_ms: 5_000, poll_interval_ms: 2_000},
        active_milestone: nil
      },
      Map.new(overrides)
    )
  end

  defp static_snapshot do
    %{
      running: [
        %{
          issue_id: "issue-http",
          identifier: "MT-HTTP",
          state: "In Progress",
          session_id: "thread-http",
          turn_count: 7,
          codex_app_server_pid: nil,
          last_codex_message: "rendered",
          last_codex_timestamp: nil,
          last_codex_event: :notification,
          codex_input_tokens: 4,
          codex_output_tokens: 8,
          codex_total_tokens: 12,
          runner_kind: "opencode",
          runner_owner: "opencode",
          runner_phase: "command",
          runner_project_root: "/home/agent/proj/symphony",
          runner_command: ["opencode", "run", "--session", "thread-http"],
          runner_attach_url: "http://127.0.0.1:3000",
          runner_result_state: "running",
          runner_failure: nil,
          started_at: DateTime.utc_now()
        }
      ],
      retrying: [
        %{
          issue_id: "issue-retry",
          identifier: "MT-RETRY",
          attempt: 2,
          due_in_ms: 2_000,
          error: "boom",
          runner_kind: "codex",
          runner_owner: "codex",
          runner_phase: "retry_wait",
          runner_result_state: "retrying",
          runner_failure: "boom"
        }
      ],
      blocked: [
        %{
          issue_id: "issue-blocked",
          identifier: "MT-BLOCKED",
          state: "In Progress",
          error: "codex turn requires operator input",
          worker_host: "dm-dev2",
          workspace_path: "/workspaces/MT-BLOCKED",
          runner_kind: "codex",
          runner_owner: "codex",
          runner_phase: "blocked",
          runner_project_root: "/workspaces/MT-BLOCKED",
          runner_result_state: "blocked",
          runner_failure: "codex turn requires operator input",
          session_id: "thread-blocked",
          blocked_at: DateTime.utc_now(),
          last_codex_event: :turn_input_required,
          last_codex_message: %{
            event: :turn_input_required,
            message: %{"method" => "turn/input_required"},
            timestamp: DateTime.utc_now()
          },
          last_codex_timestamp: DateTime.utc_now()
        }
      ],
      codex_totals: %{input_tokens: 4, output_tokens: 8, total_tokens: 12, seconds_running: 42.5},
      runner_runtime_totals: %{seconds_running: 84.5},
      stewardship: %{
        active_milestone: nil,
        active_project_milestone_id: nil,
        eligible_issue_count: 0,
        running_count: 1,
        retrying_count: 1,
        blocked_count: 1,
        owner_input_count: 0,
        recent_suppression_reasons: []
      },
      dispatch_summary: %{
        active_milestone: nil,
        active_project_milestone_id: nil,
        eligible_issue_count: 0,
        running_count: 1,
        retrying_count: 1,
        blocked_count: 1,
        owner_input_count: 0,
        recent_suppression_reasons: [],
        dispatch_state: :owner_blocked,
        reason: "Owner input or runtime block is preventing dispatch."
      },
      rate_limits: %{"primary" => %{"remaining" => 11}},
      polling: %{
        "checking?" => false,
        "next_poll_in_ms" => 5_000,
        "poll_interval_ms" => 2_000
      },
      active_milestone: nil
    }
  end

  defp wait_for_bound_port do
    assert_eventually(fn ->
      is_integer(HttpServer.bound_port())
    end)

    HttpServer.bound_port()
  end

  defp assert_eventually(fun, attempts \\ 20)

  defp assert_eventually(fun, attempts) when attempts > 0 do
    if fun.() do
      true
    else
      Process.sleep(25)
      assert_eventually(fun, attempts - 1)
    end
  end

  defp assert_eventually(_fun, 0), do: flunk("condition not met in time")

  defp ensure_workflow_store_running do
    if Process.whereis(WorkflowStore) do
      :ok
    else
      case Supervisor.restart_child(SymphonyElixir.Supervisor, WorkflowStore) do
        {:ok, _pid} -> :ok
        {:error, {:already_started, _pid}} -> :ok
      end
    end
  end

  defp opencode_task_prompt_comment(slice_id, prompt) do
    """
    <!-- symphony:opencode-task-prompt:v1 slice_id=#{slice_id} -->
    ```text
    #{prompt}
    ```
    """
  end

  defp review_decision_comment(status, slice_id, reason) do
    """
    <!-- symphony:review-decision:v1 -->
    ```text
    status: #{status}
    slice_id: #{slice_id}
    reason: #{reason}
    ```
    """
  end
end
