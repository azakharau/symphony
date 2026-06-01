defmodule SymphonyElixir.RootConfig do
  @moduledoc """
  Passive parser for root `projects.yml` multiproject configuration.
  """

  alias SymphonyElixir.Config.Schema
  alias SymphonyElixir.ProjectContext
  alias SymphonyElixir.Workflow

  @default_host "127.0.0.1"
  @project_id_pattern ~r/^[a-z0-9][a-z0-9_-]*$/

  defstruct server: %{host: @default_host, port: nil}, projects: []

  @type server :: %{host: String.t(), port: non_neg_integer() | nil}
  @type t :: %__MODULE__{server: server(), projects: [ProjectContext.t()]}

  @spec load(Path.t()) :: {:ok, t()} | {:error, term()}
  def load(path) when is_binary(path) do
    expanded_path = Path.expand(path)

    with {:ok, content} <- File.read(expanded_path),
         {:ok, decoded} <- decode_yaml(content),
         {:ok, config} <- parse(decoded, Path.dirname(expanded_path)) do
      {:ok, config}
    else
      {:error, reason} when reason in [:enoent, :eacces] ->
        {:error, {:missing_root_config_file, expanded_path, reason}}

      {:error, reason} ->
        {:error, reason}
    end
  end

  @spec parse(map()) :: {:ok, t()} | {:error, term()}
  def parse(config), do: parse(config, File.cwd!())

  @spec parse(map(), Path.t()) :: {:ok, t()} | {:error, term()}
  def parse(config, base_path) when is_map(config) and is_binary(base_path) do
    normalized_config = normalize_keys(config)

    with {:ok, server} <- parse_server(Map.get(normalized_config, "server", %{})),
         {:ok, projects} <- parse_projects(Map.get(normalized_config, "projects", []), base_path) do
      {:ok, %__MODULE__{server: server, projects: projects}}
    end
  end

  def parse(_config, _base_path), do: {:error, :root_config_not_a_map}

  @spec project_by_id(t(), String.t()) :: ProjectContext.t() | nil
  def project_by_id(%__MODULE__{projects: projects}, project_id) when is_binary(project_id) do
    Enum.find(projects, &(&1.project_id == project_id))
  end

  @spec enabled_projects(t()) :: [ProjectContext.t()]
  def enabled_projects(%__MODULE__{projects: projects}) do
    Enum.filter(projects, & &1.enabled)
  end

  defp decode_yaml(content) do
    case YamlElixir.read_from_string(content) do
      {:ok, decoded} when is_map(decoded) -> {:ok, decoded}
      {:ok, _decoded} -> {:error, :root_config_not_a_map}
      {:error, reason} -> {:error, {:root_config_parse_error, reason}}
    end
  end

  defp parse_server(server) when is_map(server) do
    server = normalize_keys(server)
    host = Map.get(server, "host", @default_host)
    port = Map.get(server, "port")

    cond do
      not is_binary(host) or String.trim(host) == "" ->
        {:error, {:invalid_root_config, "server.host must be a non-empty string"}}

      not (is_nil(port) or (is_integer(port) and port >= 0)) ->
        {:error, {:invalid_root_config, "server.port must be a non-negative integer"}}

      true ->
        {:ok, %{host: host, port: port}}
    end
  end

  defp parse_server(_server), do: {:error, {:invalid_root_config, "server must be a map"}}

  defp parse_projects(projects, base_path) when is_list(projects) do
    projects
    |> Enum.with_index(1)
    |> Enum.reduce_while({:ok, [], MapSet.new()}, &parse_project_entry(&1, &2, base_path))
    |> case do
      {:ok, contexts, _seen_ids} -> {:ok, Enum.reverse(contexts)}
      {:error, reason} -> {:error, reason}
    end
  end

  defp parse_projects(_projects, _base_path),
    do: {:error, {:invalid_root_config, "projects must be a list"}}

  defp parse_project_entry({project, index}, {:ok, acc, seen_ids}, base_path) do
    case parse_project(project, base_path, index) do
      {:ok, %ProjectContext{project_id: project_id} = context} ->
        continue_with_unique_project(context, project_id, acc, seen_ids)

      {:error, reason} ->
        {:halt, {:error, reason}}
    end
  end

  defp continue_with_unique_project(context, project_id, acc, seen_ids) do
    if MapSet.member?(seen_ids, project_id) do
      {:halt, {:error, {:invalid_root_config, "projects.id #{inspect(project_id)} must be unique"}}}
    else
      {:cont, {:ok, [context | acc], MapSet.put(seen_ids, project_id)}}
    end
  end

  defp parse_project(project, base_path, index) when is_map(project) do
    project = normalize_keys_deep(project)

    with {:ok, project_id} <- require_project_id(project, index) do
      build_project_context(project, base_path, project_id)
    end
  end

  defp parse_project(_project, _base_path, index),
    do: {:error, {:invalid_root_config, "projects[#{index}] must be a map"}}

  defp require_project_id(project, index) do
    case Map.get(project, "id") do
      id when is_binary(id) ->
        id = String.trim(id)

        if Regex.match?(@project_id_pattern, id) do
          {:ok, id}
        else
          {:error, {:invalid_root_config, "projects[#{index}].id must be lower-case URL-safe text"}}
        end

      _ ->
        {:error, {:invalid_root_config, "projects[#{index}].id is required"}}
    end
  end

  defp require_path(project, field, base_path, project_id) do
    case Map.get(project, field) do
      value when is_binary(value) ->
        value = String.trim(value)

        if value == "" do
          {:error, {:invalid_root_config, "projects #{inspect(project_id)}.#{field} must be a non-empty string"}}
        else
          {:ok, expand_path(value, base_path)}
        end

      _ ->
        {:error, {:invalid_root_config, "projects #{inspect(project_id)}.#{field} is required"}}
    end
  end

  defp build_project_context(project, base_path, project_id) do
    workflow_path = require_path(project, "workflow_path", base_path, project_id)
    enabled = parse_enabled(Map.get(project, "enabled", false), project_id)
    dashboard_order = parse_dashboard_order(Map.get(project, "dashboard_order"), project_id)
    logs_root = parse_optional_path(project, "logs_root", base_path, project_id)
    repo_root = parse_optional_path(project, "repo_root", base_path, project_id)
    app_root = parse_optional_path(project, "app_root", base_path, project_id)
    linear = parse_linear(project, project_id)
    mnemesh = parse_mnemesh(project, project_id)
    runner = parse_optional_map(project, "runner", project_id)
    execution = parse_optional_map(project, "execution", project_id)
    gates = parse_optional_map(project, "gates", project_id)

    validation_errors =
      [
        workflow_path,
        enabled,
        dashboard_order,
        logs_root,
        repo_root,
        app_root,
        linear,
        mnemesh,
        runner,
        execution,
        gates
      ]
      |> Enum.flat_map(&validation_error/1)

    workflow_errors =
      case workflow_path do
        {:ok, path} -> workflow_errors(path)
        {:error, _reason} -> []
      end

    errors = validation_errors ++ workflow_errors
    enabled? = result_value(enabled, false)

    {:ok,
     ProjectContext.new(%{
       id: project_id,
       name: parse_name(Map.get(project, "name"), project_id),
       enabled: enabled?,
       status: project_status(enabled?, errors),
       repo_root: result_value(repo_root),
       app_root: result_value(app_root),
       workflow_path: result_value(workflow_path, ""),
       dashboard_order: result_value(dashboard_order),
       logs_root: result_value(logs_root),
       linear: result_value(linear, %{"team" => %{}, "project" => %{}, "milestone" => %{}}),
       mnemesh: result_value(mnemesh, %{}),
       runner: result_value(runner, %{}),
       execution: Map.merge(%{"enabled" => true}, result_value(execution, %{})),
       gates: Map.merge(%{"dispatch_enabled" => true}, result_value(gates, %{})),
       errors: errors
     })}
  end

  defp validation_error({:error, reason}), do: [reason]
  defp validation_error({:ok, _value}), do: []

  defp result_value(result, default \\ nil)
  defp result_value({:ok, value}, _default), do: value
  defp result_value({:error, _reason}, default), do: default

  defp parse_optional_path(project, field, base_path, project_id) do
    case Map.get(project, field) do
      nil ->
        {:ok, nil}

      value when is_binary(value) ->
        value = String.trim(value)

        if value == "" do
          {:error, {:invalid_root_config, "projects #{inspect(project_id)}.#{field} must be a non-empty string"}}
        else
          {:ok, expand_path(value, base_path)}
        end

      _ ->
        {:error, {:invalid_root_config, "projects #{inspect(project_id)}.#{field} must be a string"}}
    end
  end

  defp parse_enabled(value, _project_id) when is_boolean(value), do: {:ok, value}

  defp parse_enabled(_value, project_id),
    do: {:error, {:invalid_root_config, "projects #{inspect(project_id)}.enabled must be a boolean"}}

  defp parse_dashboard_order(nil, _project_id), do: {:ok, nil}
  defp parse_dashboard_order(value, _project_id) when is_integer(value), do: {:ok, value}

  defp parse_dashboard_order(_value, project_id),
    do: {:error, {:invalid_root_config, "projects #{inspect(project_id)}.dashboard_order must be an integer"}}

  defp parse_linear(project, project_id) do
    with {:ok, linear} <- parse_optional_map(project, "linear", project_id),
         {:ok, team} <- parse_optional_map(linear, "team", project_id, "linear.team"),
         {:ok, linear_project} <- parse_optional_map(linear, "project", project_id, "linear.project"),
         {:ok, milestone} <- parse_optional_map(linear, "milestone", project_id, "linear.milestone") do
      {:ok,
       %{
         "team" => select_keys(team, ["key", "name"]),
         "project" => select_keys(linear_project, ["id", "slug", "name"]),
         "milestone" => select_keys(milestone, ["id", "name"])
       }}
    end
  end

  defp parse_mnemesh(project, project_id) do
    with {:ok, mnemesh} <- parse_optional_map(project, "mnemesh", project_id) do
      {:ok, select_keys(mnemesh, ["workspace_id", "task_id", "subtask_id", "handoff_cursor"])}
    end
  end

  defp parse_optional_map(project, field, project_id, label \\ nil) do
    case Map.get(project, field, %{}) do
      nil ->
        {:ok, %{}}

      value when is_map(value) ->
        {:ok, normalize_keys_deep(value)}

      _ ->
        field_name = label || field
        {:error, {:invalid_root_config, "projects #{inspect(project_id)}.#{field_name} must be a map"}}
    end
  end

  defp parse_name(nil, project_id), do: project_id

  defp parse_name(name, project_id) when is_binary(name) do
    case String.trim(name) do
      "" -> project_id
      trimmed -> trimmed
    end
  end

  defp parse_name(_name, project_id), do: project_id

  defp project_status(true, []), do: :valid
  defp project_status(_enabled, [_ | _]), do: :invalid
  defp project_status(false, []), do: :disabled

  defp workflow_errors(workflow_path) do
    if File.regular?(workflow_path) do
      workflow_file_errors(workflow_path)
    else
      []
    end
  end

  defp workflow_file_errors(workflow_path) do
    case Workflow.load(workflow_path) do
      {:ok, %{config: config}} when is_map(config) -> workflow_config_errors(config)
      {:error, reason} -> [{:workflow_load_error, reason}]
    end
  end

  defp workflow_config_errors(config) do
    case Schema.parse(config) do
      {:ok, _settings} -> []
      {:error, reason} -> [{:invalid_workflow_config, reason}]
    end
  end

  defp select_keys(map, keys) do
    Map.take(map, keys)
  end

  defp expand_path(path, base_path) do
    if Path.type(path) == :absolute do
      Path.expand(path)
    else
      Path.expand(path, base_path)
    end
  end

  defp normalize_keys(map) when is_map(map) do
    Map.new(map, fn {key, value} -> {to_string(key), value} end)
  end

  defp normalize_keys_deep(map) when is_map(map) do
    Map.new(map, fn {key, value} -> {to_string(key), normalize_value(value)} end)
  end

  defp normalize_value(value) when is_map(value), do: normalize_keys_deep(value)
  defp normalize_value(value), do: value
end
