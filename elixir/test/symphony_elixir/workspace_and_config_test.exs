defmodule SymphonyElixir.WorkspaceAndConfigTest do
  use SymphonyElixir.TestSupport
  alias Ecto.Changeset
  alias SymphonyElixir.Config.Schema
  alias SymphonyElixir.Config.Schema.{Codex, StringOrMap}
  alias SymphonyElixir.Linear.Client
  alias SymphonyElixir.OpenCode.ACPSessionStore
  alias SymphonyElixir.PromptBuilder
  alias SymphonyElixir.RuntimeCache
  alias SymphonyElixir.Steward.ExecutionPacket

  test "workspace bootstrap can be implemented in after_create hook" do
    test_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-elixir-workspace-hook-bootstrap-#{System.unique_integer([:positive])}"
      )

    try do
      template_repo = Path.join(test_root, "source")
      workspace_root = Path.join(test_root, "workspaces")

      File.mkdir_p!(template_repo)
      File.mkdir_p!(Path.join(template_repo, "keep"))
      File.write!(Path.join([template_repo, "keep", "file.txt"]), "keep me")
      File.write!(Path.join(template_repo, "README.md"), "hook clone\n")
      System.cmd("git", ["-C", template_repo, "init", "-b", "main"])
      System.cmd("git", ["-C", template_repo, "config", "user.name", "Test User"])
      System.cmd("git", ["-C", template_repo, "config", "user.email", "test@example.com"])
      System.cmd("git", ["-C", template_repo, "add", "README.md", "keep/file.txt"])
      System.cmd("git", ["-C", template_repo, "commit", "-m", "initial"])

      write_workflow_file!(Workflow.workflow_file_path(),
        workspace_root: workspace_root,
        hook_after_create: "git clone --depth 1 #{template_repo} ."
      )

      assert {:ok, workspace} = Workspace.create_for_issue("S-1")
      assert File.exists?(Path.join(workspace, ".git"))
      assert File.read!(Path.join(workspace, "README.md")) == "hook clone\n"
      assert File.read!(Path.join([workspace, "keep", "file.txt"])) == "keep me"
    after
      File.rm_rf(test_root)
    end
  end

  test "workspace path is deterministic per issue identifier" do
    workspace_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-elixir-workspace-deterministic-#{System.unique_integer([:positive])}"
      )

    write_workflow_file!(Workflow.workflow_file_path(), workspace_root: workspace_root)

    assert {:ok, first_workspace} = Workspace.create_for_issue("MT/Det")
    assert {:ok, second_workspace} = Workspace.create_for_issue("MT/Det")

    assert first_workspace == second_workspace
    assert Path.basename(first_workspace) == "MT_Det"
  end

  test "workspace reuses existing issue directory without deleting local changes" do
    workspace_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-elixir-workspace-reuse-#{System.unique_integer([:positive])}"
      )

    try do
      write_workflow_file!(Workflow.workflow_file_path(),
        workspace_root: workspace_root,
        hook_after_create: "echo first > README.md"
      )

      assert {:ok, first_workspace} = Workspace.create_for_issue("MT-REUSE")

      File.write!(Path.join(first_workspace, "README.md"), "changed\n")
      File.write!(Path.join(first_workspace, "local-progress.txt"), "in progress\n")
      File.mkdir_p!(Path.join(first_workspace, "deps"))
      File.mkdir_p!(Path.join(first_workspace, "_build"))
      File.mkdir_p!(Path.join(first_workspace, "tmp"))
      File.write!(Path.join([first_workspace, "deps", "cache.txt"]), "cached deps\n")
      File.write!(Path.join([first_workspace, "_build", "artifact.txt"]), "compiled artifact\n")
      File.write!(Path.join([first_workspace, "tmp", "scratch.txt"]), "remove me\n")

      assert {:ok, second_workspace} = Workspace.create_for_issue("MT-REUSE")
      assert second_workspace == first_workspace
      assert File.read!(Path.join(second_workspace, "README.md")) == "changed\n"
      assert File.read!(Path.join(second_workspace, "local-progress.txt")) == "in progress\n"
      assert File.read!(Path.join([second_workspace, "deps", "cache.txt"])) == "cached deps\n"

      assert File.read!(Path.join([second_workspace, "_build", "artifact.txt"])) ==
               "compiled artifact\n"

      assert File.read!(Path.join([second_workspace, "tmp", "scratch.txt"])) == "remove me\n"
    after
      File.rm_rf(workspace_root)
    end
  end

  test "workspace replaces stale non-directory paths" do
    workspace_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-elixir-workspace-stale-path-#{System.unique_integer([:positive])}"
      )

    try do
      stale_workspace = Path.join(workspace_root, "MT-STALE")
      File.mkdir_p!(workspace_root)
      File.write!(stale_workspace, "old state\n")

      write_workflow_file!(Workflow.workflow_file_path(), workspace_root: workspace_root)

      assert {:ok, canonical_workspace} = SymphonyElixir.PathSafety.canonicalize(stale_workspace)
      assert {:ok, workspace} = Workspace.create_for_issue("MT-STALE")
      assert workspace == canonical_workspace
      assert File.dir?(workspace)
    after
      File.rm_rf(workspace_root)
    end
  end

  test "workspace rejects symlink escapes under the configured root" do
    test_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-elixir-workspace-symlink-#{System.unique_integer([:positive])}"
      )

    try do
      workspace_root = Path.join(test_root, "workspaces")
      outside_root = Path.join(test_root, "outside")
      symlink_path = Path.join(workspace_root, "MT-SYM")

      File.mkdir_p!(workspace_root)
      File.mkdir_p!(outside_root)
      File.ln_s!(outside_root, symlink_path)

      write_workflow_file!(Workflow.workflow_file_path(), workspace_root: workspace_root)

      assert {:ok, canonical_outside_root} = SymphonyElixir.PathSafety.canonicalize(outside_root)

      assert {:ok, canonical_workspace_root} =
               SymphonyElixir.PathSafety.canonicalize(workspace_root)

      assert {:error, {:workspace_outside_root, ^canonical_outside_root, ^canonical_workspace_root}} =
               Workspace.create_for_issue("MT-SYM")
    after
      File.rm_rf(test_root)
    end
  end

  test "workspace canonicalizes symlinked workspace roots before creating issue directories" do
    test_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-elixir-workspace-root-symlink-#{System.unique_integer([:positive])}"
      )

    try do
      actual_root = Path.join(test_root, "actual-workspaces")
      linked_root = Path.join(test_root, "linked-workspaces")

      File.mkdir_p!(actual_root)
      File.ln_s!(actual_root, linked_root)

      write_workflow_file!(Workflow.workflow_file_path(), workspace_root: linked_root)

      assert {:ok, canonical_workspace} =
               SymphonyElixir.PathSafety.canonicalize(Path.join(actual_root, "MT-LINK"))

      assert {:ok, workspace} = Workspace.create_for_issue("MT-LINK")
      assert workspace == canonical_workspace
      assert File.dir?(workspace)
    after
      File.rm_rf(test_root)
    end
  end

  test "workspace remove rejects the workspace root itself with a distinct error" do
    workspace_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-elixir-workspace-root-remove-#{System.unique_integer([:positive])}"
      )

    try do
      File.mkdir_p!(workspace_root)
      write_workflow_file!(Workflow.workflow_file_path(), workspace_root: workspace_root)

      assert {:ok, canonical_workspace_root} =
               SymphonyElixir.PathSafety.canonicalize(workspace_root)

      assert {:error, {:workspace_equals_root, ^canonical_workspace_root, ^canonical_workspace_root}, ""} =
               Workspace.remove(workspace_root)
    after
      File.rm_rf(workspace_root)
    end
  end

  test "workspace remove rejects the current working tree" do
    workspace_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-elixir-workspace-current-remove-#{System.unique_integer([:positive])}"
      )

    try do
      File.mkdir_p!(workspace_root)
      write_workflow_file!(Workflow.workflow_file_path(), workspace_root: workspace_root)

      assert {:ok, workspace} = Workspace.create_for_issue("MT-CWD")

      File.cd!(workspace, fn ->
        assert {:error, {:workspace_contains_current_working_directory, ^workspace, ^workspace}, ""} = Workspace.remove(workspace)
      end)

      assert File.dir?(workspace)
    after
      File.rm_rf(workspace_root)
    end
  end

  test "workspace surfaces after_create hook failures" do
    workspace_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-elixir-workspace-hook-failure-#{System.unique_integer([:positive])}"
      )

    try do
      write_workflow_file!(Workflow.workflow_file_path(),
        workspace_root: workspace_root,
        hook_after_create: "echo nope && exit 17"
      )

      assert {:error, {:workspace_hook_failed, "after_create", 17, _output}} =
               Workspace.create_for_issue("MT-FAIL")
    after
      File.rm_rf(workspace_root)
    end
  end

  test "workspace surfaces after_create hook timeouts" do
    workspace_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-elixir-workspace-hook-timeout-#{System.unique_integer([:positive])}"
      )

    try do
      write_workflow_file!(Workflow.workflow_file_path(),
        workspace_root: workspace_root,
        hook_timeout_ms: 10,
        hook_after_create: "sleep 1"
      )

      assert {:error, {:workspace_hook_timeout, "after_create", 10}} =
               Workspace.create_for_issue("MT-TIMEOUT")
    after
      File.rm_rf(workspace_root)
    end
  end

  test "workspace creates an empty directory when no bootstrap hook is configured" do
    workspace_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-workspace-empty-#{System.unique_integer([:positive])}"
      )

    try do
      write_workflow_file!(Workflow.workflow_file_path(), workspace_root: workspace_root)

      workspace = Path.join(workspace_root, "MT-608")
      assert {:ok, canonical_workspace} = SymphonyElixir.PathSafety.canonicalize(workspace)

      assert {:ok, ^canonical_workspace} = Workspace.create_for_issue("MT-608")
      assert File.dir?(workspace)
      assert {:ok, []} = File.ls(workspace)
    after
      File.rm_rf(workspace_root)
    end
  end

  test "workspace removes all workspaces for a closed issue identifier" do
    workspace_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-elixir-issue-workspace-cleanup-#{System.unique_integer([:positive])}"
      )

    try do
      target_workspace = Path.join(workspace_root, "S_1")

      untouched_workspace =
        Path.join(workspace_root, "OTHER-#{System.unique_integer([:positive])}")

      File.mkdir_p!(target_workspace)
      File.mkdir_p!(untouched_workspace)
      File.write!(Path.join(target_workspace, "marker.txt"), "stale")
      File.write!(Path.join(untouched_workspace, "marker.txt"), "keep")

      write_workflow_file!(Workflow.workflow_file_path(), workspace_root: workspace_root)

      assert :ok = Workspace.remove_issue_workspaces("S_1")
      refute File.exists?(target_workspace)
      assert File.exists?(untouched_workspace)
    after
      File.rm_rf(workspace_root)
    end
  end

  test "workspace cleanup handles missing workspace root" do
    missing_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-elixir-missing-workspaces-#{System.unique_integer([:positive])}"
      )

    write_workflow_file!(Workflow.workflow_file_path(), workspace_root: missing_root)

    assert :ok = Workspace.remove_issue_workspaces("S-2")
  end

  test "workspace cleanup ignores non-binary identifier" do
    assert :ok = Workspace.remove_issue_workspaces(nil)
  end

  test "runtime cache cleanup removes terminal issue workspace and ACP session" do
    workspace_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-runtime-cache-terminal-#{System.unique_integer([:positive])}"
      )

    try do
      write_workflow_file!(Workflow.workflow_file_path(), workspace_root: workspace_root)

      issue = %Issue{
        id: "issue-terminal-cache",
        identifier: "SYM-CACHE",
        state: "Done"
      }

      assert {:ok, workspace} = Workspace.create_for_issue(issue)
      File.write!(Path.join(workspace, "artifact.txt"), "stale")

      project_root = File.cwd!()
      session_scope = ACPSessionStore.prompt_scope("prompt")
      assert :ok = ACPSessionStore.put(issue, project_root, "ses-stale", session_scope)
      assert {:ok, "ses-stale"} = ACPSessionStore.fetch(issue, project_root, session_scope)

      assert :ok = Workspace.cleanup_issue_runtime_cache(issue, Config.settings!())

      refute File.exists?(workspace)
      assert {:ok, nil} = ACPSessionStore.fetch(issue, project_root, session_scope)
    after
      File.rm_rf(workspace_root)
    end
  end

  test "runtime cache cleanup reports per-step errors after attempting all cleanup steps" do
    test_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-runtime-cache-error-#{System.unique_integer([:positive])}"
      )

    blocking_path = Path.join(test_root, "workspace-root-file")
    issue = %Issue{id: "issue-runtime-cache-error", identifier: "SYM-CACHE-ERROR", state: "Done"}
    fingerprint = "fingerprint-#{System.unique_integer([:positive])}"

    try do
      File.mkdir_p!(test_root)
      File.write!(blocking_path, "not a directory")
      settings = put_in(Config.settings!().workspace.root, blocking_path)

      assert :ok = RuntimeCache.record_handoff_fingerprint(nil, issue.id, fingerprint)
      assert RuntimeCache.handoff_fingerprint_seen?(nil, issue.id, fingerprint)

      log =
        capture_log(fn ->
          assert {:error, errors} = Workspace.cleanup_issue_runtime_cache(issue, settings)
          assert {:acp_session_store, {:opencode_acp_session_store_read_failed, :enotdir}} in errors
        end)

      assert log =~ "Issue runtime cache cleanup completed with errors"
      refute RuntimeCache.handoff_fingerprint_seen?(nil, issue.id, fingerprint)
    after
      File.rm_rf(test_root)
    end
  end

  test "runtime handoff cache survives the worker process that records it" do
    issue = %Issue{id: "issue-runtime-cache-worker", identifier: "SYM-RUNTIME-CACHE"}
    fingerprint = "fingerprint-#{System.unique_integer([:positive])}"
    parent = self()

    {worker, ref} =
      spawn_monitor(fn ->
        :ok = RuntimeCache.record_handoff_fingerprint(nil, issue.id, fingerprint)
        send(parent, :runtime_cache_recorded)
      end)

    assert_receive :runtime_cache_recorded
    assert_receive {:DOWN, ^ref, :process, ^worker, :normal}

    assert RuntimeCache.handoff_fingerprint_seen?(nil, issue.id, fingerprint)

    :ok = RuntimeCache.clear_issue(nil, issue)
    refute RuntimeCache.handoff_fingerprint_seen?(nil, issue.id, fingerprint)
  end

  test "runtime cache sweep deletes stale abandoned workspaces and preserves active workspaces" do
    workspace_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-runtime-cache-sweep-#{System.unique_integer([:positive])}"
      )

    try do
      write_workflow_file!(Workflow.workflow_file_path(), workspace_root: workspace_root)

      active_workspace = Path.join(workspace_root, "SYM-ACTIVE")
      abandoned_workspace = Path.join(workspace_root, "SYM-ABANDONED")
      fresh_workspace = Path.join(workspace_root, "SYM-FRESH")

      Enum.each([active_workspace, abandoned_workspace, fresh_workspace], fn path ->
        File.mkdir_p!(path)
        File.write!(Path.join(path, "marker.txt"), Path.basename(path))
      end)

      old_mtime = {{2024, 1, 1}, {0, 0, 0}}
      File.touch!(abandoned_workspace, old_mtime)
      File.touch!(active_workspace, old_mtime)

      assert {:ok, removed} =
               Workspace.sweep_abandoned_runtime_cache(["SYM-ACTIVE"], Config.settings!(), 60_000)

      assert abandoned_workspace in removed
      refute File.exists?(abandoned_workspace)
      assert File.exists?(active_workspace)
      assert File.exists?(fresh_workspace)
    after
      File.rm_rf(workspace_root)
    end
  end

  test "legacy pulse ledger file is deleted during runtime cache cleanup" do
    workspace_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-runtime-cache-ledger-#{System.unique_integer([:positive])}"
      )

    try do
      write_workflow_file!(Workflow.workflow_file_path(), workspace_root: workspace_root)
      File.mkdir_p!(workspace_root)
      ledger_path = Path.join(workspace_root, "pulse_ledger.json")
      File.write!(ledger_path, Jason.encode!(%{"suppression_counts" => %{"stale" => 1}}))

      assert :ok = Workspace.remove_legacy_runtime_cache(Config.settings!())
      refute File.exists?(ledger_path)
    after
      File.rm_rf(workspace_root)
    end
  end

  test "live terminal reconciliation removes workspace, ACP session, and runtime cache" do
    workspace_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-runtime-cache-live-terminal-#{System.unique_integer([:positive])}"
      )

    try do
      write_workflow_file!(Workflow.workflow_file_path(),
        workspace_root: workspace_root,
        tracker_terminal_states: ["Done"]
      )

      issue = %Issue{
        id: "issue-live-terminal-cache",
        identifier: "SYM-LIVE-CACHE",
        state: "Done"
      }

      assert {:ok, workspace} = Workspace.create_for_issue(issue)
      File.write!(Path.join(workspace, "artifact.txt"), "stale")

      project_root = File.cwd!()
      session_scope = ACPSessionStore.prompt_scope("prompt")
      assert :ok = ACPSessionStore.put(issue, project_root, "ses-live-stale", session_scope)
      assert :ok = RuntimeCache.record_handoff_fingerprint(nil, issue.id, "fp-live-stale")

      worker_pid =
        spawn(fn ->
          receive do
            :stop -> :ok
          end
        end)

      worker_ref = Process.monitor(worker_pid)

      state = %Orchestrator.State{
        running: %{
          issue.id => %{
            pid: worker_pid,
            ref: worker_ref,
            identifier: issue.identifier,
            issue: %{issue | state: "In Progress"},
            started_at: DateTime.utc_now()
          }
        },
        claimed: MapSet.new([issue.id]),
        blocked: %{},
        retry_attempts: %{},
        codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0},
        runner_runtime_totals: %{seconds_running: 0}
      }

      new_state = Orchestrator.reconcile_issue_states_for_test([issue], state)

      refute Map.has_key?(new_state.running, issue.id)
      refute MapSet.member?(new_state.claimed, issue.id)
      refute File.exists?(workspace)
      assert {:ok, nil} = ACPSessionStore.fetch(issue, project_root, session_scope)
      refute RuntimeCache.handoff_fingerprint_seen?(nil, issue.id, "fp-live-stale")
    after
      File.rm_rf(workspace_root)
    end
  end

  test "linear issue helpers" do
    issue = %Issue{
      id: "abc",
      labels: ["frontend", "infra"],
      assigned_to_worker: false
    }

    assert Issue.label_names(issue) == ["frontend", "infra"]
    assert issue.labels == ["frontend", "infra"]
    refute issue.assigned_to_worker
  end

  test "linear client normalizes blockers from inverse relations" do
    raw_issue = %{
      "id" => "issue-1",
      "identifier" => "MT-1",
      "title" => "Blocked todo",
      "description" => "Needs dependency",
      "priority" => 2,
      "state" => %{"name" => "Todo"},
      "branchName" => "mt-1",
      "url" => "https://example.org/issues/MT-1",
      "assignee" => %{
        "id" => "user-1"
      },
      "projectMilestone" => %{
        "id" => "milestone-1",
        "name" => "P1 hardening",
        "description" => "Owner-approved phase",
        "status" => "started",
        "targetDate" => "2026-06-15"
      },
      "labels" => %{"nodes" => [%{"name" => "Backend"}]},
      "inverseRelations" => %{
        "nodes" => [
          %{
            "type" => "blocks",
            "issue" => %{
              "id" => "issue-2",
              "identifier" => "MT-2",
              "state" => %{"name" => "In Progress"}
            }
          },
          %{
            "type" => "relatesTo",
            "issue" => %{
              "id" => "issue-3",
              "identifier" => "MT-3",
              "state" => %{"name" => "Done"}
            }
          }
        ]
      },
      "comments" => %{
        "nodes" => [
          %{
            "body" => "first owner note",
            "createdAt" => "2026-01-01T01:00:00Z",
            "user" => %{"name" => "Alex"},
            "parent" => nil
          },
          %{
            "body" => "latest owner answer",
            "createdAt" => "2026-01-03T01:00:00Z",
            "user" => %{"name" => "Owner"},
            "parent" => %{"id" => "comment-question-1"}
          }
        ]
      },
      "createdAt" => "2026-01-01T00:00:00Z",
      "updatedAt" => "2026-01-02T00:00:00Z"
    }

    issue = Client.normalize_issue_for_test(raw_issue, "user-1")

    assert issue.blocked_by == [%{id: "issue-2", identifier: "MT-2", state: "In Progress"}]
    assert issue.labels == ["backend"]
    assert issue.priority == 2
    assert issue.state == "Todo"
    assert issue.assignee_id == "user-1"

    assert issue.project_milestone == %{
             id: "milestone-1",
             name: "P1 hardening",
             description: "Owner-approved phase",
             status: "started",
             target_date: "2026-06-15"
           }

    assert issue.assigned_to_worker
    assert issue.latest_comment_at == ~U[2026-01-03 01:00:00Z]

    assert issue.comments == [
             %{body: "first owner note", created_at: ~U[2026-01-01 01:00:00Z], author: "Alex", parent_id: nil},
             %{
               body: "latest owner answer",
               created_at: ~U[2026-01-03 01:00:00Z],
               author: "Owner",
               parent_id: "comment-question-1"
             }
           ]
  end

  test "linear client marks explicitly unassigned issues as not routed to worker" do
    raw_issue = %{
      "id" => "issue-99",
      "identifier" => "MT-99",
      "title" => "Someone else's task",
      "state" => %{"name" => "Todo"},
      "assignee" => %{
        "id" => "user-2"
      }
    }

    issue = Client.normalize_issue_for_test(raw_issue, "user-1")

    refute issue.assigned_to_worker
  end

  test "linear client pagination merge helper preserves issue ordering" do
    issue_page_1 = [
      %Issue{id: "issue-1", identifier: "MT-1"},
      %Issue{id: "issue-2", identifier: "MT-2"}
    ]

    issue_page_2 = [
      %Issue{id: "issue-3", identifier: "MT-3"}
    ]

    merged = Client.merge_issue_pages_for_test([issue_page_1, issue_page_2])

    assert Enum.map(merged, & &1.identifier) == ["MT-1", "MT-2", "MT-3"]
  end

  test "linear client paginates issue state fetches by id beyond one page" do
    issue_ids = Enum.map(1..55, &"issue-#{&1}")
    first_batch_ids = Enum.take(issue_ids, 50)
    second_batch_ids = Enum.drop(issue_ids, 50)

    raw_issue = fn issue_id ->
      suffix = String.replace_prefix(issue_id, "issue-", "")

      %{
        "id" => issue_id,
        "identifier" => "MT-#{suffix}",
        "title" => "Issue #{suffix}",
        "description" => "Description #{suffix}",
        "state" => %{"name" => "In Progress"},
        "labels" => %{"nodes" => []},
        "inverseRelations" => %{"nodes" => []}
      }
    end

    graphql_fun = fn query, variables ->
      send(self(), {:fetch_issue_states_page, query, variables})

      body = %{
        "data" => %{
          "issues" => %{
            "nodes" => Enum.map(variables.ids, raw_issue)
          }
        }
      }

      {:ok, body}
    end

    assert {:ok, issues} = Client.fetch_issue_states_by_ids_for_test(issue_ids, graphql_fun)

    assert Enum.map(issues, & &1.id) == issue_ids

    assert_receive {:fetch_issue_states_page, query, %{ids: ^first_batch_ids, first: 50, relationFirst: 50}}

    assert query =~ "SymphonyLinearIssuesById"

    assert_receive {:fetch_issue_states_page, ^query, %{ids: ^second_batch_ids, first: 5, relationFirst: 50}}
  end

  test "linear client logs response bodies for non-200 graphql responses" do
    log =
      ExUnit.CaptureLog.capture_log(fn ->
        assert {:error, {:linear_api_status, 400}} =
                 Client.graphql(
                   "query Viewer { viewer { id } }",
                   %{},
                   request_fun: fn _payload, _headers ->
                     {:ok,
                      %{
                        status: 400,
                        body: %{
                          "errors" => [
                            %{
                              "message" => "Variable \"$ids\" got invalid value",
                              "extensions" => %{"code" => "BAD_USER_INPUT"}
                            }
                          ]
                        }
                      }}
                   end
                 )
      end)

    assert log =~ "Linear GraphQL request failed status=400"
    assert log =~ ~s(body=%{"errors" => [%{"extensions" => %{"code" => "BAD_USER_INPUT"})
    assert log =~ "Variable \\\"$ids\\\" got invalid value"
  end

  test "orchestrator sorts dispatch by priority then oldest created_at" do
    issue_same_priority_older = %Issue{
      id: "issue-old-high",
      identifier: "MT-200",
      title: "Old high priority",
      state: "Todo",
      priority: 1,
      created_at: ~U[2026-01-01 00:00:00Z]
    }

    issue_same_priority_newer = %Issue{
      id: "issue-new-high",
      identifier: "MT-201",
      title: "New high priority",
      state: "Todo",
      priority: 1,
      created_at: ~U[2026-01-02 00:00:00Z]
    }

    issue_lower_priority_older = %Issue{
      id: "issue-old-low",
      identifier: "MT-199",
      title: "Old lower priority",
      state: "Todo",
      priority: 2,
      created_at: ~U[2025-12-01 00:00:00Z]
    }

    sorted =
      Orchestrator.sort_issues_for_dispatch_for_test([
        issue_lower_priority_older,
        issue_same_priority_newer,
        issue_same_priority_older
      ])

    assert Enum.map(sorted, & &1.identifier) == ["MT-200", "MT-201", "MT-199"]
  end

  test "owner-input pulse selects the latest unhandled owner question update" do
    older = %Issue{
      id: "owner-old",
      identifier: "MT-300",
      title: "Older owner question",
      state: "Need Owner Input",
      updated_at: ~U[2026-01-01 00:00:00Z],
      latest_comment_at: ~U[2026-01-03 00:00:00Z],
      comments: [%{body: "old reply", created_at: ~U[2026-01-03 00:00:00Z], parent_id: "question-old"}]
    }

    latest = %Issue{
      id: "owner-latest",
      identifier: "MT-301",
      title: "Latest owner answer",
      state: "Need Owner Input",
      updated_at: ~U[2026-01-02 00:00:00Z],
      latest_comment_at: ~U[2026-01-04 00:00:00Z],
      project_milestone: %{
        id: "milestone-1",
        name: "Approved phase",
        description: "phase_state: todo"
      },
      comments: [%{body: "latest reply", created_at: ~U[2026-01-04 00:00:00Z], parent_id: "question-latest"}]
    }

    state = %Orchestrator.State{
      running: %{},
      claimed: MapSet.new(),
      blocked: %{},
      active_project_milestone_id: "milestone-1",
      owner_input_pulsed: MapSet.new(["owner-old:2026-01-03T00:00:00Z"])
    }

    assert %Issue{id: "owner-latest"} =
             Orchestrator.latest_owner_input_issue_for_pulse_for_test([older, latest], state)

    handled_state = %{
      state
      | owner_input_pulsed: MapSet.put(state.owner_input_pulsed, "owner-latest:2026-01-04T00:00:00Z")
    }

    assert Orchestrator.latest_owner_input_issue_for_pulse_for_test(
             [older, latest],
             handled_state
           ) == nil
  end

  test "owner-input pulse ignores agent top-level comments newer than owner replies" do
    issue = %Issue{
      id: "owner-report-after-reply",
      identifier: "NER-26",
      title: "Agent report left for owner review",
      state: "Need Owner Input",
      latest_comment_at: ~U[2026-01-05 00:00:00Z],
      project_milestone: %{
        id: "milestone-1",
        name: "Approved phase",
        description: "phase_state: todo"
      },
      comments: [
        %{body: "owner reply", created_at: ~U[2026-01-04 00:00:00Z], parent_id: "question-1"},
        %{body: "## Benchmark report

Validation results...", created_at: ~U[2026-01-05 00:00:00Z], parent_id: nil}
      ]
    }

    state = %Orchestrator.State{
      running: %{},
      claimed: MapSet.new(),
      blocked: %{},
      active_project_milestone_id: "milestone-1",
      owner_input_pulsed: MapSet.new()
    }

    assert Orchestrator.latest_owner_input_issue_for_pulse_for_test([issue], state) == nil
  end

  test "owner-input pulse accepts top-level owner answers" do
    issue = %Issue{
      id: "owner-top-level-answer",
      identifier: "NER-38",
      title: "Owner review: benchmark runner?",
      state: "Need Owner Input",
      latest_comment_at: ~U[2026-01-04 00:00:00Z],
      project_milestone: %{
        id: "milestone-1",
        name: "Approved phase",
        description: "phase_state: todo"
      },
      comments: [%{body: "yes", created_at: ~U[2026-01-04 00:00:00Z], parent_id: nil}]
    }

    state = %Orchestrator.State{
      running: %{},
      claimed: MapSet.new(),
      blocked: %{},
      active_project_milestone_id: "milestone-1",
      owner_input_pulsed: MapSet.new()
    }

    assert %Issue{id: "owner-top-level-answer"} =
             Orchestrator.latest_owner_input_issue_for_pulse_for_test([issue], state)
  end

  test "owner-input pulse ignores generated top-level reports without owner reply parent" do
    agent_report = %Issue{
      id: "owner-report",
      identifier: "NER-26",
      title: "Agent report left for owner review",
      state: "Need Owner Input",
      latest_comment_at: ~U[2026-01-04 00:00:00Z],
      project_milestone: %{
        id: "milestone-1",
        name: "Approved phase",
        description: "phase_state: todo"
      },
      comments: [%{body: "## Benchmark report\n\nValidation results...", created_at: ~U[2026-01-04 00:00:00Z], parent_id: nil}]
    }

    state = %Orchestrator.State{
      running: %{},
      claimed: MapSet.new(),
      blocked: %{},
      owner_input_pulsed: MapSet.new()
    }

    assert Orchestrator.latest_owner_input_issue_for_pulse_for_test([agent_report], state) == nil
  end

  test "owner-input pulse ignores OpenCode ACP session attachment guards" do
    session_attached = %Issue{
      id: "opencode-attached-session",
      identifier: "MNE-32",
      title: "OpenCode stalled session",
      state: "Need Owner Input",
      latest_comment_at: ~U[2026-06-02 16:24:27Z],
      project_milestone: %{
        id: "milestone-1",
        name: "Approved phase",
        description: "phase_state: todo"
      },
      comments: [
        %{
          body: """
          ## OpenCode Session Attached

          Issue: MNE-32
          Runner: OpenCode ACP
          Status: session attached
          Session ID: `ses_176fbd743ffeY4UlNcPj3AGAD5`
          """,
          created_at: ~U[2026-06-02 16:24:27Z],
          parent_id: nil
        }
      ]
    }

    state = %Orchestrator.State{
      running: %{},
      claimed: MapSet.new(),
      blocked: %{},
      owner_input_pulsed: MapSet.new()
    }

    assert Orchestrator.latest_owner_input_issue_for_pulse_for_test([session_attached], state) == nil
  end

  test "owner-input pulse ignores generated top-level questions without owner reply parent" do
    agent_question = %Issue{
      id: "owner-question",
      identifier: "NER-39",
      title: "Agent question left for owner review",
      state: "Need Owner Input",
      latest_comment_at: ~U[2026-01-04 00:00:00Z],
      project_milestone: %{
        id: "milestone-1",
        name: "Approved phase",
        description: "phase_state: todo"
      },
      comments: [
        %{
          body: "Which benchmark gate should run next after this accepted slice: diagnostic-only, claim-safe, or stop and wait?",
          created_at: ~U[2026-01-04 00:00:00Z],
          parent_id: nil
        }
      ]
    }

    state = %Orchestrator.State{
      running: %{},
      claimed: MapSet.new(),
      blocked: %{},
      owner_input_pulsed: MapSet.new()
    }

    assert Orchestrator.latest_owner_input_issue_for_pulse_for_test([agent_question], state) == nil
  end

  test "owner-input pulse accepts issue without runtime milestone pointer" do
    issue = %Issue{
      id: "owner-unscoped",
      identifier: "NER-30",
      title: "Owner question",
      state: "Need Owner Input",
      latest_comment_at: ~U[2026-01-04 00:00:00Z],
      comments: [%{body: "owner reply", created_at: ~U[2026-01-04 00:00:00Z], parent_id: "question-1"}]
    }

    state = %Orchestrator.State{
      running: %{},
      claimed: MapSet.new(),
      blocked: %{},
      owner_input_pulsed: MapSet.new()
    }

    assert Orchestrator.latest_owner_input_issue_for_pulse_for_test([issue], state) == issue
  end

  test "done continuation pulse ignores issue without todo project milestone" do
    done_issue = %Issue{
      id: "done-without-milestone",
      identifier: "NER-17",
      title: "Closed work",
      state: "Done",
      updated_at: ~U[2026-01-04 00:00:00Z]
    }

    state = %Orchestrator.State{
      running: %{},
      claimed: MapSet.new(),
      blocked: %{},
      continuation_pulsed: MapSet.new()
    }

    assert Orchestrator.latest_done_issue_for_continuation_for_test([done_issue], state) == nil
  end

  test "done continuation pulse ignores issue inside todo project milestone" do
    done_issue = %Issue{
      id: "done-with-todo-milestone",
      identifier: "NER-18",
      title: "Closed work",
      state: "Done",
      updated_at: ~U[2026-01-04 00:00:00Z],
      project_milestone: %{
        id: "milestone-1",
        name: "Approved phase",
        description: "phase_state: todo"
      }
    }

    state = %Orchestrator.State{
      running: %{},
      claimed: MapSet.new(),
      blocked: %{},
      continuation_pulsed: MapSet.new()
    }

    assert Orchestrator.latest_done_issue_for_continuation_for_test([done_issue], state) == nil
  end

  test "owner-input issues do not block idle owner pulse dispatch" do
    owner_input = %Issue{
      id: "owner-input-1",
      identifier: "NER-28",
      title: "Owner answer",
      state: "Need Owner Input"
    }

    normal_work = %Issue{
      id: "normal-work-1",
      identifier: "NER-29",
      title: "Normal work",
      state: "Todo"
    }

    assert Orchestrator.active_issues_blocking_idle_pulse_for_test([owner_input]) == []
    assert Orchestrator.active_issues_blocking_idle_pulse_for_test([owner_input, normal_work]) == [normal_work]
  end

  test "blocked preparing issues do not block idle owner pulse dispatch" do
    write_workflow_file!(Workflow.workflow_file_path(),
      tracker_active_states: ["Todo", "Preparing", "Need Owner Input"],
      tracker_terminal_states: ["Done"],
      max_concurrent_agents: 2,
      stewardship_active_milestone_id: "milestone-1",
      stewardship_active_milestone_name: "Approved phase"
    )

    owner_input = %Issue{
      id: "owner-input-1",
      identifier: "NER-38",
      title: "Owner answer",
      state: "Need Owner Input",
      project_milestone: %{id: "milestone-1", name: "Approved phase"}
    }

    blocked_work = %Issue{
      id: "blocked-work-1",
      identifier: "NER-41",
      title: "Blocked work",
      state: "Preparing",
      project_milestone: %{id: "milestone-1", name: "Approved phase"},
      blocked_by: [%{id: "dependency", state: "Todo"}]
    }

    dispatchable_work = %Issue{
      id: "normal-work-1",
      identifier: "NER-42",
      title: "Normal work",
      state: "Preparing",
      project_milestone: %{id: "milestone-1", name: "Approved phase"}
    }

    state = %Orchestrator.State{
      running: %{},
      claimed: MapSet.new(),
      blocked: %{},
      retry_attempts: %{},
      active_project_milestone_id: "milestone-1",
      max_concurrent_agents: 2
    }

    assert Orchestrator.active_issues_blocking_idle_pulse_for_test([owner_input, blocked_work], state) == []

    assert Orchestrator.active_issues_blocking_idle_pulse_for_test([owner_input, dispatchable_work], state) == [
             dispatchable_work
           ]
  end

  test "runtime cache GC prunes stale pulse and claim entries while preserving visible and active issue ids" do
    state = %Orchestrator.State{
      completed: MapSet.new(["stale-completed", "visible-done"]),
      claimed: MapSet.new(["stale-claimed", "visible-owner", "running-id", "retry-id", "blocked-id"]),
      continuation_pulsed: MapSet.new(["stale-done:2026-01-01T00:00:00Z", "visible-done:2026-01-02T00:00:00Z"]),
      owner_input_pulsed: MapSet.new(["stale-owner:2026-01-01T00:00:00Z", "visible-owner:2026-01-02T00:00:00Z", "running-id:2026-01-02T00:00:00Z"]),
      running: %{"running-id" => %{}},
      retry_attempts: %{"retry-id" => %{}},
      blocked: %{"blocked-id" => %{}}
    }

    visible_issues = [
      %Issue{id: "visible-owner", identifier: "SYM-OWNER", state: "Need Owner Input"},
      %Issue{id: "visible-done", identifier: "SYM-DONE", state: "Done"}
    ]

    pruned = Orchestrator.runtime_cache_gc_for_test(state, visible_issues)

    assert pruned.completed == MapSet.new(["visible-done"])
    assert pruned.claimed == MapSet.new(["visible-owner", "running-id", "retry-id", "blocked-id"])
    assert pruned.continuation_pulsed == MapSet.new(["visible-done:2026-01-02T00:00:00Z"])

    assert pruned.owner_input_pulsed ==
             MapSet.new(["visible-owner:2026-01-02T00:00:00Z", "running-id:2026-01-02T00:00:00Z"])
  end

  test "owner-answer pulse is eligible without workflow-local active milestone pointer" do
    issue = %Issue{
      id: "owner-answer",
      identifier: "MNE-38",
      title: "Owner answered",
      state: "Need Owner Input",
      comments: [
        %{
          body: "retry one more time and wait more than 300000ms",
          created_at: ~U[2026-06-09 08:50:01Z],
          parent_id: "parked-comment"
        }
      ],
      project_milestone: %{id: "milestone-1", name: "Milestone 1"}
    }

    state = %Orchestrator.State{
      active_project_milestone_id: nil,
      owner_input_pulsed: MapSet.new(),
      claimed: MapSet.new(),
      running: %{},
      blocked: %{}
    }

    assert Orchestrator.latest_owner_input_issue_for_pulse_for_test([issue], state) == issue
  end

  test "unchanged owner-input pulse suppresses from runtime cache without spawning a worker" do
    fingerprint = "owner-unchanged:2026-01-04T00:00:00Z"

    issue = %Issue{
      id: "owner-unchanged",
      identifier: "SYM-13",
      title: "Owner parked question",
      state: "Need Owner Input",
      comments: [
        %{body: "continue", created_at: ~U[2026-01-04 00:00:00Z], parent_id: nil}
      ],
      project_milestone: %{
        id: "milestone-owner",
        name: "Owner milestone",
        description: "phase_state: todo"
      }
    }

    state = %Orchestrator.State{
      max_concurrent_agents: 1,
      running: %{},
      claimed: MapSet.new(),
      blocked: %{},
      retry_attempts: %{},
      owner_input_pulsed: MapSet.new([fingerprint]),
      codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0}
    }

    assert {:none, new_state} = Orchestrator.dispatch_owner_input_pulse_candidate_for_test(issue, state)
    assert new_state.running == %{}
    assert new_state.retry_attempts == %{}
    assert %{"owner_wait_no_change" => 1} = new_state.suppression_counts
  end

  test "owner-input dispatch failure does not mark runtime pulse cache" do
    write_workflow_file!(Workflow.workflow_file_path(),
      worker_ssh_hosts: ["worker-a"],
      worker_max_concurrent_agents_per_host: 1
    )

    issue = %Issue{
      id: "owner-dispatch-fails",
      identifier: "SYM-13",
      title: "Owner answered",
      state: "Need Owner Input",
      comments: [
        %{body: "continue", created_at: ~U[2026-01-04 00:00:00Z], parent_id: nil}
      ],
      project_milestone: %{
        id: "milestone-owner",
        name: "Owner milestone",
        description: "phase_state: todo"
      }
    }

    occupied = %Issue{
      id: "occupied",
      identifier: "SYM-0",
      title: "Occupies worker",
      state: "Todo",
      project_milestone: issue.project_milestone
    }

    state = %Orchestrator.State{
      max_concurrent_agents: 2,
      running: %{"occupied" => %{worker_host: "worker-a", issue: occupied}},
      claimed: MapSet.new(),
      blocked: %{},
      retry_attempts: %{},
      owner_input_pulsed: MapSet.new(),
      codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0}
    }

    assert {:none, new_state} = Orchestrator.dispatch_owner_input_pulse_candidate_for_test(issue, state)
    refute Map.has_key?(new_state.running, issue.id)
    refute MapSet.member?(new_state.owner_input_pulsed, "owner-dispatch-fails:2026-01-04T00:00:00Z")
  end

  test "full idle pulse ignores Done continuation entirely" do
    state = %Orchestrator.State{
      max_concurrent_agents: 1,
      running: %{},
      claimed: MapSet.new(),
      blocked: %{},
      retry_attempts: %{},
      owner_input_pulsed: MapSet.new(),
      codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0}
    }

    new_state = Orchestrator.dispatch_idle_pulse_for_test(state, true)

    assert new_state.running == %{}
    assert new_state.suppression_counts == %{}
  end

  test "checked-in Symphony workflow keeps Codex acceptance issue-scoped" do
    workflow_path =
      __DIR__
      |> Path.join("../../WORKFLOW.md")
      |> Path.expand()

    assert {:ok, %{config: config}} = Workflow.load(workflow_path)
    assert {:ok, settings} = Schema.parse(config)

    assert settings.codex.thread_id == nil
    assert settings.codex.project_root == nil
    assert settings.runner.routes["in review"] == "codex"
    assert settings.runner.routes["rca required"] == "codex"
  end

  test "canonical project workflows keep Codex steward sessions issue-scoped" do
    project_workflow_paths = [
      "/home/agent/proj/mnemesh/WORKFLOW.md",
      "/home/agent/proj/nervure/WORKFLOW.md",
      "/home/agent/proj/neryva/WORKFLOW.md",
      "/home/agent/proj/neryva-agent-forge/WORKFLOW.md",
      "/home/agent/proj/symphony/elixir/WORKFLOW.md"
    ]

    existing_paths = Enum.filter(project_workflow_paths, &File.exists?/1)

    assert "/home/agent/proj/mnemesh/WORKFLOW.md" in existing_paths

    for workflow_path <- existing_paths do
      assert {:ok, %{config: config}} = Workflow.load(workflow_path)
      assert {:ok, settings} = Schema.parse(config)

      assert settings.codex.project_root == nil,
             "#{workflow_path} must not pin Codex sessions to the canonical project root"
    end
  end

  test "todo issue with non-terminal blocker is not dispatch-eligible" do
    state = %Orchestrator.State{
      max_concurrent_agents: 3,
      running: %{},
      claimed: MapSet.new(),
      active_project_milestone_id: "milestone-ready-1",
      codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0},
      retry_attempts: %{}
    }

    issue = %Issue{
      id: "blocked-1",
      identifier: "MT-1001",
      title: "Blocked work",
      state: "Todo",
      blocked_by: [%{id: "blocker-1", identifier: "MT-1002", state: "In Progress"}]
    }

    refute Orchestrator.should_dispatch_issue_for_test(issue, state)
  end

  test "non-todo issue with non-terminal blocker is not dispatch-eligible" do
    write_workflow_file!(Workflow.workflow_file_path(),
      tracker_active_states: ["Todo", "In Review"],
      tracker_terminal_states: ["Done"]
    )

    state = %Orchestrator.State{
      max_concurrent_agents: 3,
      running: %{},
      claimed: MapSet.new(),
      active_project_milestone_id: "milestone-ready-1",
      codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0},
      retry_attempts: %{}
    }

    issue = %Issue{
      id: "blocked-review-1",
      identifier: "MT-1008",
      title: "Blocked review",
      state: "In Review",
      project_milestone: %{
        id: "milestone-ready-1",
        name: "Ready milestone",
        description: "phase_state: todo"
      },
      blocked_by: [%{id: "blocker-1", identifier: "MT-1002", state: "In Progress"}]
    }

    assert Orchestrator.should_dispatch_issue_for_test(%{issue | blocked_by: []}, state)
    refute Orchestrator.should_dispatch_issue_for_test(issue, state)
  end

  test "issue assigned to another worker is not dispatch-eligible" do
    write_workflow_file!(Workflow.workflow_file_path(), tracker_assignee: "dev@example.com")

    state = %Orchestrator.State{
      max_concurrent_agents: 3,
      running: %{},
      claimed: MapSet.new(),
      active_project_milestone_id: "milestone-ready-1",
      codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0},
      retry_attempts: %{}
    }

    issue = %Issue{
      id: "assigned-away-1",
      identifier: "MT-1007",
      title: "Owned elsewhere",
      state: "Todo",
      assigned_to_worker: false
    }

    refute Orchestrator.should_dispatch_issue_for_test(issue, state)
  end

  test "preparing issue with terminal blockers remains dispatch-eligible" do
    write_workflow_file!(Workflow.workflow_file_path(),
      tracker_active_states: ["Todo", "Preparing", "In Progress", "Need Owner Input"],
      poll_fast_states: ["Todo", "Preparing"],
      runner_routes: %{"Preparing" => "codex", "In Progress" => "opencode"}
    )

    state = %Orchestrator.State{
      max_concurrent_agents: 3,
      running: %{},
      claimed: MapSet.new(),
      active_project_milestone_id: "milestone-ready-1",
      codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0},
      retry_attempts: %{}
    }

    issue = %Issue{
      id: "ready-1",
      identifier: "MT-1003",
      title: "Ready work",
      state: "Preparing",
      project_milestone: %{
        id: "milestone-ready-1",
        name: "Ready milestone",
        description: "phase_state: todo"
      },
      blocked_by: [%{id: "blocker-2", identifier: "MT-1004", state: "Closed"}]
    }

    assert Orchestrator.should_dispatch_issue_for_test(issue, state)
  end

  test "issue without project milestone is not dispatch-eligible" do
    state = %Orchestrator.State{
      max_concurrent_agents: 3,
      running: %{},
      claimed: MapSet.new(),
      codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0},
      retry_attempts: %{}
    }

    issue = %Issue{
      id: "no-milestone-1",
      identifier: "MT-1009",
      title: "Unscoped work",
      state: "Todo"
    }

    refute Orchestrator.should_dispatch_issue_for_test(issue, state)
  end

  test "issue inside unmarked project milestone is not dispatch-eligible" do
    state = %Orchestrator.State{
      max_concurrent_agents: 3,
      running: %{},
      claimed: MapSet.new(),
      codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0},
      retry_attempts: %{}
    }

    issue = %Issue{
      id: "milestone-unmarked-1",
      identifier: "MT-1010",
      title: "Milestone work",
      state: "Todo",
      project_milestone: %{
        id: "milestone-1",
        name: "Unmarked milestone",
        description: "Product direction draft"
      }
    }

    refute Orchestrator.should_dispatch_issue_for_test(issue, state)
  end

  test "active milestone is selected from explicit active pointer, not description text" do
    write_workflow_file!(Workflow.workflow_file_path(),
      tracker_active_states: ["Todo", "Preparing", "In Progress", "Need Owner Input"],
      poll_fast_states: ["Todo", "Preparing"],
      runner_routes: %{"Preparing" => "codex", "In Progress" => "opencode"}
    )

    state = %Orchestrator.State{
      max_concurrent_agents: 3,
      running: %{},
      claimed: MapSet.new(),
      active_project_milestone_id: "milestone-2",
      codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0},
      retry_attempts: %{}
    }

    issue = %Issue{
      id: "milestone-todo-1",
      identifier: "MT-1011",
      title: "Milestone work",
      state: "Preparing",
      project_milestone: %{
        id: "milestone-2",
        name: "todo milestone",
        description: "phase_state: todo\n\nProduct direction"
      }
    }

    assert Orchestrator.should_dispatch_issue_for_test(issue, state)
  end

  test "todo issues wait in queue until promoted to preparing for Codex stewardship" do
    write_workflow_file!(Workflow.workflow_file_path(),
      tracker_active_states: ["Todo", "Preparing", "In Progress", "Need Owner Input"],
      poll_fast_states: ["Todo", "Preparing"],
      runner_routes: %{"Preparing" => "codex", "In Progress" => "opencode"}
    )

    state = %Orchestrator.State{
      max_concurrent_agents: 3,
      running: %{},
      claimed: MapSet.new(),
      active_project_milestone_id: "milestone-2",
      codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0},
      retry_attempts: %{}
    }

    todo_issue = %Issue{
      id: "milestone-todo-queued",
      identifier: "MT-1012",
      title: "Queued milestone work",
      state: "Todo",
      project_milestone: %{id: "milestone-2", name: "active milestone"}
    }

    preparing_issue = %Issue{
      todo_issue
      | id: "milestone-preparing-running",
        identifier: "MT-1013",
        state: "Preparing"
    }

    refute Orchestrator.should_dispatch_issue_for_test(todo_issue, state)
    assert Orchestrator.should_dispatch_issue_for_test(preparing_issue, state)
  end

  test "poll promotes queued todo issue to preparing without dispatching it in the same cycle" do
    test_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-elixir-preparing-promotion-#{System.unique_integer([:positive])}"
      )

    try do
      write_workflow_file!(Workflow.workflow_file_path(),
        tracker_kind: "memory",
        workspace_root: test_root,
        tracker_active_states: ["Todo", "Preparing", "In Progress", "Need Owner Input"],
        tracker_terminal_states: ["Closed", "Cancelled", "Canceled", "Duplicate"],
        poll_fast_states: ["Todo", "Preparing"],
        runner_default: "codex",
        runner_routes: %{"Preparing" => "codex", "In Progress" => "opencode"},
        stewardship_active_milestone_id: "milestone-2",
        stewardship_active_milestone_name: "active milestone"
      )

      issue = %Issue{
        id: "queued-for-preparing",
        identifier: "MT-1014",
        title: "Queued milestone work",
        state: "Todo",
        project_milestone: %{id: "milestone-2", name: "active milestone"}
      }

      Application.put_env(:symphony_elixir, :memory_tracker_issues, [issue])
      Application.put_env(:symphony_elixir, :memory_tracker_recipient, self())

      initial_state = %Orchestrator.State{
        poll_interval_ms: 30_000,
        full_poll_interval_ms: 60_000,
        last_full_poll_at_ms: System.monotonic_time(:millisecond),
        fast_poll_states: ["Todo", "Preparing"],
        max_concurrent_agents: 1,
        running: %{},
        claimed: MapSet.new(),
        blocked: %{},
        retry_attempts: %{},
        active_project_milestone_id: "milestone-2",
        codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0},
        runner_runtime_totals: %{seconds_running: 0}
      }

      assert {:noreply, state} = Orchestrator.handle_info(:run_poll_cycle, initial_state)

      assert_receive {:memory_tracker_state_update, "queued-for-preparing", "Preparing"}, 50
      assert state.running == %{}
      assert state.claimed == MapSet.new()

      if is_reference(state.tick_timer_ref), do: Process.cancel_timer(state.tick_timer_ref)
    after
      Application.delete_env(:symphony_elixir, :memory_tracker_issues)
      Application.delete_env(:symphony_elixir, :memory_tracker_recipient)
      File.rm_rf(test_root)
    end
  end

  test "poll promotes unblocked todo even when another issue is parked for owner input" do
    test_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-elixir-owner-input-does-not-stop-todo-promotion-#{System.unique_integer([:positive])}"
      )

    try do
      write_workflow_file!(Workflow.workflow_file_path(),
        tracker_kind: "memory",
        workspace_root: test_root,
        tracker_active_states: ["Todo", "Preparing", "In Progress", "Need Owner Input"],
        tracker_terminal_states: ["Done", "Canceled", "Duplicate"],
        poll_fast_states: ["Todo", "Preparing", "Need Owner Input"],
        runner_default: "codex",
        runner_routes: %{"Preparing" => "codex", "In Progress" => "opencode"}
      )

      owner_input = %Issue{
        id: "parked-owner-input",
        identifier: "MT-1014A",
        title: "Parked benchmark review",
        state: "Need Owner Input",
        project_milestone: %{id: "milestone-2", name: "active milestone"}
      }

      todo_issue = %Issue{
        id: "unblocked-todo-after-owner-input",
        identifier: "MT-1014B",
        title: "Queued milestone work",
        state: "Todo",
        project_milestone: %{id: "milestone-2", name: "active milestone"}
      }

      Application.put_env(:symphony_elixir, :memory_tracker_issues, [owner_input, todo_issue])
      Application.put_env(:symphony_elixir, :memory_tracker_recipient, self())

      initial_state = %Orchestrator.State{
        poll_interval_ms: 30_000,
        full_poll_interval_ms: 60_000,
        last_full_poll_at_ms: System.monotonic_time(:millisecond),
        fast_poll_states: ["Todo", "Preparing", "Need Owner Input"],
        max_concurrent_agents: 1,
        running: %{},
        claimed: MapSet.new(),
        blocked: %{},
        retry_attempts: %{},
        active_project_milestone_id: "milestone-2",
        owner_input_pulsed: MapSet.new(["parked-owner-input:2026-01-01T00:00:00Z"]),
        codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0},
        runner_runtime_totals: %{seconds_running: 0}
      }

      assert {:noreply, state} = Orchestrator.handle_info(:run_poll_cycle, initial_state)

      assert_receive {:memory_tracker_state_update, "unblocked-todo-after-owner-input", "Preparing"}, 50
      refute_receive {:memory_tracker_state_update, "parked-owner-input", _state}, 50
      assert state.running == %{}
      assert state.claimed == MapSet.new()

      if is_reference(state.tick_timer_ref), do: Process.cancel_timer(state.tick_timer_ref)
    after
      Application.delete_env(:symphony_elixir, :memory_tracker_issues)
      Application.delete_env(:symphony_elixir, :memory_tracker_recipient)
      File.rm_rf(test_root)
    end
  end

  test "poll selects an active milestone from linear-native todo candidates when no pointer is configured" do
    test_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-elixir-linear-native-milestone-#{System.unique_integer([:positive])}"
      )

    try do
      write_workflow_file!(Workflow.workflow_file_path(),
        tracker_kind: "memory",
        workspace_root: test_root,
        tracker_active_states: ["Todo", "Preparing", "In Progress", "Need Owner Input"],
        tracker_terminal_states: ["Closed", "Cancelled", "Canceled", "Duplicate"],
        poll_fast_states: ["Todo", "Preparing"],
        runner_default: "codex",
        runner_routes: %{"Preparing" => "codex", "In Progress" => "opencode"}
      )

      issue = %Issue{
        id: "linear-native-todo",
        identifier: "MT-1015",
        title: "Queued milestone work",
        state: "Todo",
        project_milestone: %{id: "milestone-native", name: "01 Native milestone"}
      }

      Application.put_env(:symphony_elixir, :memory_tracker_issues, [issue])
      Application.put_env(:symphony_elixir, :memory_tracker_recipient, self())

      initial_state = %Orchestrator.State{
        poll_interval_ms: 30_000,
        full_poll_interval_ms: 60_000,
        last_full_poll_at_ms: System.monotonic_time(:millisecond),
        fast_poll_states: ["Todo", "Preparing"],
        max_concurrent_agents: 1,
        running: %{},
        claimed: MapSet.new(),
        blocked: %{},
        retry_attempts: %{},
        active_project_milestone_id: nil,
        codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0},
        runner_runtime_totals: %{seconds_running: 0}
      }

      assert {:noreply, state} = Orchestrator.handle_info(:run_poll_cycle, initial_state)

      assert_receive {:memory_tracker_state_update, "linear-native-todo", "Preparing"}, 50
      assert state.active_project_milestone_id == "milestone-native"
      assert state.running == %{}

      if is_reference(state.tick_timer_ref), do: Process.cancel_timer(state.tick_timer_ref)
    after
      Application.delete_env(:symphony_elixir, :memory_tracker_issues)
      Application.delete_env(:symphony_elixir, :memory_tracker_recipient)
      File.rm_rf(test_root)
    end
  end

  test "suppression event fingerprints are retained with bounded suppression events" do
    running =
      1..21
      |> Map.new(fn index ->
        issue_id = "suppressed-#{index}"

        {issue_id,
         %{
           identifier: "SYM-#{index}",
           state: "In Progress",
           runner_kind: "opencode",
           runner_owner: "opencode",
           runner_phase: "handoff",
           session_id: "session-#{index}"
         }}
      end)

    state = %Orchestrator.State{
      running: running,
      claimed: MapSet.new(),
      blocked: %{},
      retry_attempts: %{},
      codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0},
      runner_runtime_totals: %{seconds_running: 0}
    }

    update = %{
      event: :handoff_suppressed,
      timestamp: DateTime.utc_now(),
      suppression_kind: "handoff_unchanged",
      reason: "handoff unchanged"
    }

    assert {:noreply, deduped_state} =
             Orchestrator.handle_info({:runner_worker_update, "suppressed-1", update}, state)

    assert {:noreply, deduped_state} =
             Orchestrator.handle_info({:runner_worker_update, "suppressed-1", update}, deduped_state)

    assert length(deduped_state.suppression_events) == 1
    assert MapSet.size(deduped_state.suppression_event_fingerprints) == 1

    bounded_state =
      2..21
      |> Enum.reduce(deduped_state, fn index, acc ->
        assert {:noreply, next_state} =
                 Orchestrator.handle_info({:runner_worker_update, "suppressed-#{index}", update}, acc)

        next_state
      end)

    assert length(bounded_state.suppression_events) == 20
    assert MapSet.size(bounded_state.suppression_event_fingerprints) == 20
    refute Enum.any?(bounded_state.suppression_events, &(&1.issue_id == "suppressed-1"))
  end

  test "milestone description containing phase_state todo has no runtime effect" do
    state = %Orchestrator.State{
      max_concurrent_agents: 3,
      running: %{},
      claimed: MapSet.new(),
      active_project_milestone_id: "milestone-2",
      codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0},
      retry_attempts: %{}
    }

    issue = %Issue{
      id: "milestone-todo-control-1",
      identifier: "MT-1011A",
      title: "Milestone work",
      state: "In Progress",
      project_milestone: %{
        id: "milestone-2",
        name: "todo milestone",
        description:
          "phase_state: todo\n\n" <>
            "launch_gate: change the first line to `phase_state: todo` only after explicit owner decision\n" <>
            "pause_gate: use `phase_state: paused` to keep this milestone dormant\n" <>
            "decision_gate: use `phase_state: needs-decision` when owner review is required\n"
      }
    }

    assert Orchestrator.should_dispatch_issue_for_test(issue, state)
  end

  test "milestone phase marker later in description does not allow dispatch" do
    state = %Orchestrator.State{
      max_concurrent_agents: 3,
      running: %{},
      claimed: MapSet.new(),
      codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0},
      retry_attempts: %{}
    }

    issue = %Issue{
      id: "milestone-later-todo-1",
      identifier: "MT-1011B",
      title: "Milestone work",
      state: "Todo",
      project_milestone: %{
        id: "milestone-later-todo",
        name: "Later todo milestone",
        description: "Product direction\nphase_state: todo"
      }
    }

    refute Orchestrator.should_dispatch_issue_for_test(issue, state)
  end

  test "paused first-line milestone marker with later todo does not allow dispatch" do
    state = %Orchestrator.State{
      max_concurrent_agents: 3,
      running: %{},
      claimed: MapSet.new(),
      codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0},
      retry_attempts: %{}
    }

    issue = %Issue{
      id: "milestone-paused-later-todo-1",
      identifier: "MT-1011C",
      title: "Milestone work",
      state: "Todo",
      project_milestone: %{
        id: "milestone-paused-later-todo",
        name: "Paused milestone",
        description: "phase_state: paused\nphase_state: todo"
      }
    }

    refute Orchestrator.should_dispatch_issue_for_test(issue, state)
  end

  test "needs-decision first-line milestone marker with later todo does not allow dispatch" do
    state = %Orchestrator.State{
      max_concurrent_agents: 3,
      running: %{},
      claimed: MapSet.new(),
      codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0},
      retry_attempts: %{}
    }

    issue = %Issue{
      id: "milestone-needs-later-todo-1",
      identifier: "MT-1011D",
      title: "Milestone work",
      state: "Todo",
      project_milestone: %{
        id: "milestone-needs-later-todo",
        name: "Needs decision milestone",
        description: "phase_state: needs-decision\nphase_state: todo"
      }
    }

    refute Orchestrator.should_dispatch_issue_for_test(issue, state)
  end

  test "missing first-line milestone marker does not allow dispatch even with later guidance" do
    state = %Orchestrator.State{
      max_concurrent_agents: 3,
      running: %{},
      claimed: MapSet.new(),
      codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0},
      retry_attempts: %{}
    }

    issue = %Issue{
      id: "milestone-no-first-line-marker-1",
      identifier: "MT-1011E",
      title: "Milestone work",
      state: "Todo",
      project_milestone: %{
        id: "milestone-no-first-line-marker",
        name: "Draft milestone",
        description: "Product direction\nUse phase_state: todo only after approval"
      }
    }

    refute Orchestrator.should_dispatch_issue_for_test(issue, state)
  end

  test "issue outside active project milestone is not dispatch-eligible" do
    state = %Orchestrator.State{
      max_concurrent_agents: 3,
      running: %{},
      claimed: MapSet.new(),
      active_project_milestone_id: "milestone-active",
      codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0},
      retry_attempts: %{}
    }

    issue = %Issue{
      id: "milestone-other-1",
      identifier: "MT-1013",
      title: "Other milestone work",
      state: "Todo",
      project_milestone: %{
        id: "milestone-other",
        name: "Other milestone",
        description: "phase_state: todo"
      }
    }

    refute Orchestrator.should_dispatch_issue_for_test(issue, state)
  end

  test "issue inside active project milestone remains dispatch-eligible" do
    write_workflow_file!(Workflow.workflow_file_path(),
      tracker_active_states: ["Todo", "Preparing", "In Progress", "Need Owner Input"],
      poll_fast_states: ["Todo", "Preparing"],
      runner_routes: %{"Preparing" => "codex", "In Progress" => "opencode"}
    )

    state = %Orchestrator.State{
      max_concurrent_agents: 3,
      running: %{},
      claimed: MapSet.new(),
      active_project_milestone_id: "milestone-active",
      codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0},
      retry_attempts: %{}
    }

    issue = %Issue{
      id: "milestone-active-1",
      identifier: "MT-1014",
      title: "Active milestone work",
      state: "Preparing",
      project_milestone: %{
        id: "milestone-active",
        name: "Active milestone",
        description: "phase_state: todo"
      }
    }

    assert Orchestrator.should_dispatch_issue_for_test(issue, state)
  end

  test "issue inside paused project milestone is not dispatch-eligible" do
    state = %Orchestrator.State{
      max_concurrent_agents: 3,
      running: %{},
      claimed: MapSet.new(),
      codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0},
      retry_attempts: %{}
    }

    issue = %Issue{
      id: "milestone-paused-1",
      identifier: "MT-1012",
      title: "Milestone work",
      state: "Todo",
      project_milestone: %{
        id: "milestone-3",
        name: "Paused milestone",
        description: "phase_state: paused\n\nProduct direction"
      }
    }

    refute Orchestrator.should_dispatch_issue_for_test(issue, state)
  end

  test "full poll with no eligible active-milestone task does not dispatch planning or worker" do
    previous_milestones = Application.get_env(:symphony_elixir, :memory_tracker_project_milestones)

    test_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-elixir-milestone-planning-dispatch-#{System.unique_integer([:positive])}"
      )

    try do
      write_workflow_file!(Workflow.workflow_file_path(),
        tracker_kind: "memory",
        workspace_root: test_root,
        tracker_active_states: ["Todo", "In Progress", "Need Owner Input"],
        tracker_terminal_states: ["Closed", "Cancelled", "Canceled", "Duplicate"],
        poll_fast_states: ["Todo"],
        runner_default: "opencode",
        runner_routes: %{"In Review" => "codex", "RCA Required" => "codex"}
      )

      Application.put_env(:symphony_elixir, :memory_tracker_issues, [])

      Application.put_env(:symphony_elixir, :memory_tracker_project_milestones, [
        %{
          id: "milestone-planning-1",
          name: "Planning boundary",
          description: "phase_state: todo\n\nDispatch one planning task."
        }
      ])

      initial_state = %Orchestrator.State{
        poll_interval_ms: 30_000,
        full_poll_interval_ms: 60_000,
        last_full_poll_at_ms: nil,
        fast_poll_states: ["Todo"],
        max_concurrent_agents: 1,
        running: %{},
        claimed: MapSet.new(),
        blocked: %{},
        retry_attempts: %{},
        codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0},
        runner_runtime_totals: %{seconds_running: 0}
      }

      assert {:noreply, state} = Orchestrator.handle_info(:run_poll_cycle, initial_state)

      assert state.running == %{}
      assert state.claimed == MapSet.new()
      assert state.active_project_milestone_id == nil

      if is_reference(state.tick_timer_ref), do: Process.cancel_timer(state.tick_timer_ref)
    after
      restore_app_env(:memory_tracker_project_milestones, previous_milestones)
      File.rm_rf(test_root)
    end
  end

  test "legacy synthetic milestone planning paths are not present" do
    orchestrator_source = File.read!(Path.expand("../../lib/symphony_elixir/orchestrator.ex", __DIR__))
    issue_source = File.read!(Path.expand("../../lib/symphony_elixir/linear/issue.ex", __DIR__))

    refute orchestrator_source =~ "MILESTONE-"
    refute orchestrator_source =~ "project_milestone_planning"
    refute orchestrator_source =~ "synthetic_milestone"
    refute orchestrator_source =~ "phase_state"
    refute function_exported?(Orchestrator, :milestone_planning_issues_for_test, 3)
    refute issue_source =~ "synthetic_kind"
  end

  test "active milestone pointer is set only by stewardship config" do
    write_workflow_file!(Workflow.workflow_file_path(),
      stewardship_active_milestone_id: " milestone-configured ",
      stewardship_active_milestone_name: " Configured Milestone "
    )

    assert Config.settings!().stewardship.active_milestone_id == "milestone-configured"
    assert Config.settings!().stewardship.active_milestone_name == "Configured Milestone"

    state = %Orchestrator.State{running: %{}, blocked: %{}, retry_attempts: %{}}

    state = Orchestrator.apply_configured_active_milestone_for_test(state)
    assert state.active_project_milestone_id == "milestone-configured"
  end

  test "milestone dispatch does not require a runtime active pointer" do
    write_workflow_file!(Workflow.workflow_file_path(),
      tracker_active_states: ["Todo", "Preparing", "In Progress", "Need Owner Input"],
      poll_fast_states: ["Todo", "Preparing"],
      runner_routes: %{"Preparing" => "codex", "In Progress" => "opencode"}
    )

    matching_issue = %Issue{
      id: "issue-matching",
      identifier: "SYM-20",
      title: "Matching milestone",
      state: "Preparing",
      project_milestone: %{id: "milestone-active", name: "Active"}
    }

    non_matching_issue = %Issue{
      id: "issue-other",
      identifier: "SYM-21",
      title: "Other milestone",
      state: "Preparing",
      project_milestone: %{id: "milestone-other", name: "Other"}
    }

    state = %Orchestrator.State{active_project_milestone_id: "milestone-active"}

    assert Orchestrator.should_dispatch_issue_for_test(matching_issue, state)
    assert Orchestrator.should_dispatch_issue_for_test(non_matching_issue, state)
    assert Orchestrator.should_dispatch_issue_for_test(matching_issue, %Orchestrator.State{})
  end

  test "retry of active issue hydrates missing milestone pointer from live issue" do
    write_workflow_file!(Workflow.workflow_file_path(),
      tracker_active_states: ["Todo", "Preparing", "In Progress", "Need Owner Input"],
      poll_fast_states: ["Todo", "Preparing"],
      runner_routes: %{"Preparing" => "codex", "In Progress" => "opencode"}
    )

    issue = %Issue{
      id: "issue-retry-active",
      identifier: "MNE-38",
      title: "Active implementation retry",
      state: "In Progress",
      project_milestone: %{id: "milestone-active", name: "01 Active"}
    }

    state = %Orchestrator.State{
      active_project_milestone_id: nil,
      closed_project_milestone_ids: MapSet.new(["milestone-active"]),
      max_concurrent_agents: 6,
      running: %{},
      claimed: MapSet.new(),
      blocked: %{},
      retry_attempts: %{},
      codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0}
    }

    assert {true, hydrated_state} = Orchestrator.retry_dispatchable_issue_for_test(issue, state)
    assert hydrated_state.active_project_milestone_id == "milestone-active"
    refute MapSet.member?(hydrated_state.closed_project_milestone_ids, "milestone-active")
  end

  test "active milestone dispatch allows only one active issue lane" do
    write_workflow_file!(Workflow.workflow_file_path(),
      tracker_active_states: ["Todo", "Preparing", "In Progress", "Need Owner Input"],
      poll_fast_states: ["Todo", "Preparing"],
      runner_routes: %{"Preparing" => "codex", "In Progress" => "opencode"}
    )

    running_issue = %Issue{
      id: "issue-running",
      identifier: "SYM-20",
      title: "Running milestone issue",
      state: "In Progress",
      project_milestone: %{id: "milestone-active", name: "Active"}
    }

    next_issue = %Issue{
      id: "issue-next",
      identifier: "SYM-21",
      title: "Next milestone issue",
      state: "Preparing",
      project_milestone: %{id: "milestone-active", name: "Active"}
    }

    base_state = %Orchestrator.State{
      active_project_milestone_id: "milestone-active",
      max_concurrent_agents: 6,
      running: %{},
      claimed: MapSet.new(),
      blocked: %{},
      retry_attempts: %{},
      codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0}
    }

    assert Orchestrator.should_dispatch_issue_for_test(next_issue, base_state)

    refute Orchestrator.should_dispatch_issue_for_test(next_issue, %{
             base_state
             | running: %{"issue-running" => %{issue: running_issue}},
               claimed: MapSet.new(["issue-running"])
           })

    refute Orchestrator.should_dispatch_issue_for_test(next_issue, %{
             base_state
             | retry_attempts: %{"issue-running" => %{issue: running_issue}}
           })

    refute Orchestrator.should_dispatch_issue_for_test(next_issue, %{
             base_state
             | blocked: %{"issue-running" => %{issue: running_issue}}
           })
  end

  test "configured milestone pointer can be cleared and reselected" do
    write_workflow_file!(Workflow.workflow_file_path(), stewardship_active_milestone_id: "milestone-closed")

    closed_state =
      %Orchestrator.State{running: %{}, blocked: %{}, retry_attempts: %{}}
      |> Orchestrator.apply_configured_active_milestone_for_test()

    assert closed_state.active_project_milestone_id == "milestone-closed"

    write_workflow_file!(Workflow.workflow_file_path(), stewardship_active_milestone_id: "milestone-replacement")
    replacement_state = Orchestrator.apply_configured_active_milestone_for_test(closed_state)
    assert replacement_state.active_project_milestone_id == "milestone-replacement"

    write_workflow_file!(Workflow.workflow_file_path())
    cleared_state = Orchestrator.apply_configured_active_milestone_for_test(replacement_state)
    assert cleared_state.active_project_milestone_id == nil

    write_workflow_file!(Workflow.workflow_file_path(), stewardship_active_milestone_id: "milestone-closed")
    reselected_state = Orchestrator.apply_configured_active_milestone_for_test(cleared_state)
    assert reselected_state.active_project_milestone_id == "milestone-closed"
  end

  test "runtime closed milestone does not reactivate until owner clears or replaces pointer" do
    write_workflow_file!(Workflow.workflow_file_path(), stewardship_active_milestone_id: "milestone-closed")

    closed_state =
      %Orchestrator.State{
        running: %{},
        blocked: %{},
        retry_attempts: %{},
        closed_project_milestone_ids: MapSet.new(["milestone-closed"])
      }
      |> Orchestrator.apply_configured_active_milestone_for_test()

    assert closed_state.active_project_milestone_id == nil

    write_workflow_file!(Workflow.workflow_file_path(), stewardship_active_milestone_id: "milestone-replacement")
    replacement_state = Orchestrator.apply_configured_active_milestone_for_test(closed_state)
    assert replacement_state.active_project_milestone_id == "milestone-replacement"

    write_workflow_file!(Workflow.workflow_file_path())
    cleared_state = Orchestrator.apply_configured_active_milestone_for_test(replacement_state)
    assert cleared_state.active_project_milestone_id == nil
    assert cleared_state.closed_project_milestone_ids == MapSet.new()
  end

  test "non-full poll does not dispatch project milestone planning issue" do
    previous_milestones = Application.get_env(:symphony_elixir, :memory_tracker_project_milestones)

    test_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-elixir-milestone-planning-fast-poll-#{System.unique_integer([:positive])}"
      )

    try do
      write_workflow_file!(Workflow.workflow_file_path(),
        tracker_kind: "memory",
        workspace_root: test_root,
        tracker_active_states: ["Todo", "In Progress", "Need Owner Input"],
        tracker_terminal_states: ["Closed", "Cancelled", "Canceled", "Duplicate"],
        poll_fast_states: ["Todo"],
        runner_default: "opencode",
        runner_routes: %{"In Review" => "codex", "RCA Required" => "codex"}
      )

      Application.put_env(:symphony_elixir, :memory_tracker_issues, [])

      Application.put_env(:symphony_elixir, :memory_tracker_project_milestones, [
        %{
          id: "milestone-fast-poll",
          name: "Fast poll skipped",
          description: "phase_state: todo"
        }
      ])

      initial_state = %Orchestrator.State{
        poll_interval_ms: 30_000,
        full_poll_interval_ms: 60_000,
        last_full_poll_at_ms: System.monotonic_time(:millisecond),
        fast_poll_states: ["Todo"],
        max_concurrent_agents: 1,
        running: %{},
        claimed: MapSet.new(),
        blocked: %{},
        retry_attempts: %{},
        codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0},
        runner_runtime_totals: %{seconds_running: 0}
      }

      assert {:noreply, state} = Orchestrator.handle_info(:run_poll_cycle, initial_state)

      assert state.running == %{}
      assert state.claimed == MapSet.new()
      assert state.active_project_milestone_id == nil

      if is_reference(state.tick_timer_ref), do: Process.cancel_timer(state.tick_timer_ref)
    after
      restore_app_env(:memory_tracker_project_milestones, previous_milestones)
      File.rm_rf(test_root)
    end
  end

  test "generated worker prompts do not contain orchestration role declaration preambles" do
    issue = %Issue{id: "issue-prompt", identifier: "SYM-900", title: "Implement slice", state: "Todo"}
    prompt = PromptBuilder.build_prompt(issue)

    refute prompt =~ "You are the coding orchestrator"
    refute prompt =~ "You are the Machine Architect"
    refute prompt =~ "You are the OpenCode build orchestrator"
    refute prompt =~ ~r/^\s*You are\b/
  end

  test "execution packet records active milestone docs and closure requirements" do
    issue = %Issue{
      id: "issue-packet",
      identifier: "SYM-901",
      title: "Implement steward slice",
      state: "Todo",
      project_milestone: %{id: "milestone-current", name: "Current", description: "phase_state: todo\n\nOwner scope"},
      description: "- acceptance: durable packet before dispatch"
    }

    packet = ExecutionPacket.build(issue)

    assert packet["packet_version"] == "symphony:execution-packet:v1"
    assert packet["active_milestone"] == %{"id" => "milestone-current", "name" => "Current"}
    assert packet["issue"]["identifier"] == "SYM-901"
    assert packet["documentation_requirement"] =~ "docs"
    assert "return exact validation commands and outcomes" in packet["handoff_requirements"]

    assert {:ok, prompt} = ExecutionPacket.prompt(packet)
    refute prompt =~ ~r/^\s*You are\b/
    assert prompt =~ "Use Codex as the only bootstrap runner for Symphony vNext development."
    assert prompt =~ "Todo:"
    assert prompt =~ "Preparing:"
    assert prompt =~ "Do not run Codex stewardship while the issue is still in Todo."
    assert prompt =~ "Do not create or repost an OpenCode implementation prompt."
    assert prompt =~ "Implement the scoped change in Codex."
    assert prompt =~ "In Review:"
    assert prompt =~ "Post one `symphony:review-decision:v1` comment."
    assert prompt =~ "Never start OpenCode ACP for Symphony vNext bootstrap development."
  end

  test "active child work keeps milestone lock without fetching milestone phase or writing suppressions" do
    milestone = %{id: "milestone-current", name: "Current", description: "phase_state: todo"}

    active_issue = %Issue{
      id: "issue-active",
      identifier: "SYM-13",
      title: "Active work",
      state: "Todo",
      project_milestone: milestone
    }

    state = %Orchestrator.State{
      active_project_milestone_id: "milestone-current",
      running: %{},
      blocked: %{},
      retry_attempts: %{}
    }

    new_state = Orchestrator.maybe_release_active_project_milestone_for_test(state, [active_issue])

    assert new_state.active_project_milestone_id == "milestone-current"
    refute_received :milestone_fetch_called
    assert new_state.suppression_counts == %{}
  end

  test "open active milestone without child work stays locked without durable suppression churn" do
    state = %Orchestrator.State{
      active_project_milestone_id: "milestone-current",
      running: %{},
      blocked: %{},
      retry_attempts: %{}
    }

    new_state = Orchestrator.maybe_release_active_project_milestone_for_test(state, [])

    assert new_state.active_project_milestone_id == "milestone-current"
    assert new_state.suppression_counts == %{}
  end

  test "active milestone exhaustion clears runtime closure state" do
    milestone = %{id: "milestone-current", name: "Current", description: "phase_state: todo"}

    terminal_issue = %Issue{
      id: "issue-terminal",
      identifier: "SYM-14",
      title: "Finished work",
      state: "Closed",
      project_milestone: milestone
    }

    state = %Orchestrator.State{
      active_project_milestone_id: "milestone-current",
      running: %{},
      blocked: %{},
      retry_attempts: %{}
    }

    new_state = Orchestrator.maybe_release_active_project_milestone_for_test(state, [terminal_issue])

    assert new_state.active_project_milestone_id == nil
    assert MapSet.member?(new_state.closed_project_milestone_ids, "milestone-current")
  end

  test "active milestone poll closure records runtime closure state" do
    write_workflow_file!(Workflow.workflow_file_path(), tracker_kind: "memory")

    milestone = %{id: "milestone-current", name: "Current", description: "phase_state: todo"}

    terminal_issue = %Issue{
      id: "issue-terminal-fetched",
      identifier: "SYM-16",
      title: "Finished work fetched from tracker",
      state: "Closed",
      project_milestone: milestone
    }

    Application.put_env(:symphony_elixir, :memory_tracker_issues, [terminal_issue])

    state = %Orchestrator.State{
      active_project_milestone_id: "milestone-current",
      running: %{},
      blocked: %{},
      retry_attempts: %{},
      fast_poll_states: ["Need Owner Input"]
    }

    new_state = Orchestrator.maybe_release_active_project_milestone_from_poll_for_test(state, [])

    assert new_state.active_project_milestone_id == nil
    assert MapSet.member?(new_state.closed_project_milestone_ids, "milestone-current")
  end

  test "non-exhausted active milestone with waiting child work does not release" do
    milestone = %{id: "milestone-current", name: "Current", description: "phase_state: todo"}

    waiting_issue = %Issue{
      id: "issue-waiting",
      identifier: "SYM-15",
      title: "Owner review",
      state: "Need Owner Input",
      project_milestone: milestone
    }

    state = %Orchestrator.State{
      active_project_milestone_id: "milestone-current",
      running: %{},
      blocked: %{},
      retry_attempts: %{}
    }

    new_state = Orchestrator.maybe_release_active_project_milestone_for_test(state, [waiting_issue])

    assert new_state.active_project_milestone_id == "milestone-current"
  end

  test "missing Linear issue is not privileged during dispatch revalidation" do
    issue = %Issue{
      id: "issue-missing",
      identifier: "NER-MISSING",
      title: "Missing issue",
      state: "Todo",
      project_milestone: %{id: "milestone-1", name: "Milestone", description: "phase_state: todo"}
    }

    assert {:skip, :missing} =
             Orchestrator.revalidate_issue_for_dispatch_for_test(issue, fn ["issue-missing"] ->
               {:ok, []}
             end)
  end

  test "need owner input issue is parked instead of normal dispatch-eligible" do
    write_workflow_file!(Workflow.workflow_file_path(),
      tracker_active_states: ["Todo", "In Progress", "In Review", "Need Owner Input"]
    )

    state = %Orchestrator.State{
      max_concurrent_agents: 3,
      running: %{},
      claimed: MapSet.new(),
      blocked: %{},
      codex_totals: %{input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0},
      retry_attempts: %{}
    }

    issue = %Issue{
      id: "owner-input-1",
      identifier: "NER-17",
      title: "Owner question",
      state: "Need Owner Input"
    }

    refute Orchestrator.should_dispatch_issue_for_test(issue, state)
  end

  test "dispatch revalidation skips stale todo issue once a non-terminal blocker appears" do
    stale_issue = %Issue{
      id: "blocked-2",
      identifier: "MT-1005",
      title: "Stale blocked work",
      state: "Todo",
      blocked_by: []
    }

    refreshed_issue = %Issue{
      id: "blocked-2",
      identifier: "MT-1005",
      title: "Stale blocked work",
      state: "Todo",
      blocked_by: [%{id: "blocker-3", identifier: "MT-1006", state: "In Progress"}]
    }

    fetcher = fn ["blocked-2"] -> {:ok, [refreshed_issue]} end

    assert {:skip, %Issue{} = skipped_issue} =
             Orchestrator.revalidate_issue_for_dispatch_for_test(stale_issue, fetcher)

    assert skipped_issue.identifier == "MT-1005"

    assert skipped_issue.blocked_by == [
             %{id: "blocker-3", identifier: "MT-1006", state: "In Progress"}
           ]
  end

  test "workspace remove returns error information for missing directory" do
    random_path =
      Path.join(
        System.tmp_dir!(),
        "symphony-elixir-missing-#{System.unique_integer([:positive])}"
      )

    assert {:ok, []} = Workspace.remove(random_path)
  end

  test "workspace hooks support multiline YAML scripts and run at lifecycle boundaries" do
    test_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-elixir-workspace-hooks-#{System.unique_integer([:positive])}"
      )

    try do
      workspace_root = Path.join(test_root, "workspaces")
      before_remove_marker = Path.join(test_root, "before_remove.log")
      after_create_counter = Path.join(test_root, "after_create.count")

      File.mkdir_p!(workspace_root)

      write_workflow_file!(Workflow.workflow_file_path(),
        workspace_root: workspace_root,
        hook_after_create: "echo after_create > after_create.log\necho call >> \"#{after_create_counter}\"",
        hook_before_remove: "echo before_remove > \"#{before_remove_marker}\""
      )

      config = Config.settings!()
      assert config.hooks.after_create =~ "echo after_create > after_create.log"
      assert config.hooks.before_remove =~ "echo before_remove >"

      assert {:ok, workspace} = Workspace.create_for_issue("MT-HOOKS")
      assert File.read!(Path.join(workspace, "after_create.log")) == "after_create\n"

      assert {:ok, _workspace} = Workspace.create_for_issue("MT-HOOKS")
      assert length(String.split(String.trim(File.read!(after_create_counter)), "\n")) == 1

      assert :ok = Workspace.remove_issue_workspaces("MT-HOOKS")
      assert File.read!(before_remove_marker) == "before_remove\n"
      refute File.exists?(workspace)
    after
      File.rm_rf(test_root)
    end
  end

  test "workspace remove continues when before_remove hook fails" do
    test_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-elixir-workspace-hooks-fail-#{System.unique_integer([:positive])}"
      )

    try do
      workspace_root = Path.join(test_root, "workspaces")

      File.mkdir_p!(workspace_root)

      write_workflow_file!(Workflow.workflow_file_path(),
        workspace_root: workspace_root,
        hook_before_remove: "echo failure && exit 17"
      )

      assert {:ok, workspace} = Workspace.create_for_issue("MT-HOOKS-FAIL")
      assert :ok = Workspace.remove_issue_workspaces("MT-HOOKS-FAIL")
      refute File.exists?(workspace)
    after
      File.rm_rf(test_root)
    end
  end

  test "workspace remove continues when before_remove hook fails with large output" do
    test_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-elixir-workspace-hooks-large-fail-#{System.unique_integer([:positive])}"
      )

    try do
      workspace_root = Path.join(test_root, "workspaces")

      File.mkdir_p!(workspace_root)

      write_workflow_file!(Workflow.workflow_file_path(),
        workspace_root: workspace_root,
        hook_before_remove: "i=0; while [ $i -lt 3000 ]; do printf a; i=$((i+1)); done; exit 17"
      )

      assert {:ok, workspace} = Workspace.create_for_issue("MT-HOOKS-LARGE-FAIL")
      assert :ok = Workspace.remove_issue_workspaces("MT-HOOKS-LARGE-FAIL")
      refute File.exists?(workspace)
    after
      File.rm_rf(test_root)
    end
  end

  test "workspace remove continues when before_remove hook times out" do
    previous_timeout = Application.get_env(:symphony_elixir, :workspace_hook_timeout_ms)

    on_exit(fn ->
      if is_nil(previous_timeout) do
        Application.delete_env(:symphony_elixir, :workspace_hook_timeout_ms)
      else
        Application.put_env(:symphony_elixir, :workspace_hook_timeout_ms, previous_timeout)
      end
    end)

    Application.put_env(:symphony_elixir, :workspace_hook_timeout_ms, 10)

    test_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-elixir-workspace-hooks-timeout-#{System.unique_integer([:positive])}"
      )

    try do
      workspace_root = Path.join(test_root, "workspaces")

      File.mkdir_p!(workspace_root)

      write_workflow_file!(Workflow.workflow_file_path(),
        workspace_root: workspace_root,
        hook_before_remove: "sleep 1"
      )

      assert {:ok, workspace} = Workspace.create_for_issue("MT-HOOKS-TIMEOUT")
      assert :ok = Workspace.remove_issue_workspaces("MT-HOOKS-TIMEOUT")
      refute File.exists?(workspace)
    after
      File.rm_rf(test_root)
    end
  end

  test "config reads defaults for optional settings" do
    previous_linear_api_key = System.get_env("LINEAR_API_KEY")
    on_exit(fn -> restore_env("LINEAR_API_KEY", previous_linear_api_key) end)
    System.delete_env("LINEAR_API_KEY")

    write_workflow_file!(Workflow.workflow_file_path(),
      workspace_root: nil,
      max_concurrent_agents: nil,
      codex_approval_policy: nil,
      codex_thread_sandbox: nil,
      codex_turn_sandbox_policy: nil,
      codex_turn_timeout_ms: nil,
      codex_read_timeout_ms: nil,
      codex_stall_timeout_ms: nil,
      tracker_api_token: nil,
      tracker_project_slug: nil
    )

    config = Config.settings!()
    assert config.tracker.endpoint == "https://api.linear.app/graphql"
    assert config.tracker.active_states == ["Todo", "Preparing", "In Progress"]
    assert config.tracker.api_key == nil
    assert config.tracker.project_slug == nil
    assert config.polling.fast_states == ["Todo", "Preparing", "Need Owner Input"]
    assert config.workspace.root == Path.join(System.tmp_dir!(), "symphony_workspaces")
    assert config.worker.max_concurrent_agents_per_host == nil
    assert config.agent.max_concurrent_agents == 10
    assert config.codex.command == "codex app-server"

    assert config.codex.approval_policy == %{
             "reject" => %{
               "sandbox_approval" => true,
               "rules" => true,
               "mcp_elicitations" => true
             }
           }

    assert config.codex.thread_sandbox == "workspace-write"

    assert {:ok, canonical_default_workspace_root} =
             SymphonyElixir.PathSafety.canonicalize(Path.join(System.tmp_dir!(), "symphony_workspaces"))

    assert Config.codex_turn_sandbox_policy() == %{
             "type" => "workspaceWrite",
             "writableRoots" => [canonical_default_workspace_root],
             "readOnlyAccess" => %{"type" => "fullAccess"},
             "networkAccess" => false,
             "excludeTmpdirEnvVar" => false,
             "excludeSlashTmp" => false
           }

    assert config.codex.turn_timeout_ms == 3_600_000
    assert config.codex.read_timeout_ms == 5_000
    assert config.codex.stall_timeout_ms == 300_000

    write_workflow_file!(Workflow.workflow_file_path(),
      codex_command: "codex --config 'model=\"gpt-5.5\"' app-server"
    )

    assert Config.settings!().codex.command ==
             "codex --config 'model=\"gpt-5.5\"' app-server"

    explicit_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-elixir-explicit-sandbox-root-#{System.unique_integer([:positive])}"
      )

    explicit_workspace = Path.join(explicit_root, "MT-EXPLICIT")
    explicit_cache = Path.join(explicit_workspace, "cache")
    File.mkdir_p!(explicit_cache)

    on_exit(fn -> File.rm_rf(explicit_root) end)

    write_workflow_file!(Workflow.workflow_file_path(),
      workspace_root: explicit_root,
      codex_approval_policy: "on-request",
      codex_thread_sandbox: "workspace-write",
      codex_turn_sandbox_policy: %{
        type: "workspaceWrite",
        writableRoots: [explicit_workspace, explicit_cache]
      }
    )

    config = Config.settings!()
    assert config.codex.approval_policy == "on-request"
    assert config.codex.thread_sandbox == "workspace-write"

    assert Config.codex_turn_sandbox_policy(explicit_workspace) == %{
             "type" => "workspaceWrite",
             "writableRoots" => [explicit_workspace, explicit_cache]
           }

    write_workflow_file!(Workflow.workflow_file_path(), tracker_active_states: ",")
    assert {:error, {:invalid_workflow_config, message}} = Config.validate!()
    assert message =~ "tracker.active_states"

    write_workflow_file!(Workflow.workflow_file_path(), max_concurrent_agents: "bad")
    assert {:error, {:invalid_workflow_config, message}} = Config.validate!()
    assert message =~ "agent.max_concurrent_agents"

    write_workflow_file!(Workflow.workflow_file_path(), worker_max_concurrent_agents_per_host: 0)
    assert {:error, {:invalid_workflow_config, message}} = Config.validate!()
    assert message =~ "worker.max_concurrent_agents_per_host"

    write_workflow_file!(Workflow.workflow_file_path(), codex_turn_timeout_ms: "bad")
    assert {:error, {:invalid_workflow_config, message}} = Config.validate!()
    assert message =~ "codex.turn_timeout_ms"

    write_workflow_file!(Workflow.workflow_file_path(), codex_read_timeout_ms: "bad")
    assert {:error, {:invalid_workflow_config, message}} = Config.validate!()
    assert message =~ "codex.read_timeout_ms"

    write_workflow_file!(Workflow.workflow_file_path(), codex_stall_timeout_ms: "bad")
    assert {:error, {:invalid_workflow_config, message}} = Config.validate!()
    assert message =~ "codex.stall_timeout_ms"

    write_workflow_file!(Workflow.workflow_file_path(),
      runner_routes: %{"RCA Required" => "opencode"},
      process_policy_rca_required_state: "RCA Required"
    )

    assert {:error, {:invalid_workflow_config, message}} = Config.settings()
    assert message =~ "process_policy.rca_required_state"
    assert message =~ "must route to codex"

    write_workflow_file!(Workflow.workflow_file_path(),
      runner_routes: %{"In Review" => "opencode", "RCA Required" => "codex"},
      opencode_result_state: "In Review",
      process_policy_rca_required_state: "RCA Required"
    )

    assert {:error, {:invalid_workflow_config, message}} = Config.settings()
    assert message =~ "opencode.result_state"
    assert message =~ "must route to codex"

    write_workflow_file!(Workflow.workflow_file_path(),
      tracker_active_states: %{todo: true},
      tracker_terminal_states: %{done: true},
      poll_interval_ms: %{bad: true},
      workspace_root: 123,
      max_retry_backoff_ms: 0,
      max_concurrent_agents_by_state: %{"Todo" => "1", "Review" => 0, "Done" => "bad"},
      hook_timeout_ms: 0,
      observability_enabled: "maybe",
      observability_refresh_ms: %{bad: true},
      observability_render_interval_ms: %{bad: true},
      server_port: -1,
      server_host: 123
    )

    assert {:error, {:invalid_workflow_config, _message}} = Config.validate!()

    write_workflow_file!(Workflow.workflow_file_path(), codex_approval_policy: "")
    assert :ok = Config.validate!()
    assert Config.settings!().codex.approval_policy == ""

    write_workflow_file!(Workflow.workflow_file_path(), codex_thread_sandbox: "")
    assert :ok = Config.validate!()
    assert Config.settings!().codex.thread_sandbox == ""

    write_workflow_file!(Workflow.workflow_file_path(), codex_turn_sandbox_policy: "bad")
    assert {:error, {:invalid_workflow_config, message}} = Config.validate!()
    assert message =~ "codex.turn_sandbox_policy"

    write_workflow_file!(Workflow.workflow_file_path(),
      codex_approval_policy: "future-policy",
      codex_thread_sandbox: "future-sandbox",
      codex_turn_sandbox_policy: %{
        type: "futureSandbox",
        nested: %{flag: true}
      }
    )

    config = Config.settings!()
    assert config.codex.approval_policy == "future-policy"
    assert config.codex.thread_sandbox == "future-sandbox"

    assert :ok = Config.validate!()

    assert Config.codex_turn_sandbox_policy() == %{
             "type" => "futureSandbox",
             "nested" => %{"flag" => true}
           }

    write_workflow_file!(Workflow.workflow_file_path(), codex_command: "codex app-server")
    assert Config.settings!().codex.command == "codex app-server"
  end

  test "opencode acp disables idle stall watchdog when stall timeout is omitted" do
    File.write!(
      Workflow.workflow_file_path(),
      """
      ---
      opencode:
        protocol: acp
        command: opencode
        timeout_ms: 10800000
      ---

      prompt
      """
    )

    config = Config.settings!()
    assert config.opencode.timeout_ms == 10_800_000
    assert config.opencode.read_timeout_ms == 120_000
    assert config.opencode.stall_timeout_ms == 0
  end

  test "opencode acp honors explicit positive stall timeout" do
    File.write!(
      Workflow.workflow_file_path(),
      """
      ---
      opencode:
        protocol: acp
        command: opencode
        timeout_ms: 10800000
        stall_timeout_ms: 300000
      ---

      prompt
      """
    )

    config = Config.settings!()
    assert config.opencode.timeout_ms == 10_800_000
    assert config.opencode.read_timeout_ms == 120_000
    assert config.opencode.stall_timeout_ms == 300_000
  end

  test "opencode acp honors explicit zero stall timeout" do
    File.write!(
      Workflow.workflow_file_path(),
      """
      ---
      opencode:
        protocol: acp
        command: opencode
        timeout_ms: 10800000
        stall_timeout_ms: 0
      ---

      prompt
      """
    )

    config = Config.settings!()
    assert config.opencode.timeout_ms == 10_800_000
    assert config.opencode.read_timeout_ms == 120_000
    assert config.opencode.stall_timeout_ms == 0
  end

  test "config resolves optional project roots from missing and blank environment tokens" do
    missing_env = "SYMPHONY_TEST_MISSING_ROOT"
    blank_env = "SYMPHONY_TEST_BLANK_ROOT"
    previous_missing = System.get_env(missing_env)
    previous_blank = System.get_env(blank_env)

    on_exit(fn ->
      restore_env(missing_env, previous_missing)
      restore_env(blank_env, previous_blank)
    end)

    System.delete_env(missing_env)
    System.put_env(blank_env, "")

    assert %{} = Schema.normalize_runner_routes(nil)

    write_workflow_file!(Workflow.workflow_file_path(),
      codex_project_root: "$#{missing_env}",
      opencode_project_root: "$#{blank_env}"
    )

    config = Config.settings!()
    assert config.codex.project_root == nil
    assert config.opencode.project_root == nil
  end

  test "config resolves $VAR references for env-backed secret and path values" do
    workspace_env_var = "SYMP_WORKSPACE_ROOT_#{System.unique_integer([:positive])}"
    api_key_env_var = "SYMP_LINEAR_API_KEY_#{System.unique_integer([:positive])}"
    workspace_root = Path.join("/tmp", "symphony-workspace-root")
    api_key = "resolved-secret"
    codex_bin = Path.join(["~", "bin", "codex"])

    previous_workspace_root = System.get_env(workspace_env_var)
    previous_api_key = System.get_env(api_key_env_var)

    System.put_env(workspace_env_var, workspace_root)
    System.put_env(api_key_env_var, api_key)

    on_exit(fn ->
      restore_env(workspace_env_var, previous_workspace_root)
      restore_env(api_key_env_var, previous_api_key)
    end)

    write_workflow_file!(Workflow.workflow_file_path(),
      tracker_api_token: "$#{api_key_env_var}",
      workspace_root: "$#{workspace_env_var}",
      codex_command: "#{codex_bin} app-server"
    )

    config = Config.settings!()
    assert config.tracker.api_key == api_key
    assert config.workspace.root == Path.expand(workspace_root)
    assert config.codex.command == "#{codex_bin} app-server"
  end

  test "config no longer resolves legacy env: references" do
    workspace_env_var = "SYMP_WORKSPACE_ROOT_#{System.unique_integer([:positive])}"
    api_key_env_var = "SYMP_LINEAR_API_KEY_#{System.unique_integer([:positive])}"
    workspace_root = Path.join("/tmp", "symphony-workspace-root")
    api_key = "resolved-secret"

    previous_workspace_root = System.get_env(workspace_env_var)
    previous_api_key = System.get_env(api_key_env_var)

    System.put_env(workspace_env_var, workspace_root)
    System.put_env(api_key_env_var, api_key)

    on_exit(fn ->
      restore_env(workspace_env_var, previous_workspace_root)
      restore_env(api_key_env_var, previous_api_key)
    end)

    write_workflow_file!(Workflow.workflow_file_path(),
      tracker_api_token: "env:#{api_key_env_var}",
      workspace_root: "env:#{workspace_env_var}"
    )

    config = Config.settings!()
    assert config.tracker.api_key == "env:#{api_key_env_var}"
    assert config.workspace.root == "env:#{workspace_env_var}"
  end

  test "config supports per-state max concurrent agent overrides" do
    workflow = """
    ---
    agent:
      max_concurrent_agents: 10
      max_concurrent_agents_by_state:
        todo: 1
        "In Progress": 4
        "In Review": 2
    ---
    """

    File.write!(Workflow.workflow_file_path(), workflow)

    assert Config.settings!().agent.max_concurrent_agents == 10
    assert Config.max_concurrent_agents_for_state("Todo") == 1
    assert Config.max_concurrent_agents_for_state("In Progress") == 4
    assert Config.max_concurrent_agents_for_state("In Review") == 2
    assert Config.max_concurrent_agents_for_state("Closed") == 10
    assert Config.max_concurrent_agents_for_state(:not_a_string) == 10

    write_workflow_file!(Workflow.workflow_file_path(), worker_max_concurrent_agents_per_host: 2)
    assert :ok = Config.validate!()
    assert Config.settings!().worker.max_concurrent_agents_per_host == 2
  end

  test "schema helpers cover custom type and state limit validation" do
    assert StringOrMap.type() == :map
    assert StringOrMap.embed_as(:json) == :self
    assert StringOrMap.equal?(%{"a" => 1}, %{"a" => 1})
    refute StringOrMap.equal?(%{"a" => 1}, %{"a" => 2})

    assert {:ok, "value"} = StringOrMap.cast("value")
    assert {:ok, %{"a" => 1}} = StringOrMap.cast(%{"a" => 1})
    assert :error = StringOrMap.cast(123)

    assert {:ok, "value"} = StringOrMap.load("value")
    assert :error = StringOrMap.load(123)

    assert {:ok, %{"a" => 1}} = StringOrMap.dump(%{"a" => 1})
    assert :error = StringOrMap.dump(123)

    assert Schema.normalize_state_limits(nil) == %{}

    assert Schema.normalize_state_limits(%{"In Progress" => 2, todo: 1}) == %{
             "todo" => 1,
             "in progress" => 2
           }

    changeset =
      {%{}, %{limits: :map}}
      |> Changeset.cast(%{limits: %{"" => 1, "todo" => 0}}, [:limits])
      |> Schema.validate_state_limits(:limits)

    assert changeset.errors == [
             limits: {"state names must not be blank", []},
             limits: {"limits must be positive integers", []}
           ]
  end

  test "schema parse normalizes policy keys and env-backed fallbacks" do
    missing_workspace_env = "SYMP_MISSING_WORKSPACE_#{System.unique_integer([:positive])}"
    empty_secret_env = "SYMP_EMPTY_SECRET_#{System.unique_integer([:positive])}"
    missing_secret_env = "SYMP_MISSING_SECRET_#{System.unique_integer([:positive])}"

    previous_missing_workspace_env = System.get_env(missing_workspace_env)
    previous_empty_secret_env = System.get_env(empty_secret_env)
    previous_missing_secret_env = System.get_env(missing_secret_env)
    previous_linear_api_key = System.get_env("LINEAR_API_KEY")

    System.delete_env(missing_workspace_env)
    System.put_env(empty_secret_env, "")
    System.delete_env(missing_secret_env)
    System.put_env("LINEAR_API_KEY", "fallback-linear-token")

    on_exit(fn ->
      restore_env(missing_workspace_env, previous_missing_workspace_env)
      restore_env(empty_secret_env, previous_empty_secret_env)
      restore_env(missing_secret_env, previous_missing_secret_env)
      restore_env("LINEAR_API_KEY", previous_linear_api_key)
    end)

    assert {:ok, settings} =
             Schema.parse(%{
               tracker: %{api_key: "$#{empty_secret_env}"},
               workspace: %{root: "$#{missing_workspace_env}"},
               codex: %{approval_policy: %{reject: %{sandbox_approval: true}}}
             })

    assert settings.tracker.api_key == nil
    assert settings.workspace.root == Path.join(System.tmp_dir!(), "symphony_workspaces")

    assert settings.codex.approval_policy == %{
             "reject" => %{"sandbox_approval" => true}
           }

    assert {:ok, settings} =
             Schema.parse(%{
               tracker: %{api_key: "$#{missing_secret_env}"},
               workspace: %{root: ""}
             })

    assert settings.tracker.api_key == "fallback-linear-token"
    assert settings.workspace.root == Path.join(System.tmp_dir!(), "symphony_workspaces")
  end

  test "schema resolves sandbox policies from explicit and default workspaces" do
    explicit_policy = %{"type" => "workspaceWrite", "writableRoots" => ["/tmp/explicit"]}

    assert Schema.resolve_turn_sandbox_policy(%Schema{
             codex: %Codex{turn_sandbox_policy: explicit_policy},
             workspace: %Schema.Workspace{root: "/tmp/ignored"}
           }) == explicit_policy

    assert Schema.resolve_turn_sandbox_policy(%Schema{
             codex: %Codex{turn_sandbox_policy: nil},
             workspace: %Schema.Workspace{root: ""}
           }) == %{
             "type" => "workspaceWrite",
             "writableRoots" => [Path.expand(Path.join(System.tmp_dir!(), "symphony_workspaces"))],
             "readOnlyAccess" => %{"type" => "fullAccess"},
             "networkAccess" => false,
             "excludeTmpdirEnvVar" => false,
             "excludeSlashTmp" => false
           }

    assert Schema.resolve_turn_sandbox_policy(
             %Schema{
               codex: %Codex{turn_sandbox_policy: nil},
               workspace: %Schema.Workspace{root: "/tmp/ignored"}
             },
             "/tmp/workspace"
           ) == %{
             "type" => "workspaceWrite",
             "writableRoots" => [Path.expand("/tmp/workspace")],
             "readOnlyAccess" => %{"type" => "fullAccess"},
             "networkAccess" => false,
             "excludeTmpdirEnvVar" => false,
             "excludeSlashTmp" => false
           }
  end

  test "schema keeps workspace roots raw while sandbox helpers expand only for local use" do
    assert {:ok, settings} =
             Schema.parse(%{
               workspace: %{root: "~/.symphony-workspaces"},
               codex: %{}
             })

    assert settings.workspace.root == "~/.symphony-workspaces"

    assert Schema.resolve_turn_sandbox_policy(settings) == %{
             "type" => "workspaceWrite",
             "writableRoots" => [Path.expand("~/.symphony-workspaces")],
             "readOnlyAccess" => %{"type" => "fullAccess"},
             "networkAccess" => false,
             "excludeTmpdirEnvVar" => false,
             "excludeSlashTmp" => false
           }

    assert {:ok, remote_policy} =
             Schema.resolve_runtime_turn_sandbox_policy(settings, nil, remote: true)

    assert remote_policy == %{
             "type" => "workspaceWrite",
             "writableRoots" => ["~/.symphony-workspaces"],
             "readOnlyAccess" => %{"type" => "fullAccess"},
             "networkAccess" => false,
             "excludeTmpdirEnvVar" => false,
             "excludeSlashTmp" => false
           }
  end

  test "runtime sandbox policy resolution passes explicit policies through unchanged" do
    test_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-elixir-runtime-sandbox-#{System.unique_integer([:positive])}"
      )

    try do
      workspace_root = Path.join(test_root, "workspaces")
      issue_workspace = Path.join(workspace_root, "MT-100")
      File.mkdir_p!(issue_workspace)

      write_workflow_file!(Workflow.workflow_file_path(),
        workspace_root: workspace_root,
        codex_turn_sandbox_policy: %{
          type: "workspaceWrite",
          writableRoots: ["relative/path"],
          networkAccess: true
        }
      )

      assert {:ok, runtime_settings} = Config.codex_runtime_settings(issue_workspace)

      assert runtime_settings.turn_sandbox_policy == %{
               "type" => "workspaceWrite",
               "writableRoots" => ["relative/path"],
               "networkAccess" => true
             }

      write_workflow_file!(Workflow.workflow_file_path(),
        workspace_root: workspace_root,
        codex_turn_sandbox_policy: %{
          type: "futureSandbox",
          nested: %{flag: true}
        }
      )

      assert {:ok, runtime_settings} = Config.codex_runtime_settings(issue_workspace)

      assert runtime_settings.turn_sandbox_policy == %{
               "type" => "futureSandbox",
               "nested" => %{"flag" => true}
             }
    after
      File.rm_rf(test_root)
    end
  end

  test "path safety returns errors for invalid path segments" do
    invalid_segment = String.duplicate("a", 300)
    path = Path.join(System.tmp_dir!(), invalid_segment)
    expanded_path = Path.expand(path)

    assert {:error, {:path_canonicalize_failed, ^expanded_path, :enametoolong}} =
             SymphonyElixir.PathSafety.canonicalize(path)
  end

  test "runtime sandbox policy resolution defaults when omitted and ignores workspace for explicit policies" do
    test_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-elixir-runtime-sandbox-branches-#{System.unique_integer([:positive])}"
      )

    try do
      workspace_root = Path.join(test_root, "workspaces")
      issue_workspace = Path.join(workspace_root, "MT-101")

      File.mkdir_p!(issue_workspace)

      write_workflow_file!(Workflow.workflow_file_path(), workspace_root: workspace_root)

      settings = Config.settings!()

      assert {:ok, canonical_workspace_root} =
               SymphonyElixir.PathSafety.canonicalize(workspace_root)

      assert {:ok, default_policy} = Schema.resolve_runtime_turn_sandbox_policy(settings)
      assert default_policy["type"] == "workspaceWrite"
      assert default_policy["writableRoots"] == [canonical_workspace_root]

      assert {:ok, blank_workspace_policy} =
               Schema.resolve_runtime_turn_sandbox_policy(settings, "")

      assert blank_workspace_policy == default_policy

      read_only_settings = %{
        settings
        | codex: %{
            settings.codex
            | turn_sandbox_policy: %{"type" => "readOnly", "networkAccess" => true}
          }
      }

      assert {:ok, %{"type" => "readOnly", "networkAccess" => true}} =
               Schema.resolve_runtime_turn_sandbox_policy(read_only_settings, 123)

      future_settings = %{
        settings
        | codex: %{
            settings.codex
            | turn_sandbox_policy: %{"type" => "futureSandbox", "nested" => %{"flag" => true}}
          }
      }

      assert {:ok, %{"type" => "futureSandbox", "nested" => %{"flag" => true}}} =
               Schema.resolve_runtime_turn_sandbox_policy(future_settings, 123)

      assert {:error, {:unsafe_turn_sandbox_policy, {:invalid_workspace_root, 123}}} =
               Schema.resolve_runtime_turn_sandbox_policy(settings, 123)
    after
      File.rm_rf(test_root)
    end
  end

  test "workflow prompt is used when building base prompt" do
    workflow_prompt = "Workflow prompt body used as codex instruction."

    write_workflow_file!(Workflow.workflow_file_path(), prompt: workflow_prompt)
    assert Config.workflow_prompt() == workflow_prompt
  end

  test "remote workspace lifecycle uses ssh host aliases from worker config" do
    test_root =
      Path.join(
        System.tmp_dir!(),
        "symphony-elixir-remote-workspace-#{System.unique_integer([:positive])}"
      )

    previous_path = System.get_env("PATH")
    previous_trace = System.get_env("SYMP_TEST_SSH_TRACE")

    on_exit(fn ->
      restore_env("PATH", previous_path)
      restore_env("SYMP_TEST_SSH_TRACE", previous_trace)
    end)

    try do
      trace_file = Path.join(test_root, "ssh.trace")
      fake_ssh = Path.join(test_root, "ssh")
      workspace_root = "~/.symphony-remote-workspaces"
      workspace_path = "/remote/home/.symphony-remote-workspaces/MT-SSH-WS"

      File.mkdir_p!(test_root)
      System.put_env("SYMP_TEST_SSH_TRACE", trace_file)
      System.put_env("PATH", test_root <> ":" <> (previous_path || ""))

      File.write!(fake_ssh, """
      #!/bin/sh
      trace_file="${SYMP_TEST_SSH_TRACE:-/tmp/symphony-fake-ssh.trace}"
      printf 'ARGV:%s\\n' "$*" >> "$trace_file"

      case "$*" in
        *"__SYMPHONY_WORKSPACE__"*)
          printf '%s\\t%s\\t%s\\n' '__SYMPHONY_WORKSPACE__' '1' '#{workspace_path}'
          ;;
      esac

      exit 0
      """)

      File.chmod!(fake_ssh, 0o755)

      write_workflow_file!(Workflow.workflow_file_path(),
        workspace_root: workspace_root,
        worker_ssh_hosts: ["worker-01:2200"],
        hook_before_run: "echo before-run",
        hook_after_run: "echo after-run",
        hook_before_remove: "echo before-remove"
      )

      assert Config.settings!().worker.ssh_hosts == ["worker-01:2200"]
      assert Config.settings!().workspace.root == workspace_root
      assert {:ok, ^workspace_path} = Workspace.create_for_issue("MT-SSH-WS", "worker-01:2200")
      assert :ok = Workspace.run_before_run_hook(workspace_path, "MT-SSH-WS", "worker-01:2200")
      assert :ok = Workspace.run_after_run_hook(workspace_path, "MT-SSH-WS", "worker-01:2200")
      assert :ok = Workspace.remove_issue_workspaces("MT-SSH-WS", "worker-01:2200")

      trace = File.read!(trace_file)
      assert trace =~ "-p 2200 worker-01 bash -lc"
      assert trace =~ "__SYMPHONY_WORKSPACE__"
      assert trace =~ "~/.symphony-remote-workspaces/MT-SSH-WS"
      assert trace =~ "${workspace#~/}"
      assert trace =~ "echo before-run"
      assert trace =~ "echo after-run"
      assert trace =~ "echo before-remove"
      assert trace =~ "rm -rf"
      assert trace =~ workspace_path
    after
      File.rm_rf(test_root)
    end
  end

  defp restore_app_env(key, nil), do: Application.delete_env(:symphony_elixir, key)
  defp restore_app_env(key, value), do: Application.put_env(:symphony_elixir, key, value)
end
