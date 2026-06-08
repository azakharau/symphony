defmodule SymphonyElixir.IsolationRegressionSupport do
  alias SymphonyElixir.Linear.Issue
  alias SymphonyElixir.ProjectContext
  alias SymphonyElixir.ProjectRegistry
  alias SymphonyElixir.WorkflowStore

  def with_memory_project_context(project_id, test_root, test_fn) do
    ensure_project_registry_started!()

    workflow_path = Path.join([test_root, project_id, "WORKFLOW.md"])
    File.mkdir_p!(Path.dirname(workflow_path))
    File.write!(workflow_path, "---\ntracker:\n  kind: memory\n---\n#{project_id} prompt\n")

    context =
      ProjectContext.new(%{
        id: project_id,
        enabled: true,
        workflow_path: workflow_path
      })

    store_name = ProjectRegistry.via_name(context.process_names.workflow_store)

    _pid =
      ExUnit.Callbacks.start_supervised!(
        {WorkflowStore, [name: store_name, workflow_path: workflow_path]},
        id: {WorkflowStore, project_id}
      )

    test_fn.(context)
  end

  def with_custom_project_context(project_id, test_root, workflow_content, test_fn) do
    ensure_project_registry_started!()

    prompt_body = workflow_content[:prompt_body] || "#{project_id} prompt"
    extra_lines = workflow_content[:front_matter_overrides] || []

    fm_lines =
      [
        "---",
        "tracker:",
        "  kind: memory",
        extra_lines,
        "---"
      ]
      |> List.flatten()

    workflow_path = Path.join([test_root, project_id, "WORKFLOW.md"])
    File.mkdir_p!(Path.dirname(workflow_path))
    File.write!(workflow_path, Enum.join(fm_lines, "\n") <> "\n" <> prompt_body <> "\n")

    context =
      ProjectContext.new(%{
        id: project_id,
        enabled: true,
        workflow_path: workflow_path
      })

    store_name = ProjectRegistry.via_name(context.process_names.workflow_store)

    _pid =
      ExUnit.Callbacks.start_supervised!(
        {WorkflowStore, [name: store_name, workflow_path: workflow_path]},
        id: {WorkflowStore, project_id}
      )

    test_fn.(context)
  end

  def make_issue(project_key, id_suffix) do
    %Issue{
      id: "#{project_key}-#{id_suffix}",
      identifier: "#{project_key}-#{id_suffix}",
      title: "#{project_key} issue #{id_suffix}",
      state: "Todo"
    }
  end

  defp ensure_project_registry_started! do
    unless Process.whereis(ProjectRegistry) do
      ExUnit.Callbacks.start_supervised!(ProjectRegistry)
    end
  end
end
