defmodule SymphonyElixir.IsolationRegression.WorkflowConfigPromptTest do
  use ExUnit.Case, async: false

  import SymphonyElixir.IsolationRegressionSupport, except: [make_issue: 2]
  alias SymphonyElixir.Config
  alias SymphonyElixir.Linear.Issue
  alias SymphonyElixir.ProjectRegistry
  alias SymphonyElixir.PromptBuilder
  alias SymphonyElixir.WorkflowStore

  describe "workflow_store per-project independence" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-workflow-store-#{System.unique_integer([:positive])}"
        )

      File.mkdir_p!(test_root)

      on_exit(fn ->
        File.rm_rf(test_root)
      end)

      %{test_root: test_root}
    end

    defp write_workflow!(root, project_id, prompt) do
      path = Path.join([root, project_id, "WORKFLOW.md"])
      File.mkdir_p!(Path.dirname(path))

      content = """
      ---
      tracker:
        kind: memory
        project_slug: "#{project_id}-slug"
      ---
      #{prompt}
      """

      File.write!(path, content)
      path
    end

    test "named workflow stores serve independent content", %{test_root: root} do
      alpha_path = write_workflow!(root, "alpha", "alpha prompt body")
      beta_path = write_workflow!(root, "beta", "beta prompt body")

      alpha_store = :alpha_workflow_store
      beta_store = :beta_workflow_store

      start_supervised!(
        {WorkflowStore, [name: alpha_store, workflow_path: alpha_path]},
        id: :workflow_store_alpha
      )

      start_supervised!(
        {WorkflowStore, [name: beta_store, workflow_path: beta_path]},
        id: :workflow_store_beta
      )

      assert {:ok, %{prompt: "alpha prompt body"}} = WorkflowStore.current(alpha_store)
      assert {:ok, %{prompt: "beta prompt body"}} = WorkflowStore.current(beta_store)
    end

    test "stopping one workflow store does not affect the other", %{test_root: root} do
      alpha_path = write_workflow!(root, "alpha", "alpha stable")
      beta_path = write_workflow!(root, "beta", "beta stable")

      alpha_store = :alpha_store_stop_test
      beta_store = :beta_store_stop_test

      start_supervised!(
        {WorkflowStore, [name: alpha_store, workflow_path: alpha_path]},
        id: :ws_stop_alpha
      )

      start_supervised!(
        {WorkflowStore, [name: beta_store, workflow_path: beta_path]},
        id: :ws_stop_beta
      )

      assert {:ok, %{prompt: "alpha stable"}} = WorkflowStore.current(alpha_store)
      assert {:ok, %{prompt: "beta stable"}} = WorkflowStore.current(beta_store)

      stop_supervised!(:ws_stop_alpha)

      assert {:ok, %{prompt: "beta stable"}} = WorkflowStore.current(beta_store)
      assert GenServer.whereis(alpha_store) == nil
    end

    test "workflow stores track independent file stamps", %{test_root: root} do
      alpha_path = write_workflow!(root, "alpha", "alpha initial")
      beta_path = write_workflow!(root, "beta", "beta initial")

      alpha_store = :alpha_stamp_store
      beta_store = :beta_stamp_store

      start_supervised!(
        {WorkflowStore, [name: alpha_store, workflow_path: alpha_path]},
        id: :ws_stamp_alpha
      )

      start_supervised!(
        {WorkflowStore, [name: beta_store, workflow_path: beta_path]},
        id: :ws_stamp_beta
      )

      assert {:ok, %{prompt: "alpha initial"}} = WorkflowStore.current(alpha_store)
      assert {:ok, %{prompt: "beta initial"}} = WorkflowStore.current(beta_store)

      File.write!(alpha_path, "---\ntracker:\n  kind: memory\n---\nalpha changed!\n")

      Process.sleep(1_100)

      assert {:ok, %{prompt: "alpha changed!"}} = WorkflowStore.current(alpha_store)
      assert {:ok, %{prompt: "beta initial"}} = WorkflowStore.current(beta_store)
    end

    test "workflow stores use their own configured workflow path", %{test_root: root} do
      alpha_path = write_workflow!(root, "alpha", "alpha exclusive path")
      beta_path = write_workflow!(root, "beta", "beta exclusive path")

      alpha_store = :alpha_path_store
      beta_store = :beta_path_store

      start_supervised!(
        {WorkflowStore, [name: alpha_store, workflow_path: alpha_path]},
        id: :ws_path_alpha
      )

      start_supervised!(
        {WorkflowStore, [name: beta_store, workflow_path: beta_path]},
        id: :ws_path_beta
      )

      assert {:ok, %{config: %{"tracker" => %{"project_slug" => "alpha-slug"}}}} =
               WorkflowStore.current(alpha_store)

      assert {:ok, %{config: %{"tracker" => %{"project_slug" => "beta-slug"}}}} =
               WorkflowStore.current(beta_store)
    end

    test "workflow store preserves fixed_path across reloads", %{test_root: root} do
      path = write_workflow!(root, "alpha", "alpha initial")
      store = :fixed_path_store

      start_supervised!(
        {WorkflowStore, [name: store, workflow_path: path]},
        id: :ws_fixed_path
      )

      assert {:ok, %{prompt: "alpha initial"}} = WorkflowStore.current(store)

      # Write new content to the same path
      File.write!(path, "---
tracker:
  kind: memory
---
alpha reloaded!
")

      Process.sleep(1_100)

      # After reload, the store should use the same configured path (not
      # Workflow.workflow_file_path()) and see the updated content.
      assert {:ok, %{prompt: "alpha reloaded!"}} = WorkflowStore.current(store)
    end
  end

  # ────────────────────────────────────────────────────────────────
  # Config per-project settings isolation tests
  # ────────────────────────────────────────────────────────────────

  describe "config per-project settings isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-config-#{System.unique_integer([:positive])}"
        )

      File.mkdir_p!(test_root)

      unless Process.whereis(ProjectRegistry) do
        start_supervised!(ProjectRegistry)
      end

      on_exit(fn ->
        File.rm_rf(test_root)
      end)

      %{test_root: test_root}
    end

    test "Config.settings!/1 returns per-project tracker project_slug", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "  project_slug: alpha-slug"
          ]
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              front_matter_overrides: [
                "  project_slug: beta-slug"
              ]
            },
            fn beta_ctx ->
              alpha_settings = Config.settings!(alpha_ctx)
              beta_settings = Config.settings!(beta_ctx)

              assert alpha_settings.tracker.project_slug == "alpha-slug"
              assert beta_settings.tracker.project_slug == "beta-slug"
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project agent.max_concurrent_agents", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "agent:",
            "  max_concurrent_agents: 3"
          ]
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              front_matter_overrides: [
                "agent:",
                "  max_concurrent_agents: 7"
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).agent.max_concurrent_agents == 3
              assert Config.settings!(beta_ctx).agent.max_concurrent_agents == 7
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project agent.max_turns", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "agent:",
            "  max_turns: 5"
          ]
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              front_matter_overrides: [
                "agent:",
                "  max_turns: 15"
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).agent.max_turns == 5
              assert Config.settings!(beta_ctx).agent.max_turns == 15
            end
          )
        end
      )
    end

    test "Config.settings!/1 returns per-project polling.interval_ms", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "polling:",
            "  interval_ms: 5000"
          ]
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              front_matter_overrides: [
                "polling:",
                "  interval_ms: 60000"
              ]
            },
            fn beta_ctx ->
              assert Config.settings!(alpha_ctx).polling.interval_ms == 5_000
              assert Config.settings!(beta_ctx).polling.interval_ms == 60_000
            end
          )
        end
      )
    end

    test "Config.workflow_prompt/1 returns per-project prompt template", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          prompt_body: "Alpha specific prompt template"
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              prompt_body: "Beta specific prompt template"
            },
            fn beta_ctx ->
              assert Config.workflow_prompt(alpha_ctx) =~ "Alpha specific prompt template"
              assert Config.workflow_prompt(beta_ctx) =~ "Beta specific prompt template"
              refute Config.workflow_prompt(alpha_ctx) =~ "Beta"
            end
          )
        end
      )
    end

    test "Config.max_concurrent_agents_for_state/2 returns per-project values", %{test_root: root} do
      with_custom_project_context(
        "alpha",
        root,
        %{
          front_matter_overrides: [
            "agent:",
            "  max_concurrent_agents: 2",
            "  max_concurrent_agents_by_state:",
            "    \"In Progress\": 1"
          ]
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              front_matter_overrides: [
                "agent:",
                "  max_concurrent_agents: 5",
                "  max_concurrent_agents_by_state:",
                "    \"In Progress\": 3"
              ]
            },
            fn beta_ctx ->
              assert Config.max_concurrent_agents_for_state("Todo", alpha_ctx) == 2
              assert Config.max_concurrent_agents_for_state("In Progress", alpha_ctx) == 1

              assert Config.max_concurrent_agents_for_state("Todo", beta_ctx) == 5
              assert Config.max_concurrent_agents_for_state("In Progress", beta_ctx) == 3
            end
          )
        end
      )
    end
  end

  # ────────────────────────────────────────────────────────────────
  # PromptBuilder per-project isolation tests
  # ────────────────────────────────────────────────────────────────

  describe "prompt_builder per-project isolation" do
    setup do
      test_root =
        Path.join(
          System.tmp_dir!(),
          "isolation-prompt-builder-#{System.unique_integer([:positive])}"
        )

      File.mkdir_p!(test_root)

      unless Process.whereis(ProjectRegistry) do
        start_supervised!(ProjectRegistry)
      end

      on_exit(fn ->
        File.rm_rf(test_root)
      end)

      %{test_root: test_root}
    end

    defp make_issue(project_key, id_suffix) do
      %Issue{
        id: "#{project_key}-#{id_suffix}",
        identifier: "#{String.upcase(project_key)}-#{id_suffix}",
        title: "#{project_key} issue",
        state: "Todo"
      }
    end

    test "build_prompt uses the per-project workflow template from the project's WorkflowStore", %{
      test_root: root
    } do
      with_custom_project_context(
        "alpha",
        root,
        %{
          prompt_body: "Alpha template for {{ issue.identifier }}"
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              prompt_body: "Beta template for {{ issue.identifier }}"
            },
            fn beta_ctx ->
              alpha_issue = make_issue("alpha", "101")
              beta_issue = make_issue("beta", "202")

              alpha_prompt = PromptBuilder.build_prompt(alpha_issue, project_context: alpha_ctx)
              beta_prompt = PromptBuilder.build_prompt(beta_issue, project_context: beta_ctx)

              assert alpha_prompt =~ "Alpha template for ALPHA-101"
              assert beta_prompt =~ "Beta template for BETA-202"
              refute alpha_prompt =~ "Beta"
              refute beta_prompt =~ "Alpha"
            end
          )
        end
      )
    end

    test "build_prompt does not leak templates across projects for the same issue", %{
      test_root: root
    } do
      with_custom_project_context(
        "alpha",
        root,
        %{
          prompt_body: "Alpha says: {{ issue.title }}"
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              prompt_body: "Beta says: {{ issue.title }}"
            },
            fn beta_ctx ->
              shared_issue = make_issue("shared", "X1")

              alpha_prompt = PromptBuilder.build_prompt(shared_issue, project_context: alpha_ctx)
              beta_prompt = PromptBuilder.build_prompt(shared_issue, project_context: beta_ctx)

              assert alpha_prompt =~ "Alpha says"
              assert beta_prompt =~ "Beta says"
              refute alpha_prompt =~ "Beta says"
              refute beta_prompt =~ "Alpha says"
            end
          )
        end
      )
    end

    test "build_prompt renders the correct issue data per project context", %{
      test_root: root
    } do
      with_custom_project_context(
        "alpha",
        root,
        %{
          prompt_body: "{{ issue.identifier }}: {{ issue.title }}"
        },
        fn alpha_ctx ->
          with_custom_project_context(
            "beta",
            root,
            %{
              prompt_body: "{{ issue.identifier }}: {{ issue.title }}"
            },
            fn beta_ctx ->
              alpha_issue = %Issue{id: "a-1", identifier: "ALPHA-1", title: "Alpha one", state: "Todo"}
              beta_issue = %Issue{id: "b-1", identifier: "BETA-1", title: "Beta one", state: "Todo"}

              assert PromptBuilder.build_prompt(alpha_issue, project_context: alpha_ctx) =~
                       "ALPHA-1: Alpha one"

              assert PromptBuilder.build_prompt(beta_issue, project_context: beta_ctx) =~
                       "BETA-1: Beta one"
            end
          )
        end
      )
    end
  end

  # ────────────────────────────────────────────────────────────────
  # ProcessPolicy per-project isolation tests
  # ────────────────────────────────────────────────────────────────
end
