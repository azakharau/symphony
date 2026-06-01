defmodule SymphonyElixir.RootConfig do
  @moduledoc """
  Passive parser for root `projects.yml` multiproject configuration.
  """

  alias SymphonyElixir.ProjectContext
  alias SymphonyElixir.Workflow
  alias SymphonyElixir.Config.Schema

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
    |> Enum.reduce_while({:ok, [], MapSet.new()}, fn {project, index}, {:ok, acc, seen_ids} ->
      case parse_project(project, base_path, index) do
        {:ok, %ProjectContext{project_id: project_id} = context} ->
          if MapSet.member?(seen_ids, project_id) do
            {:halt, {:error, {:invalid_root_config, "projects.id #{inspect(project_id)} must be unique"}}}
          else
            {:cont, {:ok, [context | acc], MapSet.put(seen_ids, project_id)}}
          end

        {:error, reason} ->
          {:halt, {:error, reason}}
      end
    end)
    |> case do
      {:ok, contexts, _seen_ids} -> {:ok, Enum.reverse(contexts)}
      {:error, reason} -> {:error, reason}
    end
  end

  defp parse_projects(_projects, _base_path),
    do: {:error, {:invalid_root_config, "projects must be a list"}}

  defp parse_project(project, base_path, index) when is_map(project) do
    project = normalize_keys_deep(project)

    with {:ok, project_id} <- require_project_id(project, index),
         {:ok, workflow_path} <- require_path(project, "workflow_path", base_path, project_id),
         {:ok, enabled} <- parse_enabled(Map.get(project, "enabled", false), project_id),
         {:ok, dashboard_order} <- parse_dashboard_order(Map.get(project, "dashboard_order"), project_id),
         {:ok, logs_root} <- parse_optional_path(project, "logs_root", base_path, project_id),
         {:ok, repo_root} <- parse_optional_path(project, "repo_root", base_path, project_id),
         {:ok, app_root} <- parse_optional_path(project, "app_root", base_path, project_id),
         {:ok, linear} <- parse_linear(project, project_id),
         {:ok, mnemesh} <- parse_mnemesh(project, project_id),
         {:ok, runner} <- parse_optional_map(project, "runner", project_id),
         {:ok, execution} <- parse_optional_map(project, "execution", project_id),
         {:ok, gates} <- parse_optional_map(project, "gates", project_id) do
      errors = workflow_errors(workflow_path)

      {:ok,
       ProjectContext.new(%{
         id: project_id,
         name: parse_name(Map.get(project, "name"), project_id),
         enabled: enabled,
         status: project_status(enabled, errors),
         repo_root: repo_root,
         app_root: app_root,
         workflow_path: workflow_path,
         dashboard_order: dashboard_order,
         logs_root: logs_root,
         linear: linear,
         mnemesh: mnemesh,
         runner: runner,
         execution: Map.merge(%{"enabled" => true}, execution),
         gates: Map.merge(%{"dispatch_enabled" => true}, gates),
         errors: errors
       })}
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

  defp project_status(false, _errors), do: :disabled
  defp project_status(true, []), do: :valid
  defp project_status(true, _errors), do: :invalid

  defp workflow_errors(workflow_path) do
    if File.regular?(workflow_path) do
      case Workflow.load(workflow_path) do
        {:ok, %{config: config}} when is_map(config) ->
          case Schema.parse(config) do
            {:ok, _settings} -> []
            {:error, reason} -> [{:invalid_workflow_config, reason}]
          end

        {:error, reason} ->
          [{:workflow_load_error, reason}]
      end
    else
      []
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
