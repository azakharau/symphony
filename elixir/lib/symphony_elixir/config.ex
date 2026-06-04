defmodule SymphonyElixir.Config do
  @moduledoc """
  Runtime configuration loaded from `WORKFLOW.md`.
  """

  alias SymphonyElixir.Config.Schema
  alias SymphonyElixir.ProjectContext
  alias SymphonyElixir.ProjectRegistry
  alias SymphonyElixir.Workflow
  alias SymphonyElixir.WorkflowStore

  @default_prompt_template """
  Linear issue execution request.

  Identifier: {{ issue.identifier }}
  Title: {{ issue.title }}

  Body:
  {% if issue.description %}
  {{ issue.description }}
  {% else %}
  No description provided.
  {% endif %}

  Comments:
  {% for comment in issue.comments %}
  - {% if comment.created_at %}{{ comment.created_at }} {% endif %}{% if comment.author %}{{ comment.author }}: {% endif %}{{ comment.body }}
  {% else %}
  No comments provided.
  {% endfor %}
  """

  @type codex_runtime_settings :: %{
          approval_policy: String.t() | map(),
          project_root: String.t() | nil,
          thread_id: String.t() | nil,
          thread_sandbox: String.t(),
          turn_sandbox_policy: map()
        }

  @spec settings() :: {:ok, Schema.t()} | {:error, term()}
  def settings do
    case Workflow.current() do
      {:ok, %{config: config}} when is_map(config) ->
        Schema.parse(config)

      {:error, reason} ->
        {:error, reason}
    end
  end

  @spec settings(ProjectContext.t() | GenServer.server() | nil) :: {:ok, Schema.t()} | {:error, term()}
  def settings(nil), do: settings()

  def settings(%ProjectContext{} = context) do
    context.process_names.workflow_store
    |> ProjectRegistry.via_name()
    |> settings()
  end

  def settings(workflow_store_name) do
    case WorkflowStore.current(workflow_store_name) do
      {:ok, %{config: config}} when is_map(config) ->
        Schema.parse(config)

      {:error, reason} ->
        {:error, reason}
    end
  end

  @spec settings!() :: Schema.t()
  def settings! do
    case settings() do
      {:ok, settings} ->
        settings

      {:error, reason} ->
        raise ArgumentError, message: format_config_error(reason)
    end
  end

  @spec settings!(ProjectContext.t() | GenServer.server() | nil) :: Schema.t()
  def settings!(nil), do: settings!()

  def settings!(context_or_store) do
    case settings(context_or_store) do
      {:ok, settings} -> settings
      {:error, reason} -> raise ArgumentError, message: format_config_error(reason)
    end
  end

  @spec max_concurrent_agents_for_state(term()) :: pos_integer()
  def max_concurrent_agents_for_state(state_name) when is_binary(state_name) do
    config = settings!()

    Map.get(
      config.agent.max_concurrent_agents_by_state,
      Schema.normalize_issue_state(state_name),
      config.agent.max_concurrent_agents
    )
  end

  def max_concurrent_agents_for_state(_state_name), do: settings!().agent.max_concurrent_agents

  @spec max_concurrent_agents_for_state(term(), ProjectContext.t() | GenServer.server() | nil) :: pos_integer()
  def max_concurrent_agents_for_state(state_name, context_or_store) when is_binary(state_name) do
    config = settings!(context_or_store)

    Map.get(
      config.agent.max_concurrent_agents_by_state,
      Schema.normalize_issue_state(state_name),
      config.agent.max_concurrent_agents
    )
  end

  def max_concurrent_agents_for_state(_state_name, context_or_store) do
    settings!(context_or_store).agent.max_concurrent_agents
  end

  @spec codex_turn_sandbox_policy(Path.t() | nil) :: map()
  def codex_turn_sandbox_policy(workspace \\ nil) do
    case Schema.resolve_runtime_turn_sandbox_policy(settings!(), workspace) do
      {:ok, policy} ->
        policy

      {:error, reason} ->
        raise ArgumentError, message: "Invalid codex turn sandbox policy: #{inspect(reason)}"
    end
  end

  @spec workflow_prompt() :: String.t()
  def workflow_prompt do
    case Workflow.current() do
      {:ok, %{prompt_template: prompt}} ->
        if String.trim(prompt) == "", do: @default_prompt_template, else: prompt

      _ ->
        @default_prompt_template
    end
  end

  @spec workflow_prompt(ProjectContext.t() | GenServer.server() | nil) :: String.t()
  def workflow_prompt(nil), do: workflow_prompt()

  def workflow_prompt(%ProjectContext{} = context) do
    context.process_names.workflow_store
    |> ProjectRegistry.via_name()
    |> workflow_prompt()
  end

  def workflow_prompt(workflow_store_name) do
    case WorkflowStore.current(workflow_store_name) do
      {:ok, %{prompt_template: prompt}} ->
        if String.trim(prompt) == "", do: @default_prompt_template, else: prompt

      _ ->
        @default_prompt_template
    end
  end

  @spec server_port() :: non_neg_integer() | nil
  def server_port do
    case Application.get_env(:symphony_elixir, :server_port_override) do
      port when is_integer(port) and port >= 0 -> port
      _ -> settings!().server.port
    end
  end

  @spec validate!() :: :ok | {:error, term()}
  def validate! do
    with {:ok, settings} <- settings() do
      validate_semantics(settings)
    end
  end

  @spec codex_runtime_settings(Path.t() | nil, keyword()) ::
          {:ok, codex_runtime_settings()} | {:error, term()}
  def codex_runtime_settings(workspace \\ nil, opts \\ []) do
    settings = Keyword.get(opts, :settings)

    with {:ok, settings} <- normalize_runtime_settings(settings),
         {:ok, turn_sandbox_policy} <-
           Schema.resolve_runtime_turn_sandbox_policy(settings, workspace, opts) do
      {:ok,
       %{
         approval_policy: settings.codex.approval_policy,
         project_root: settings.codex.project_root,
         thread_id: settings.codex.thread_id,
         thread_sandbox: settings.codex.thread_sandbox,
         turn_sandbox_policy: turn_sandbox_policy
       }}
    end
  end

  defp normalize_runtime_settings(%Schema{} = settings), do: {:ok, settings}
  defp normalize_runtime_settings(nil), do: settings()
  defp normalize_runtime_settings(other), do: {:error, {:invalid_runtime_settings, other}}

  defp validate_semantics(settings) do
    cond do
      is_nil(settings.tracker.kind) ->
        {:error, :missing_tracker_kind}

      settings.tracker.kind not in ["linear", "memory"] ->
        {:error, {:unsupported_tracker_kind, settings.tracker.kind}}

      settings.tracker.kind == "linear" and not is_binary(settings.tracker.api_key) ->
        {:error, :missing_linear_api_token}

      settings.tracker.kind == "linear" and not is_binary(settings.tracker.project_slug) ->
        {:error, :missing_linear_project_slug}

      true ->
        :ok
    end
  end

  defp format_config_error(reason) do
    case reason do
      {:invalid_workflow_config, message} ->
        "Invalid WORKFLOW.md config: #{message}"

      {:missing_workflow_file, path, raw_reason} ->
        "Missing WORKFLOW.md at #{path}: #{inspect(raw_reason)}"

      {:workflow_parse_error, raw_reason} ->
        "Failed to parse WORKFLOW.md: #{inspect(raw_reason)}"

      :workflow_front_matter_not_a_map ->
        "Failed to parse WORKFLOW.md: workflow front matter must decode to a map"

      other ->
        "Invalid WORKFLOW.md config: #{inspect(other)}"
    end
  end
end
