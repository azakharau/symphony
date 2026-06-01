defmodule SymphonyElixir.CLI do
  @moduledoc """
  Escript entrypoint for running Symphony with an explicit WORKFLOW.md path.
  """

  alias SymphonyElixir.LogFile

  @acknowledgement_switch :i_understand_that_this_will_be_running_without_the_usual_guardrails
  @switches [
    {@acknowledgement_switch, :boolean},
    logs_root: :string,
    port: :integer,
    projects_config: :string
  ]

  @type ensure_started_result :: {:ok, [atom()]} | {:error, term()}
  @type deps :: %{
          file_regular?: (String.t() -> boolean()),
          set_workflow_file_path: (String.t() -> :ok | {:error, term()}),
          set_logs_root: (String.t() -> :ok | {:error, term()}),
          set_server_port_override: (non_neg_integer() | nil -> :ok | {:error, term()}),
          set_root_config_path: (String.t() -> :ok | {:error, term()}),
          load_root_config: (String.t() -> {:ok, SymphonyElixir.RootConfig.t()} | {:error, term()}),
          ensure_root_started: (SymphonyElixir.RootConfig.t() -> ensure_started_result()),
          ensure_all_started: (-> ensure_started_result())
        }

  @spec main([String.t()]) :: no_return()
  def main(args) do
    case evaluate(args) do
      :ok ->
        wait_for_shutdown()

      {:error, message} ->
        IO.puts(:stderr, message)
        System.halt(1)
    end
  end

  @spec evaluate([String.t()], deps()) :: :ok | {:error, String.t()}
  def evaluate(args, deps \\ runtime_deps()) do
    case OptionParser.parse(args, strict: @switches) do
      {opts, [], []} ->
        evaluate_default_or_projects_config(opts, deps)

      {opts, [workflow_path], []} ->
        evaluate_workflow_path(opts, workflow_path, deps)

      _ ->
        {:error, usage_message()}
    end
  end

  @spec run(String.t(), deps()) :: :ok | {:error, String.t()}
  def run(workflow_path, deps) do
    expanded_path = Path.expand(workflow_path)

    if deps.file_regular?.(expanded_path) do
      :ok = deps.set_workflow_file_path.(expanded_path)

      case deps.ensure_all_started.() do
        {:ok, _started_apps} ->
          :ok

        {:error, reason} ->
          {:error, "Failed to start Symphony with workflow #{expanded_path}: #{inspect(reason)}"}
      end
    else
      {:error, "Workflow file not found: #{expanded_path}"}
    end
  end

  @spec run_projects_config(String.t(), deps()) :: :ok | {:error, String.t()}
  def run_projects_config(projects_config_path, deps) do
    expanded_path = Path.expand(projects_config_path)

    if deps.file_regular?.(expanded_path) do
      case load_root_config(deps, expanded_path) do
        {:ok, root_config} -> start_projects_config(deps, root_config, expanded_path)
        {:error, reason} -> {:error, "Invalid projects config #{expanded_path}: #{inspect(reason)}"}
      end
    else
      {:error, "Projects config file not found: #{expanded_path}"}
    end
  end

  @spec usage_message() :: String.t()
  defp usage_message do
    "Usage: symphony [--logs-root <path>] [--port <port>] [--projects-config <path-to-projects.yml> | path-to-WORKFLOW.md]"
  end

  defp evaluate_default_or_projects_config(opts, deps) do
    with :ok <- require_guardrails_acknowledgement(opts),
         :ok <- maybe_set_logs_root(opts, deps),
         :ok <- maybe_set_server_port(opts, deps) do
      case Keyword.get(opts, :projects_config) do
        nil -> run(Path.expand("WORKFLOW.md"), deps)
        projects_config_path -> run_projects_config(projects_config_path, deps)
      end
    end
  end

  defp evaluate_workflow_path(opts, workflow_path, deps) do
    with :ok <- require_guardrails_acknowledgement(opts),
         :ok <- maybe_set_logs_root(opts, deps),
         :ok <- maybe_set_server_port(opts, deps) do
      if Keyword.has_key?(opts, :projects_config) do
        {:error, usage_message()}
      else
        run(workflow_path, deps)
      end
    end
  end

  defp start_projects_config(deps, root_config, expanded_path) do
    with :ok <- set_root_config_path(deps, expanded_path),
         {:ok, _started_apps} <- ensure_root_started(deps, root_config) do
      :ok
    else
      {:error, reason} ->
        {:error, "Failed to start Symphony with projects config #{expanded_path}: #{inspect(reason)}"}
    end
  end

  @spec runtime_deps() :: deps()
  defp runtime_deps do
    %{
      file_regular?: &File.regular?/1,
      set_workflow_file_path: &SymphonyElixir.Workflow.set_workflow_file_path/1,
      set_logs_root: &set_logs_root/1,
      set_server_port_override: &set_server_port_override/1,
      set_root_config_path: &set_root_config_path/1,
      load_root_config: &SymphonyElixir.RootConfig.load/1,
      ensure_root_started: fn _root_config -> Application.ensure_all_started(:symphony_elixir) end,
      ensure_all_started: fn -> Application.ensure_all_started(:symphony_elixir) end
    }
  end

  defp set_root_config_path(deps, expanded_path) do
    Map.get(deps, :set_root_config_path, &set_root_config_path/1).(expanded_path)
  end

  defp set_root_config_path(expanded_path) do
    Application.put_env(:symphony_elixir, :root_config_path, expanded_path)
    :ok
  end

  defp load_root_config(deps, expanded_path) do
    Map.get(deps, :load_root_config, &SymphonyElixir.RootConfig.load/1).(expanded_path)
  end

  defp ensure_root_started(deps, root_config) do
    Map.get(deps, :ensure_root_started, fn _root_config -> Application.ensure_all_started(:symphony_elixir) end).(root_config)
  end

  defp maybe_set_logs_root(opts, deps) do
    case Keyword.get_values(opts, :logs_root) do
      [] ->
        :ok

      values ->
        logs_root = values |> List.last() |> String.trim()

        if logs_root == "" do
          {:error, usage_message()}
        else
          :ok = deps.set_logs_root.(Path.expand(logs_root))
        end
    end
  end

  defp require_guardrails_acknowledgement(opts) do
    if Keyword.get(opts, @acknowledgement_switch, false) do
      :ok
    else
      {:error, acknowledgement_banner()}
    end
  end

  @spec acknowledgement_banner() :: String.t()
  defp acknowledgement_banner do
    lines = [
      "This Symphony implementation is a low key engineering preview.",
      "Codex will run without any guardrails.",
      "SymphonyElixir is not a supported product and is presented as-is.",
      "To proceed, start with `--i-understand-that-this-will-be-running-without-the-usual-guardrails` CLI argument"
    ]

    width = Enum.max(Enum.map(lines, &String.length/1))
    border = String.duplicate("─", width + 2)
    top = "╭" <> border <> "╮"
    bottom = "╰" <> border <> "╯"
    spacer = "│ " <> String.duplicate(" ", width) <> " │"

    content =
      [
        top,
        spacer
        | Enum.map(lines, fn line ->
            "│ " <> String.pad_trailing(line, width) <> " │"
          end)
      ] ++ [spacer, bottom]

    [
      IO.ANSI.red(),
      IO.ANSI.bright(),
      Enum.join(content, "\n"),
      IO.ANSI.reset()
    ]
    |> IO.iodata_to_binary()
  end

  defp set_logs_root(logs_root) do
    Application.put_env(:symphony_elixir, :log_file, LogFile.default_log_file(logs_root))
    :ok
  end

  defp maybe_set_server_port(opts, deps) do
    case Keyword.get_values(opts, :port) do
      [] ->
        :ok

      values ->
        port = List.last(values)

        if is_integer(port) and port >= 0 do
          :ok = deps.set_server_port_override.(port)
        else
          {:error, usage_message()}
        end
    end
  end

  defp set_server_port_override(port) when is_integer(port) and port >= 0 do
    Application.put_env(:symphony_elixir, :server_port_override, port)
    :ok
  end

  @spec wait_for_shutdown() :: no_return()
  defp wait_for_shutdown do
    case Process.whereis(SymphonyElixir.Supervisor) do
      nil ->
        IO.puts(:stderr, "Symphony supervisor is not running")
        System.halt(1)

      pid ->
        ref = Process.monitor(pid)

        receive do
          {:DOWN, ^ref, :process, ^pid, reason} ->
            case reason do
              :normal -> System.halt(0)
              _ -> System.halt(1)
            end
        end
    end
  end
end
