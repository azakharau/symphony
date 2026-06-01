defmodule SymphonyElixir.ProjectSupervisor do
  @moduledoc """
  Per-project lifecycle supervisor for root mode.

  The supervisor starts project-local infrastructure that is safe to lifecycle
  independently. Per-project orchestrator dispatch remains deferred until the
  existing global orchestrator is made project-context aware.
  """

  use Supervisor

  alias SymphonyElixir.{Orchestrator, ProjectContext, ProjectRegistry, WorkflowStore}

  @spec start_link(ProjectContext.t()) :: Supervisor.on_start()
  def start_link(%ProjectContext{} = context) do
    name = ProjectRegistry.via_name({:project_supervisor, context.project_id})

    Supervisor.start_link(__MODULE__, context, name: name)
  end

  @impl true
  def init(%ProjectContext{} = context) do
    names = context.process_names

    children = [
      {WorkflowStore, name: ProjectRegistry.via_name(names.workflow_store), workflow_path: context.workflow_path},
      {Task.Supervisor, name: ProjectRegistry.via_name(names.task_supervisor)},
      {Orchestrator, name: ProjectRegistry.via_name(names.orchestrator), dispatch_paused?: true, project_context: context}
    ]

    Supervisor.init(children, strategy: :one_for_one)
  end

  @spec child_spec(ProjectContext.t()) :: Supervisor.child_spec()
  def child_spec(%ProjectContext{} = context) do
    %{
      id: {:project_supervisor, context.project_id},
      start: {__MODULE__, :start_link, [context]},
      restart: :permanent,
      type: :supervisor
    }
  end
end
