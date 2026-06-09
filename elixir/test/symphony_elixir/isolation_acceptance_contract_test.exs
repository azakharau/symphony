defmodule SymphonyElixir.IsolationAcceptanceContractTest do
  use SymphonyElixir.TestSupport

  alias SymphonyElixir.OpenCode.ACPSessionStore
  alias SymphonyElixirWeb.Presenter

  @terminal_columns 115

  @acceptance_matrix [
    %{
      row: :tracker,
      tests: [
        {"elixir/test/symphony_elixir/isolation_tracker_comments_review_test.exs", "fetch_project_milestones returns per-project milestones isolated between projects"}
      ],
      apis: [{Tracker, :fetch_project_milestones, 1}]
    },
    %{
      row: :workspace_paths,
      tests: [
        {"elixir/test/symphony_elixir/isolation_process_workspace_test.exs", "workspace paths use per-project workspace roots"}
      ],
      apis: [{Workspace, :remove_issue_workspaces, 3}]
    },
    %{
      row: :runtime_ledger_cache,
      tests: [
        {"elixir/test/symphony_elixir/isolation_acp_workspace_cleanup_test.exs", "cleanup_issue_runtime_cache with project settings only removes that project's workspace"}
      ],
      apis: [{Workspace, :cleanup_issue_runtime_cache, 2}]
    },
    %{
      row: :prompt_packet_context,
      tests: [
        {"elixir/test/symphony_elixir/isolation_workflow_config_prompt_test.exs", "build_prompt does not leak templates across projects for the same issue"},
        {"elixir/test/symphony_elixir/isolation_orchestrator_steward_registry_test.exs", "build/2 returns project-specific payload for different project contexts"}
      ],
      apis: [
        {PromptBuilder, :build_prompt, 2},
        {SymphonyElixir.Steward.ExecutionPacket, :build, 2}
      ]
    },
    %{
      row: :runner_session_identity,
      tests: [
        {
          "elixir/test/symphony_elixir/isolation_acceptance_contract_test.exs",
          "acceptance session store keeps runner sessions isolated by issue, project root, and prompt scope"
        }
      ],
      apis: [
        {ACPSessionStore, :put, 5},
        {ACPSessionStore, :fetch, 4},
        {Orchestrator, :snapshot, 2}
      ]
    },
    %{
      row: :dashboard_api_aggregate_identity,
      tests: [
        {
          "elixir/test/symphony_elixir/isolation_acceptance_contract_test.exs",
          "acceptance projection preserves runner aggregate identity across presenter and dashboard"
        }
      ],
      apis: [
        {Presenter, :state_payload, 2},
        {Presenter, :issue_payload, 3},
        {StatusDashboard, :format_snapshot_content_for_test, 3}
      ]
    },
    %{
      row: :config_process_policy_isolation,
      tests: [
        {"elixir/test/symphony_elixir/isolation_config_settings_core_test.exs", "Config.settings!/1 returns per-project codex.command"},
        {"elixir/test/symphony_elixir/isolation_process_workspace_test.exs", "codex_owned_rca_required_state returns per-project value"},
        {"elixir/test/symphony_elixir/isolation_tracker_workspace_runtime_edges_test.exs", "dispatchable? returns true for valid project and false for disabled project"}
      ],
      apis: [{Config, :settings!, 1}, {SymphonyElixir.ProjectContext, :dispatchable?, 1}]
    },
    %{
      row: :existing_single_project_behavior,
      tests: [{"elixir/test/symphony_elixir/core_test.exs", "config defaults and validation checks"}],
      apis: [{Config, :settings!, 0}]
    },
    %{
      row: :full_suite_safety,
      tests: [{"elixir/test/symphony_elixir/isolation_acceptance_contract_test.exs", "acceptance matrix names executable tests and production APIs"}],
      apis: [{Orchestrator, :snapshot, 2}]
    },
    %{
      row: :cleanup_no_bak_artifact,
      tests: [
        {"elixir/test/symphony_elixir/isolation_acp_workspace_cleanup_test.exs", "remove_legacy_runtime_cache only removes pulse_ledger.json from the project's own workspace root"}
      ],
      apis: [{Workspace, :remove_legacy_runtime_cache, 1}]
    }
  ]

  test "acceptance matrix names executable tests and production APIs" do
    assert Enum.map(@acceptance_matrix, & &1.row) == [
             :tracker,
             :workspace_paths,
             :runtime_ledger_cache,
             :prompt_packet_context,
             :runner_session_identity,
             :dashboard_api_aggregate_identity,
             :config_process_policy_isolation,
             :existing_single_project_behavior,
             :full_suite_safety,
             :cleanup_no_bak_artifact
           ]

    for %{tests: tests, apis: apis} <- @acceptance_matrix do
      for {path, test_name} <- tests do
        source = File.read!(Path.expand(Path.join([__DIR__, "..", "..", "..", path])))
        assert source =~ ~s(test "#{test_name}")
      end

      for {module, function, arity} <- apis do
        assert Code.ensure_loaded?(module)
        assert function_exported?(module, function, arity)
      end
    end
  end

  test "acceptance projection preserves runner aggregate identity across presenter and dashboard" do
    write_workflow_file!(Workflow.workflow_file_path(),
      stewardship_active_milestone_id: "milestone-acceptance",
      stewardship_active_milestone_name: "Acceptance"
    )

    orchestrator_name = Module.concat(__MODULE__, :PresenterAggregateOrchestrator)
    {:ok, pid} = Orchestrator.start_link(name: orchestrator_name, dispatch_paused?: true)

    on_exit(fn ->
      if Process.alive?(pid) do
        Process.exit(pid, :normal)
      end
    end)

    now = DateTime.utc_now()

    running_entry = %{
      issue_id: "issue-alpha-running",
      identifier: "SYM-15",
      project_id: "project-alpha",
      project_name: "Alpha Project",
      project_root: "/home/agent/proj/symphony-alpha",
      issue: %Issue{id: "issue-alpha-running", identifier: "SYM-15", title: "Acceptance Alpha", state: "In Progress"},
      state: "In Progress",
      runner_kind: "opencode",
      runner_owner: "opencode",
      runner_phase: :command,
      runner_command: ["opencode", "run", "--session", "ses-alpha"],
      runner_project_root: "/home/agent/proj/symphony-alpha",
      runner_attach_url: "http://127.0.0.1:3000/session/ses-alpha",
      workspace_path: "/tmp/symphony-alpha/SYM-15",
      session_id: "ses-alpha",
      codex_input_tokens: 31,
      codex_output_tokens: 7,
      codex_total_tokens: 38,
      turn_count: 2,
      started_at: now,
      last_codex_timestamp: now,
      last_codex_event: :command_prepared,
      last_codex_message: %{event: :command_prepared, message: nil}
    }

    beta_entry = %{
      running_entry
      | issue_id: "issue-beta-running",
        project_id: "project-beta",
        project_name: "Beta Project",
        project_root: "/home/agent/proj/symphony-beta",
        issue: %Issue{id: "issue-beta-running", identifier: "SYM-15", title: "Acceptance Beta", state: "In Progress"},
        runner_command: ["opencode", "run", "--session", "ses-beta"],
        runner_project_root: "/home/agent/proj/symphony-beta",
        runner_attach_url: "http://127.0.0.1:3000/session/ses-beta",
        workspace_path: "/tmp/symphony-beta/SYM-15",
        session_id: "ses-beta"
    }

    :sys.replace_state(pid, fn state ->
      %{
        state
        | running: %{running_entry.issue_id => running_entry, beta_entry.issue_id => beta_entry},
          active_project_milestone_id: "milestone-acceptance",
          codex_totals: %{input_tokens: 31, output_tokens: 7, total_tokens: 38, seconds_running: 0},
          runner_runtime_totals: %{seconds_running: 11},
          suppression_counts: %{"active_milestone_locked" => 1}
      }
    end)

    snapshot = Orchestrator.snapshot(orchestrator_name, 1_000)
    assert %{running: snapshot_entries} = snapshot
    assert length(snapshot_entries) == 2

    alpha_snapshot = Enum.find(snapshot_entries, &(&1.issue_id == "issue-alpha-running"))
    beta_snapshot = Enum.find(snapshot_entries, &(&1.issue_id == "issue-beta-running"))

    assert alpha_snapshot.identifier == "SYM-15"
    assert alpha_snapshot.project_id == "project-alpha"
    assert alpha_snapshot.project_name == "Alpha Project"
    assert alpha_snapshot.project_root == "/home/agent/proj/symphony-alpha"
    assert alpha_snapshot.runner_owner == "opencode"
    assert alpha_snapshot.runner_command == ["opencode", "run", "--session", "ses-alpha"]
    assert alpha_snapshot.workspace_path == "/tmp/symphony-alpha/SYM-15"
    assert alpha_snapshot.session_id == "ses-alpha"

    assert beta_snapshot.identifier == "SYM-15"
    assert beta_snapshot.project_id == "project-beta"
    assert beta_snapshot.project_name == "Beta Project"
    assert beta_snapshot.project_root == "/home/agent/proj/symphony-beta"
    assert beta_snapshot.runner_project_root == "/home/agent/proj/symphony-beta"
    assert beta_snapshot.workspace_path == "/tmp/symphony-beta/SYM-15"
    assert beta_snapshot.session_id == "ses-beta"
    assert snapshot.active_milestone == %{milestone_id: "milestone-acceptance", milestone_name: "Acceptance"}

    payload = Presenter.state_payload(orchestrator_name, 1_000)
    assert payload.counts.running == 2
    assert payload.active_milestone == %{milestone_id: "milestone-acceptance", milestone_name: "Acceptance"}

    alpha_payload = Enum.find(payload.running, &(&1.issue_id == "issue-alpha-running"))
    beta_payload = Enum.find(payload.running, &(&1.issue_id == "issue-beta-running"))

    assert alpha_payload.project == %{id: "project-alpha", name: "Alpha Project", root: "/home/agent/proj/symphony-alpha"}
    assert alpha_payload.runner.owner == "opencode"
    assert alpha_payload.runner.command == ["opencode", "run", "--session", "ses-alpha"]
    assert alpha_payload.session_id == "ses-alpha"

    assert beta_payload.project == %{id: "project-beta", name: "Beta Project", root: "/home/agent/proj/symphony-beta"}
    assert beta_payload.runner.project_root == "/home/agent/proj/symphony-beta"
    assert beta_payload.workspace_path == "/tmp/symphony-beta/SYM-15"
    assert beta_payload.session_id == "ses-beta"

    assert {:ok, issue_payload} = Presenter.issue_payload("SYM-15", orchestrator_name, 1_000)
    assert issue_payload.issue_id in ["issue-alpha-running", "issue-beta-running"]
    assert Enum.map(issue_payload.matches, & &1.issue_id) |> Enum.sort() == ["issue-alpha-running", "issue-beta-running"]
    assert Enum.map(issue_payload.matches, & &1.project.id) |> Enum.sort() == ["project-alpha", "project-beta"]
    assert Enum.map(issue_payload.matches, & &1.project.name) |> Enum.sort() == ["Alpha Project", "Beta Project"]

    assert Enum.map(issue_payload.matches, & &1.project.root) |> Enum.sort() == [
             "/home/agent/proj/symphony-alpha",
             "/home/agent/proj/symphony-beta"
           ]

    rendered = StatusDashboard.format_snapshot_content_for_test({:ok, snapshot}, 0.0, @terminal_columns)
    assert rendered =~ "Milestone:"
    assert rendered =~ "Acceptance"
    assert rendered =~ "active_milestone_locked: 1"
    assert rendered =~ "SYM-15"
    assert rendered =~ "command prepared"
  end

  test "acceptance session store keeps runner sessions isolated by issue, project root, and prompt scope" do
    issue = %Issue{id: "issue-acceptance-session", identifier: "SYM-15", title: "Acceptance", state: "Todo"}

    store_root = Path.join(System.tmp_dir!(), "symphony-acceptance-sessions-#{System.unique_integer([:positive])}")
    alpha_root = Path.join(store_root, "alpha")
    beta_root = Path.join(store_root, "beta")
    File.mkdir_p!(alpha_root)
    File.mkdir_p!(beta_root)

    write_workflow_file!(Workflow.workflow_file_path(), workspace_root: store_root)
    store_opts = [settings: Config.settings!()]

    on_exit(fn -> File.rm_rf(store_root) end)

    alpha_scope = ACPSessionStore.prompt_scope("alpha prompt")
    beta_scope = ACPSessionStore.prompt_scope("beta prompt")

    assert :ok = ACPSessionStore.put(issue, alpha_root, "ses-alpha", alpha_scope, store_opts)
    assert :ok = ACPSessionStore.put(issue, alpha_root, "ses-alpha-beta-scope", beta_scope, store_opts)
    assert :ok = ACPSessionStore.put(issue, beta_root, "ses-beta", alpha_scope, store_opts)

    assert {:ok, "ses-alpha"} = ACPSessionStore.fetch(issue, alpha_root, alpha_scope, store_opts)
    assert {:ok, "ses-alpha-beta-scope"} = ACPSessionStore.fetch(issue, alpha_root, beta_scope, store_opts)
    assert {:ok, "ses-beta"} = ACPSessionStore.fetch(issue, beta_root, alpha_scope, store_opts)

    refute {:ok, "ses-beta"} == ACPSessionStore.fetch(issue, alpha_root, alpha_scope, store_opts)
    refute {:ok, "ses-alpha"} == ACPSessionStore.fetch(issue, beta_root, alpha_scope, store_opts)
    refute {:ok, "ses-alpha"} == ACPSessionStore.fetch(issue, alpha_root, beta_scope, store_opts)
  end
end
