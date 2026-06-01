defmodule SymphonyElixir.ProjectContext do
  @moduledoc """
  Passive per-project context carried by root multiproject configuration.

  This module intentionally does not start processes. It gives later dynamic
  supervisor work a stable, validated carrier for per-project paths and process
  names while keeping the existing single `WORKFLOW.md` runtime untouched.
  """

  @enforce_keys [:id, :name, :enabled, :status, :workflow_path, :process_names]
  defstruct [
    :id,
    :project_id,
    :name,
    :status,
    :repo_root,
    :app_root,
    :workflow_path,
    :dashboard_order,
    :logs_root,
    :linear,
    :mnemesh,
    :runner,
    :execution,
    :gates,
    :errors,
    :process_names,
    enabled: false
  ]

  @type process_names :: %{
          workflow_store: term(),
          task_supervisor: term(),
          orchestrator: term(),
          http_server: term(),
          status_dashboard: term()
        }

  @type t :: %__MODULE__{
          id: String.t(),
          project_id: String.t(),
          name: String.t(),
          enabled: boolean(),
          status: :valid | :disabled | :invalid,
          repo_root: Path.t() | nil,
          app_root: Path.t() | nil,
          workflow_path: Path.t(),
          dashboard_order: integer() | nil,
          logs_root: Path.t() | nil,
          linear: map(),
          mnemesh: map(),
          runner: map(),
          execution: map(),
          gates: map(),
          errors: [term()],
          process_names: process_names()
        }

  @spec new(map()) :: t()
  def new(attrs) when is_map(attrs) do
    id = Map.get(attrs, :id) || Map.fetch!(attrs, :project_id)
    enabled = Map.get(attrs, :enabled, false)
    errors = Map.get(attrs, :errors, [])
    status = Map.get(attrs, :status) || default_status(enabled, errors)

    %__MODULE__{
      id: id,
      project_id: id,
      name: Map.get(attrs, :name) || id,
      enabled: enabled,
      status: status,
      repo_root: Map.get(attrs, :repo_root),
      app_root: Map.get(attrs, :app_root),
      workflow_path: Map.fetch!(attrs, :workflow_path),
      dashboard_order: Map.get(attrs, :dashboard_order),
      logs_root: Map.get(attrs, :logs_root),
      linear: Map.get(attrs, :linear, %{}),
      mnemesh: Map.get(attrs, :mnemesh, %{}),
      runner: Map.get(attrs, :runner, %{}),
      execution: Map.get(attrs, :execution, %{"enabled" => true}),
      gates: Map.get(attrs, :gates, %{"dispatch_enabled" => true}),
      errors: errors,
      process_names: Map.get(attrs, :process_names) || process_names(id)
    }
  end

  @spec dispatchable?(t()) :: boolean()
  def dispatchable?(%__MODULE__{} = context) do
    is_nil(dispatch_blocker(context))
  end

  @spec dispatch_blocker(t()) ::
          :disabled
          | {:invalid_project, [term()]}
          | {:missing_workflow_file, Path.t()}
          | :execution_disabled
          | :gate_disabled
          | nil
  def dispatch_blocker(%__MODULE__{status: :invalid, errors: errors}) do
    {:invalid_project, errors}
  end

  def dispatch_blocker(%__MODULE__{enabled: false}), do: :disabled
  def dispatch_blocker(%__MODULE__{status: :disabled}), do: :disabled

  def dispatch_blocker(%__MODULE__{execution: execution, gates: gates, workflow_path: workflow_path}) do
    cond do
      not File.regular?(workflow_path) -> {:missing_workflow_file, workflow_path}
      not map_enabled?(execution, "enabled", true) -> :execution_disabled
      not map_enabled?(gates, "dispatch_enabled", true) -> :gate_disabled
      true -> nil
    end
  end

  @spec process_names(String.t()) :: process_names()
  def process_names(project_id) when is_binary(project_id) do
    %{
      workflow_store: {:symphony_project, project_id, :workflow_store},
      task_supervisor: {:symphony_project, project_id, :task_supervisor},
      orchestrator: {:symphony_project, project_id, :orchestrator},
      http_server: {:symphony_project, project_id, :http_server},
      status_dashboard: {:symphony_project, project_id, :status_dashboard}
    }
  end

  defp default_status(false, _errors), do: :disabled
  defp default_status(true, []), do: :valid
  defp default_status(true, _errors), do: :invalid

  defp map_enabled?(map, key, default) when is_map(map) do
    Map.get(map, key, default) not in [false, "false"]
  end

  defp map_enabled?(_map, _key, default), do: default
end
