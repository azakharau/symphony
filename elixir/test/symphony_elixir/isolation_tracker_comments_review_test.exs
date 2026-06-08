defmodule SymphonyElixir.IsolationRegression.TrackerCommentsReviewTest do
  use ExUnit.Case, async: false

  import SymphonyElixir.IsolationRegressionSupport
  alias SymphonyElixir.Linear.Issue
  alias SymphonyElixir.ProjectContext
  alias SymphonyElixir.ProjectRegistry
  alias SymphonyElixir.RuntimeCache
  alias SymphonyElixir.Tracker

  describe "tracker per-project data isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-regression-tracker-#{System.unique_integer([:positive])}"
        )

      File.mkdir_p!(test_root)

      unless Process.whereis(ProjectRegistry) do
        start_supervised!(ProjectRegistry)
      end

      Application.put_env(:symphony_elixir, :memory_tracker_recipient, self())

      on_exit(fn ->
        File.rm_rf(test_root)
        Application.delete_env(:symphony_elixir, :memory_tracker_project_issues)
        Application.delete_env(:symphony_elixir, :memory_tracker_issues)
        Application.delete_env(:symphony_elixir, :memory_tracker_recipient)
        Application.delete_env(:symphony_elixir, :memory_tracker_project_milestones_by_project)
        Application.delete_env(:symphony_elixir, :memory_tracker_project_milestones)
        Application.delete_env(:symphony_elixir, :memory_tracker_project_opencode_comments)
        Application.delete_env(:symphony_elixir, :memory_tracker_opencode_comments)
      end)

      %{test_root: test_root}
    end

    test "fetch_issues_by_states returns only the requesting project's issues", %{test_root: root} do
      alpha_issue = %Issue{id: "alpha-1", identifier: "ALPHA-1", title: "Alpha work", state: "Todo"}
      beta_issue = %Issue{id: "beta-1", identifier: "BETA-1", title: "Beta work", state: "Todo"}

      Application.put_env(:symphony_elixir, :memory_tracker_project_issues, %{
        "alpha" => [alpha_issue],
        "beta" => [beta_issue]
      })

      with_memory_project_context("alpha", root, fn alpha_ctx ->
        with_memory_project_context("beta", root, fn beta_ctx ->
          assert {:ok, [%Issue{identifier: "ALPHA-1"}]} =
                   Tracker.fetch_issues_by_states(["Todo"], alpha_ctx)

          assert {:ok, [%Issue{identifier: "BETA-1"}]} =
                   Tracker.fetch_issues_by_states(["Todo"], beta_ctx)
        end)
      end)
    end

    test "fetch_issue_states_by_ids scopes results to the requesting project's issues", %{
      test_root: root
    } do
      alpha_issue = %Issue{id: "shared-id", identifier: "SHARED", title: "Shared", state: "Todo"}
      beta_issue = %Issue{id: "beta-only", identifier: "BETA-O", title: "Beta only", state: "Todo"}

      Application.put_env(:symphony_elixir, :memory_tracker_project_issues, %{
        "alpha" => [alpha_issue],
        "beta" => [beta_issue]
      })

      with_memory_project_context("alpha", root, fn alpha_ctx ->
        with_memory_project_context("beta", root, fn beta_ctx ->
          assert {:ok, [%Issue{identifier: "SHARED"}]} =
                   Tracker.fetch_issue_states_by_ids(["shared-id"], alpha_ctx)

          assert {:ok, []} =
                   Tracker.fetch_issue_states_by_ids(["shared-id"], beta_ctx)
        end)
      end)
    end

    test "create_comment scopes memory tracker comments by project", %{test_root: root} do
      with_memory_project_context("alpha", root, fn alpha_ctx ->
        with_memory_project_context("beta", root, fn beta_ctx ->
          assert :ok = Tracker.create_comment("issue-1", "alpha says hi", alpha_ctx)
          assert :ok = Tracker.create_comment("issue-1", "beta says hi", beta_ctx)

          assert_receive {:memory_tracker_comment, "alpha", "issue-1", "alpha says hi"}
          assert_receive {:memory_tracker_comment, "beta", "issue-1", "beta says hi"}
        end)
      end)
    end

    test "update_issue_state scopes memory tracker state updates by project", %{test_root: root} do
      with_memory_project_context("alpha", root, fn alpha_ctx ->
        with_memory_project_context("beta", root, fn beta_ctx ->
          assert :ok = Tracker.update_issue_state("issue-1", "In Progress", alpha_ctx)
          assert :ok = Tracker.update_issue_state("issue-2", "Done", beta_ctx)

          assert_receive {:memory_tracker_state_update, "alpha", "issue-1", "In Progress"}
          assert_receive {:memory_tracker_state_update, "beta", "issue-2", "Done"}
        end)
      end)
    end

    test "fetch_candidate_issues returns per-project issues", %{test_root: root} do
      alpha_issue = %Issue{id: "alpha-cand", identifier: "ALPHA-C", title: "Alpha cand", state: "Todo"}

      Application.put_env(:symphony_elixir, :memory_tracker_project_issues, %{
        "alpha" => [alpha_issue]
      })

      with_memory_project_context("alpha", root, fn alpha_ctx ->
        assert {:ok, [%Issue{identifier: "ALPHA-C"}]} =
                 Tracker.fetch_candidate_issues(alpha_ctx)
      end)
    end

    test "fetch_issue_states_by_ids returns empty when project has no matching issues", %{
      test_root: root
    } do
      Application.put_env(:symphony_elixir, :memory_tracker_project_issues, %{
        "projector" => [%Issue{id: "p1", identifier: "P-1", title: "P one", state: "Todo"}]
      })

      with_memory_project_context("other", root, fn other_ctx ->
        assert {:ok, []} = Tracker.fetch_issue_states_by_ids(["p1"], other_ctx)
      end)
    end

    test "fetch_project_milestones returns per-project milestones", %{test_root: root} do
      Application.put_env(:symphony_elixir, :memory_tracker_project_milestones_by_project, %{
        "alpha" => [%{id: "m-alpha", name: "Alpha milestone", description: "phase_state: todo"}]
      })

      with_memory_project_context("alpha", root, fn alpha_ctx ->
        assert {:ok, [%{id: "m-alpha"}]} = Tracker.fetch_project_milestones(alpha_ctx)
      end)
    end
  end

  # ────────────────────────────────────────────────────────────────
  # RuntimeCache per-project isolation tests
  # ────────────────────────────────────────────────────────────────

  describe "runtime cache per-project isolation" do
    test "handoff_fingerprint_seen? distinguishes same issue_id across different project contexts" do
      alpha_ctx = ProjectContext.new(%{id: "alpha", workflow_path: "/tmp/runtime-alpha/WORKFLOW.md"})
      beta_ctx = ProjectContext.new(%{id: "beta", workflow_path: "/tmp/runtime-beta/WORKFLOW.md"})

      :ok = RuntimeCache.record_handoff_fingerprint(alpha_ctx, "issue-1", "fp-1")

      assert RuntimeCache.handoff_fingerprint_seen?(alpha_ctx, "issue-1", "fp-1")
      refute RuntimeCache.handoff_fingerprint_seen?(beta_ctx, "issue-1", "fp-1")
    end

    test "clear_issue with project context only clears entries for the specified project" do
      alpha_ctx = ProjectContext.new(%{id: "alpha", workflow_path: "/tmp/cache-alpha/WORKFLOW.md"})
      beta_ctx = ProjectContext.new(%{id: "beta", workflow_path: "/tmp/cache-beta/WORKFLOW.md"})

      :ok = RuntimeCache.record_handoff_fingerprint(alpha_ctx, "issue-1", "fp-alpha")
      :ok = RuntimeCache.record_handoff_fingerprint(beta_ctx, "issue-1", "fp-beta")

      :ok = RuntimeCache.clear_issue(alpha_ctx, "issue-1")

      refute RuntimeCache.handoff_fingerprint_seen?(alpha_ctx, "issue-1", "fp-alpha")
      assert RuntimeCache.handoff_fingerprint_seen?(beta_ctx, "issue-1", "fp-beta")
    end

    test "clear_issue with nil project clears all entries for that issue_id regardless of project" do
      alpha_ctx = ProjectContext.new(%{id: "alpha", workflow_path: "/tmp/cache-clearall-alpha/WORKFLOW.md"})
      beta_ctx = ProjectContext.new(%{id: "beta", workflow_path: "/tmp/cache-clearall-beta/WORKFLOW.md"})

      :ok = RuntimeCache.record_handoff_fingerprint(alpha_ctx, "issue-clearall", "fp-alpha")
      :ok = RuntimeCache.record_handoff_fingerprint(beta_ctx, "issue-clearall", "fp-beta")

      :ok = RuntimeCache.clear_issue(nil, "issue-clearall")

      refute RuntimeCache.handoff_fingerprint_seen?(alpha_ctx, "issue-clearall", "fp-alpha")
      refute RuntimeCache.handoff_fingerprint_seen?(beta_ctx, "issue-clearall", "fp-beta")
    end

    test "nil project entries coexist with project-scoped entries" do
      alpha_ctx = ProjectContext.new(%{id: "alpha", workflow_path: "/tmp/cache-coexist/WORKFLOW.md"})

      :ok = RuntimeCache.record_handoff_fingerprint(alpha_ctx, "issue-coexist", "fp-alpha")
      :ok = RuntimeCache.record_handoff_fingerprint(nil, "issue-coexist", "fp-global")

      assert RuntimeCache.handoff_fingerprint_seen?(alpha_ctx, "issue-coexist", "fp-alpha")
      assert RuntimeCache.handoff_fingerprint_seen?(nil, "issue-coexist", "fp-global")
    end

    test "clear_issue with nil does not remove unrelated issue entries" do
      alpha_ctx = ProjectContext.new(%{id: "alpha", workflow_path: "/tmp/cache-keep/WORKFLOW.md"})
      beta_ctx = ProjectContext.new(%{id: "beta", workflow_path: "/tmp/cache-keep-beta/WORKFLOW.md"})

      :ok = RuntimeCache.record_handoff_fingerprint(alpha_ctx, "issue-keep", "fp-keep")
      :ok = RuntimeCache.record_handoff_fingerprint(beta_ctx, "issue-remove", "fp-remove")

      :ok = RuntimeCache.clear_issue(nil, "issue-remove")

      assert RuntimeCache.handoff_fingerprint_seen?(alpha_ctx, "issue-keep", "fp-keep")
      refute RuntimeCache.handoff_fingerprint_seen?(beta_ctx, "issue-remove", "fp-remove")
    end

    test "record_handoff_fingerprint stores project-keyed entries independently" do
      alpha_ctx = ProjectContext.new(%{id: "alpha", workflow_path: "/tmp/cache-independent-alpha/WORKFLOW.md"})
      beta_ctx = ProjectContext.new(%{id: "beta", workflow_path: "/tmp/cache-independent-beta/WORKFLOW.md"})

      :ok = RuntimeCache.record_handoff_fingerprint(alpha_ctx, "issue-r1", "fp-r1")
      :ok = RuntimeCache.record_handoff_fingerprint(beta_ctx, "issue-r2", "fp-r2")

      assert RuntimeCache.handoff_fingerprint_seen?(alpha_ctx, "issue-r1", "fp-r1")
      assert RuntimeCache.handoff_fingerprint_seen?(beta_ctx, "issue-r2", "fp-r2")
      refute RuntimeCache.handoff_fingerprint_seen?(alpha_ctx, "issue-r2", "fp-r2")
      refute RuntimeCache.handoff_fingerprint_seen?(beta_ctx, "issue-r1", "fp-r1")
    end
  end

  # ────────────────────────────────────────────────────────────────
  # OpenCode task prompt per-project isolation tests
  # ────────────────────────────────────────────────────────────────

  describe "opencode task prompt per-project isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-opencode-#{System.unique_integer([:positive])}"
        )

      File.mkdir_p!(test_root)

      unless Process.whereis(ProjectRegistry) do
        start_supervised!(ProjectRegistry)
      end

      on_exit(fn ->
        File.rm_rf(test_root)
        Application.delete_env(:symphony_elixir, :memory_tracker_project_opencode_comments)
        Application.delete_env(:symphony_elixir, :memory_tracker_opencode_comments)
        Application.delete_env(:symphony_elixir, :memory_tracker_project_issues)
        Application.delete_env(:symphony_elixir, :memory_tracker_issues)
      end)

      %{test_root: test_root}
    end

    defp opencode_task_prompt_comment(slice_id, prompt) do
      """
      <!-- symphony:opencode-task-prompt:v1 slice_id=#{slice_id} -->
      ```text
      #{prompt}
      ```
      """
    end

    test "latest_opencode_task_prompt returns per-project comments", %{test_root: root} do
      Application.put_env(:symphony_elixir, :memory_tracker_project_opencode_comments, %{
        "alpha" => %{
          "issue-a1" => [opencode_task_prompt_comment("slice-a1", "alpha prompt content")]
        },
        "beta" => %{
          "issue-b1" => [opencode_task_prompt_comment("slice-b1", "beta prompt content")]
        }
      })

      with_memory_project_context("alpha", root, fn alpha_ctx ->
        with_memory_project_context("beta", root, fn beta_ctx ->
          assert {:ok, "alpha prompt content"} =
                   Tracker.latest_opencode_task_prompt("issue-a1", alpha_ctx)

          assert {:ok, "beta prompt content"} =
                   Tracker.latest_opencode_task_prompt("issue-b1", beta_ctx)

          assert {:error, :opencode_task_prompt_not_found} =
                   Tracker.latest_opencode_task_prompt("issue-b1", alpha_ctx)

          assert {:error, :opencode_task_prompt_not_found} =
                   Tracker.latest_opencode_task_prompt("issue-a1", beta_ctx)
        end)
      end)
    end

    test "latest_opencode_task_packet returns per-project packets from comments", %{test_root: root} do
      Application.put_env(:symphony_elixir, :memory_tracker_project_opencode_comments, %{
        "alpha" => %{
          "issue-common" => [opencode_task_prompt_comment("slice-common-alpha", "alpha packet")]
        },
        "beta" => %{
          "issue-common" => [opencode_task_prompt_comment("slice-common-beta", "beta packet")]
        }
      })

      with_memory_project_context("alpha", root, fn alpha_ctx ->
        with_memory_project_context("beta", root, fn beta_ctx ->
          assert {:ok, packet_alpha} = Tracker.latest_opencode_task_packet("issue-common", alpha_ctx)
          assert packet_alpha.slice_id == "slice-common-alpha"
          assert packet_alpha.prompt == "alpha packet"

          assert {:ok, packet_beta} = Tracker.latest_opencode_task_packet("issue-common", beta_ctx)
          assert packet_beta.slice_id == "slice-common-beta"
          assert packet_beta.prompt == "beta packet"
        end)
      end)
    end

    test "latest_opencode_task_packet falls back to per-project issue description", %{test_root: root} do
      Application.put_env(:symphony_elixir, :memory_tracker_project_issues, %{
        "alpha" => [
          %Issue{
            id: "issue-by-desc",
            identifier: "ALPHA-DESC",
            title: "Alpha desc",
            state: "Todo",
            description: opencode_task_prompt_comment("desc-slice", "desc prompt alpha")
          }
        ],
        "beta" => [
          %Issue{
            id: "issue-by-desc",
            identifier: "BETA-DESC",
            title: "Beta desc",
            state: "Todo",
            description: opencode_task_prompt_comment("desc-slice", "desc prompt beta")
          }
        ]
      })

      with_memory_project_context("alpha", root, fn alpha_ctx ->
        with_memory_project_context("beta", root, fn beta_ctx ->
          assert {:ok, "desc prompt alpha"} =
                   Tracker.latest_opencode_task_prompt("issue-by-desc", alpha_ctx)

          assert {:ok, "desc prompt beta"} =
                   Tracker.latest_opencode_task_prompt("issue-by-desc", beta_ctx)
        end)
      end)
    end
  end

  # ────────────────────────────────────────────────────────────────
  # Review decisions per-project isolation tests
  # ────────────────────────────────────────────────────────────────

  describe "review decisions per-project isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-review-decision-#{System.unique_integer([:positive])}"
        )

      File.mkdir_p!(test_root)

      unless Process.whereis(ProjectRegistry) do
        start_supervised!(ProjectRegistry)
      end

      on_exit(fn ->
        File.rm_rf(test_root)
        Application.delete_env(:symphony_elixir, :memory_tracker_project_opencode_comments)
        Application.delete_env(:symphony_elixir, :memory_tracker_opencode_comments)
      end)

      %{test_root: test_root}
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

    test "review_decisions returns per-project decisions", %{test_root: root} do
      Application.put_env(:symphony_elixir, :memory_tracker_project_opencode_comments, %{
        "alpha" => %{
          "issue-review" => [
            review_decision_comment("accept", "slice-alpha", "alpha looks good")
          ]
        },
        "beta" => %{
          "issue-review" => [
            review_decision_comment("revise", "slice-beta", "beta needs changes")
          ]
        }
      })

      with_memory_project_context("alpha", root, fn alpha_ctx ->
        with_memory_project_context("beta", root, fn beta_ctx ->
          assert {:ok, [alpha_decision]} = Tracker.review_decisions("issue-review", alpha_ctx)
          assert alpha_decision.status == "accept"
          assert alpha_decision.slice_id == "slice-alpha"

          assert {:ok, [beta_decision]} = Tracker.review_decisions("issue-review", beta_ctx)
          assert beta_decision.status == "revise"
          assert beta_decision.slice_id == "slice-beta"
        end)
      end)
    end

    test "review_decisions returns empty for a project with no matching comments", %{test_root: root} do
      Application.put_env(:symphony_elixir, :memory_tracker_project_opencode_comments, %{
        "alpha" => %{
          "issue-review" => [review_decision_comment("accept", "slice-alpha", "ok")]
        }
      })

      with_memory_project_context("alpha", root, fn alpha_ctx ->
        with_memory_project_context("beta", root, fn beta_ctx ->
          assert {:ok, [_decision]} = Tracker.review_decisions("issue-review", alpha_ctx)
          assert {:ok, []} = Tracker.review_decisions("issue-review", beta_ctx)
        end)
      end)
    end

    test "review_decisions isolates decisions for the same issue_id across projects", %{test_root: root} do
      Application.put_env(:symphony_elixir, :memory_tracker_project_opencode_comments, %{
        "alpha" => %{
          "shared-review" => [
            review_decision_comment("accept", "slice-alpha", "alpha approves"),
            review_decision_comment("reject", "slice-alpha-v2", "alpha rejects redesign")
          ]
        },
        "beta" => %{
          "shared-review" => [
            review_decision_comment("revise", "slice-beta", "beta wants changes")
          ]
        }
      })

      with_memory_project_context("alpha", root, fn alpha_ctx ->
        with_memory_project_context("beta", root, fn beta_ctx ->
          assert {:ok, alpha_decisions} = Tracker.review_decisions("shared-review", alpha_ctx)
          assert length(alpha_decisions) == 2
          assert Enum.map(alpha_decisions, & &1.status) == ["accept", "reject"]

          assert {:ok, beta_decisions} = Tracker.review_decisions("shared-review", beta_ctx)
          assert length(beta_decisions) == 1
          assert hd(beta_decisions).status == "revise"
        end)
      end)
    end
  end

  # ────────────────────────────────────────────────────────────────
  # Tracker milestones per-project isolation tests
  # ────────────────────────────────────────────────────────────────

  describe "tracker milestones per-project isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-milestones-#{System.unique_integer([:positive])}"
        )

      File.mkdir_p!(test_root)

      unless Process.whereis(ProjectRegistry) do
        start_supervised!(ProjectRegistry)
      end

      Application.put_env(:symphony_elixir, :memory_tracker_recipient, self())

      on_exit(fn ->
        File.rm_rf(test_root)
        Application.delete_env(:symphony_elixir, :memory_tracker_project_milestones_by_project)
        Application.delete_env(:symphony_elixir, :memory_tracker_project_milestones)
        Application.delete_env(:symphony_elixir, :memory_tracker_recipient)
        Application.delete_env(:symphony_elixir, :memory_tracker_project_issues)
        Application.delete_env(:symphony_elixir, :memory_tracker_issues)
      end)

      %{test_root: test_root}
    end

    test "fetch_project_milestones returns per-project milestones isolated between projects", %{test_root: root} do
      Application.put_env(:symphony_elixir, :memory_tracker_project_milestones_by_project, %{
        "alpha" => [%{id: "m-alpha", name: "Alpha milestone", description: "phase_state: todo"}],
        "beta" => [%{id: "m-beta", name: "Beta milestone", description: "phase_state: in_progress"}]
      })

      with_memory_project_context("alpha", root, fn alpha_ctx ->
        with_memory_project_context("beta", root, fn beta_ctx ->
          assert {:ok, [%{id: "m-alpha"}]} = Tracker.fetch_project_milestones(alpha_ctx)
          assert {:ok, [%{id: "m-beta"}]} = Tracker.fetch_project_milestones(beta_ctx)
        end)
      end)
    end

    test "fetch_project_milestones for a project without per-project entries falls back to default milestones", %{test_root: root} do
      Application.put_env(:symphony_elixir, :memory_tracker_project_milestones, [
        %{id: "m-default", name: "Default milestone", description: "phase_state: todo"}
      ])

      Application.put_env(:symphony_elixir, :memory_tracker_project_milestones_by_project, %{
        "alpha" => [%{id: "m-alpha", name: "Alpha milestone", description: "phase_state: todo"}]
      })

      with_memory_project_context("alpha", root, fn alpha_ctx ->
        with_memory_project_context("beta", root, fn beta_ctx ->
          # Alpha gets its project-specific milestones
          assert {:ok, [%{id: "m-alpha"}]} = Tracker.fetch_project_milestones(alpha_ctx)

          # Beta has no per-project entry, falls back to default
          assert {:ok, [%{id: "m-default"}]} = Tracker.fetch_project_milestones(beta_ctx)
        end)
      end)
    end

    test "fetch_project_milestones with nil context falls through to default memory adapter", %{test_root: root} do
      workflow_path = Path.join(root, "nil-context-WORKFLOW.md")
      File.write!(workflow_path, "---\ntracker:\n  kind: memory\n---\ndefault prompt\n")
      SymphonyElixir.Workflow.set_workflow_file_path(workflow_path)
      on_exit(fn -> Application.delete_env(:symphony_elixir, :workflow_file_path) end)

      Application.put_env(:symphony_elixir, :memory_tracker_project_milestones, [
        %{id: "m-default", name: "Default milestone", description: "phase_state: todo"}
      ])

      assert {:ok, [%{id: "m-default"}]} = Tracker.fetch_project_milestones(nil)
    end

    test "fetch_project_milestones with empty per-project map returns empty list for missing project", %{test_root: root} do
      Application.put_env(:symphony_elixir, :memory_tracker_project_milestones_by_project, %{})

      with_memory_project_context("gamma", root, fn gamma_ctx ->
        assert {:ok, []} = Tracker.fetch_project_milestones(gamma_ctx)
      end)
    end
  end

  # ────────────────────────────────────────────────────────────────
  # WorkflowStore per-project independence tests
  # ────────────────────────────────────────────────────────────────
end
