defmodule SymphonyElixir.Workspace do
  @moduledoc """
  Creates isolated per-issue workspaces for parallel Codex agents.
  """

  require Logger
  alias SymphonyElixir.{Config, Linear.Issue, PathSafety, RuntimeCache, SSH}
  alias SymphonyElixir.OpenCode.ACPSessionStore

  @remote_workspace_marker "__SYMPHONY_WORKSPACE__"
  @default_runtime_cache_ttl_ms 7 * 24 * 60 * 60 * 1_000

  @type worker_host :: String.t() | nil

  @spec create_for_issue(map() | String.t() | nil, worker_host(), term()) ::
          {:ok, Path.t()} | {:error, term()}
  def create_for_issue(issue_or_identifier, worker_host \\ nil, settings \\ Config.settings!()) do
    issue_context = issue_context(issue_or_identifier)

    try do
      safe_id = safe_identifier(issue_context.issue_identifier)

      with {:ok, workspace} <- workspace_path_for_issue(safe_id, worker_host, settings),
           :ok <- validate_workspace_path(workspace, worker_host, settings),
           {:ok, workspace, created?} <- ensure_workspace(workspace, worker_host, settings),
           :ok <- maybe_run_after_create_hook(workspace, issue_context, created?, worker_host, settings) do
        {:ok, workspace}
      end
    rescue
      error in [ArgumentError, ErlangError, File.Error] ->
        Logger.error("Workspace creation failed #{issue_log_context(issue_context)} worker_host=#{worker_host_for_log(worker_host)} error=#{Exception.message(error)}")
        {:error, error}
    end
  end

  defp ensure_workspace(workspace, nil, _settings) do
    cond do
      File.dir?(workspace) ->
        {:ok, workspace, false}

      File.exists?(workspace) ->
        File.rm_rf!(workspace)
        create_workspace(workspace)

      true ->
        create_workspace(workspace)
    end
  end

  defp ensure_workspace(workspace, worker_host, settings) when is_binary(worker_host) do
    script =
      [
        "set -eu",
        remote_shell_assign("workspace", workspace),
        "if [ -d \"$workspace\" ]; then",
        "  created=0",
        "elif [ -e \"$workspace\" ]; then",
        "  rm -rf \"$workspace\"",
        "  mkdir -p \"$workspace\"",
        "  created=1",
        "else",
        "  mkdir -p \"$workspace\"",
        "  created=1",
        "fi",
        "cd \"$workspace\"",
        "printf '%s\\t%s\\t%s\\n' '#{@remote_workspace_marker}' \"$created\" \"$(pwd -P)\""
      ]
      |> Enum.reject(&(&1 == ""))
      |> Enum.join("\n")

    case run_remote_command(worker_host, script, settings.hooks.timeout_ms) do
      {:ok, {output, 0}} ->
        parse_remote_workspace_output(output)

      {:ok, {output, status}} ->
        {:error, {:workspace_prepare_failed, worker_host, status, output}}

      {:error, reason} ->
        {:error, reason}
    end
  end

  defp create_workspace(workspace) do
    File.rm_rf!(workspace)
    File.mkdir_p!(workspace)
    {:ok, workspace, true}
  end

  @spec remove(Path.t()) :: {:ok, [String.t()]} | {:error, term(), String.t()}
  def remove(workspace), do: remove(workspace, nil, Config.settings!())

  @spec remove(Path.t(), worker_host()) :: {:ok, [String.t()]} | {:error, term(), String.t()}
  def remove(workspace, nil), do: remove(workspace, nil, Config.settings!())

  def remove(workspace, worker_host) when is_binary(worker_host) do
    remove(workspace, worker_host, Config.settings!())
  end

  @spec remove(Path.t(), nil, term()) :: {:ok, [String.t()]} | {:error, term(), String.t()}
  def remove(workspace, nil, settings) do
    if File.exists?(workspace) do
      case validate_workspace_path(workspace, nil, settings) do
        :ok -> remove_local_workspace(workspace, settings)
        {:error, reason} -> {:error, reason, ""}
      end
    else
      File.rm_rf(workspace)
    end
  end

  @spec remove(Path.t(), worker_host(), term()) :: {:ok, [String.t()]} | {:error, term(), String.t()}
  def remove(workspace, worker_host, settings) when is_binary(worker_host) do
    maybe_run_before_remove_hook(workspace, worker_host, settings)

    script =
      [
        remote_shell_assign("workspace", workspace),
        "rm -rf \"$workspace\""
      ]
      |> Enum.join("\n")

    case run_remote_command(worker_host, script, settings.hooks.timeout_ms) do
      {:ok, {_output, 0}} ->
        {:ok, []}

      {:ok, {output, status}} ->
        {:error, {:workspace_remove_failed, worker_host, status, output}, ""}

      {:error, reason} ->
        {:error, reason, ""}
    end
  end

  defp remove_local_workspace(workspace, settings) do
    case reject_current_working_tree_remove(workspace) do
      :ok ->
        maybe_run_before_remove_hook(workspace, nil, settings)
        File.rm_rf(workspace)

      {:error, reason} ->
        {:error, reason, ""}
    end
  end

  @spec remove_issue_workspaces(term()) :: :ok
  def remove_issue_workspaces(identifier), do: remove_issue_workspaces(identifier, nil)

  @spec remove_issue_workspaces(term(), worker_host()) :: :ok
  def remove_issue_workspaces(identifier, worker_host) when is_binary(identifier) and is_binary(worker_host) do
    safe_id = safe_identifier(identifier)

    case workspace_path_for_issue(safe_id, worker_host, Config.settings!()) do
      {:ok, workspace} -> remove(workspace, worker_host)
      {:error, _reason} -> :ok
    end

    :ok
  end

  def remove_issue_workspaces(identifier, nil) when is_binary(identifier) do
    remove_issue_workspaces(identifier, nil, Config.settings!())
  end

  def remove_issue_workspaces(_identifier, _worker_host) do
    :ok
  end

  @spec remove_issue_workspaces(term(), nil, term()) :: :ok
  def remove_issue_workspaces(identifier, nil, settings) when is_binary(identifier) do
    safe_id = safe_identifier(identifier)

    case settings.worker.ssh_hosts do
      [] ->
        case workspace_path_for_issue(safe_id, nil, settings) do
          {:ok, workspace} -> remove(workspace, nil, settings)
          {:error, _reason} -> :ok
        end

      worker_hosts ->
        Enum.each(worker_hosts, &remove_issue_workspaces(identifier, &1, settings))
    end

    :ok
  end

  @spec remove_issue_workspaces(term(), worker_host(), term()) :: :ok
  def remove_issue_workspaces(identifier, worker_host, settings) when is_binary(identifier) and is_binary(worker_host) do
    safe_id = safe_identifier(identifier)

    case workspace_path_for_issue(safe_id, worker_host, settings) do
      {:ok, workspace} -> remove(workspace, worker_host, settings)
      {:error, _reason} -> :ok
    end

    :ok
  end

  def remove_issue_workspaces(_identifier, _worker_host, _settings) do
    :ok
  end

  @spec cleanup_issue_runtime_cache(Issue.t(), term()) :: :ok | {:error, [{atom(), term()}]}
  def cleanup_issue_runtime_cache(%Issue{} = issue, settings \\ Config.settings!()) do
    errors =
      [
        workspace: remove_issue_workspaces(issue.identifier, nil, settings),
        acp_session_store: ACPSessionStore.remove_issue(issue, settings),
        runtime_cache: RuntimeCache.clear_issue(nil, issue)
      ]
      |> Enum.flat_map(fn
        {_step, :ok} -> []
        {step, {:error, reason}} -> [{step, reason}]
        {step, other} -> [{step, other}]
      end)

    case errors do
      [] ->
        :ok

      errors ->
        Logger.warning("Issue runtime cache cleanup completed with errors issue_identifier=#{inspect(issue.identifier)} errors=#{inspect(errors)}")
        {:error, errors}
    end
  end

  @spec remove_legacy_runtime_cache(term()) :: :ok
  def remove_legacy_runtime_cache(settings \\ Config.settings!()) do
    settings.workspace.root
    |> Path.join("pulse_ledger.json")
    |> File.rm()
    |> case do
      :ok ->
        :ok

      {:error, :enoent} ->
        :ok

      {:error, reason} ->
        Logger.warning("Failed to remove legacy pulse ledger cache path=#{Path.join(settings.workspace.root, ".symphony/pulse_ledger.json")} reason=#{inspect(reason)}")
        :ok
    end
  end

  @spec sweep_abandoned_runtime_cache([String.t()], term(), non_neg_integer()) :: {:ok, [Path.t()]} | {:error, term()}
  def sweep_abandoned_runtime_cache(active_identifiers, settings \\ Config.settings!(), ttl_ms \\ @default_runtime_cache_ttl_ms)
      when is_list(active_identifiers) and is_integer(ttl_ms) and ttl_ms >= 0 do
    active_workspace_names =
      active_identifiers
      |> Enum.filter(&is_binary/1)
      |> Enum.map(&safe_identifier/1)
      |> MapSet.new()

    with {:ok, root} <- PathSafety.canonicalize(settings.workspace.root),
         {:ok, entries} <- list_workspace_entries(root) do
      now_ms = System.system_time(:millisecond)

      removed =
        entries
        |> Enum.filter(&abandoned_runtime_cache_entry?(&1, active_workspace_names, now_ms, ttl_ms))
        |> Enum.flat_map(&remove_abandoned_runtime_cache_entry(&1, settings))

      {:ok, removed}
    end
  end

  @spec run_before_run_hook(Path.t(), map() | String.t() | nil, worker_host(), term()) ::
          :ok | {:error, term()}
  def run_before_run_hook(workspace, issue_or_identifier, worker_host \\ nil, settings \\ Config.settings!())
      when is_binary(workspace) do
    issue_context = issue_context(issue_or_identifier)
    hooks = settings.hooks

    case hooks.before_run do
      nil ->
        :ok

      command ->
        run_hook(command, workspace, issue_context, "before_run", worker_host, settings)
    end
  end

  @spec run_after_run_hook(Path.t(), map() | String.t() | nil, worker_host(), term()) :: :ok
  def run_after_run_hook(workspace, issue_or_identifier, worker_host \\ nil, settings \\ Config.settings!())
      when is_binary(workspace) do
    issue_context = issue_context(issue_or_identifier)
    hooks = settings.hooks

    case hooks.after_run do
      nil ->
        :ok

      command ->
        run_hook(command, workspace, issue_context, "after_run", worker_host, settings)
        |> ignore_hook_failure()
    end
  end

  defp workspace_path_for_issue(safe_id, nil, settings) when is_binary(safe_id) do
    settings.workspace.root
    |> Path.join(safe_id)
    |> PathSafety.canonicalize()
  end

  defp workspace_path_for_issue(safe_id, worker_host, settings) when is_binary(safe_id) and is_binary(worker_host) do
    {:ok, Path.join(settings.workspace.root, safe_id)}
  end

  defp safe_identifier(identifier) do
    String.replace(identifier || "issue", ~r/[^a-zA-Z0-9._-]/, "_")
  end

  defp maybe_run_after_create_hook(workspace, issue_context, created?, worker_host, settings) do
    hooks = settings.hooks

    case created? do
      true ->
        case hooks.after_create do
          nil ->
            :ok

          command ->
            run_hook(command, workspace, issue_context, "after_create", worker_host, settings)
        end

      false ->
        :ok
    end
  end

  defp list_workspace_entries(root) do
    case File.ls(root) do
      {:ok, names} ->
        {:ok, Enum.map(names, &Path.join(root, &1))}

      {:error, :enoent} ->
        {:ok, []}

      {:error, reason} ->
        {:error, {:workspace_runtime_cache_list_failed, reason}}
    end
  end

  defp abandoned_runtime_cache_entry?(path, active_workspace_names, now_ms, ttl_ms) do
    File.dir?(path) and
      Path.basename(path) not in [".symphony"] and
      not MapSet.member?(active_workspace_names, Path.basename(path)) and
      runtime_cache_entry_expired?(path, now_ms, ttl_ms)
  end

  defp runtime_cache_entry_expired?(path, now_ms, ttl_ms) do
    case File.stat(path, time: :posix) do
      {:ok, %{mtime: mtime_seconds}} -> now_ms - mtime_seconds * 1_000 >= ttl_ms
      {:error, _reason} -> false
    end
  end

  defp remove_abandoned_runtime_cache_entry(path, settings) do
    case remove(path, nil, settings) do
      {:ok, _files} ->
        [path]

      {:error, reason, _output} ->
        Logger.warning("Failed to remove abandoned runtime cache path=#{path} reason=#{inspect(reason)}")
        []
    end
  end

  defp maybe_run_before_remove_hook(workspace, nil, settings) do
    hooks = settings.hooks

    case File.dir?(workspace) do
      true ->
        case hooks.before_remove do
          nil ->
            :ok

          command ->
            run_hook(
              command,
              workspace,
              %{issue_id: nil, issue_identifier: Path.basename(workspace)},
              "before_remove",
              nil,
              settings
            )
            |> ignore_hook_failure()
        end

      false ->
        :ok
    end
  end

  defp maybe_run_before_remove_hook(workspace, worker_host, settings) when is_binary(worker_host) do
    hooks = settings.hooks

    case hooks.before_remove do
      nil ->
        :ok

      command ->
        script =
          [
            remote_shell_assign("workspace", workspace),
            "if [ -d \"$workspace\" ]; then",
            "  cd \"$workspace\"",
            "  #{command}",
            "fi"
          ]
          |> Enum.join("\n")

        run_remote_command(worker_host, script, settings.hooks.timeout_ms)
        |> case do
          {:ok, {output, status}} ->
            handle_hook_command_result(
              {output, status},
              workspace,
              %{issue_id: nil, issue_identifier: Path.basename(workspace)},
              "before_remove"
            )

          {:error, {:workspace_hook_timeout, "before_remove", _timeout_ms} = reason} ->
            {:error, reason}

          {:error, reason} ->
            {:error, reason}
        end
        |> ignore_hook_failure()
    end
  end

  defp ignore_hook_failure(:ok), do: :ok
  defp ignore_hook_failure({:error, _reason}), do: :ok

  defp run_hook(command, workspace, issue_context, hook_name, nil, settings) do
    timeout_ms = settings.hooks.timeout_ms

    Logger.info("Running workspace hook hook=#{hook_name} #{issue_log_context(issue_context)} workspace=#{workspace} worker_host=local")

    task =
      Task.async(fn ->
        System.cmd("sh", ["-lc", command], cd: workspace, stderr_to_stdout: true)
      end)

    case Task.yield(task, timeout_ms) do
      {:ok, cmd_result} ->
        handle_hook_command_result(cmd_result, workspace, issue_context, hook_name)

      nil ->
        Task.shutdown(task, :brutal_kill)

        Logger.warning("Workspace hook timed out hook=#{hook_name} #{issue_log_context(issue_context)} workspace=#{workspace} worker_host=local timeout_ms=#{timeout_ms}")

        {:error, {:workspace_hook_timeout, hook_name, timeout_ms}}
    end
  end

  defp run_hook(command, workspace, issue_context, hook_name, worker_host, settings) when is_binary(worker_host) do
    timeout_ms = settings.hooks.timeout_ms

    Logger.info("Running workspace hook hook=#{hook_name} #{issue_log_context(issue_context)} workspace=#{workspace} worker_host=#{worker_host}")

    case run_remote_command(worker_host, "cd #{shell_escape(workspace)} && #{command}", timeout_ms) do
      {:ok, cmd_result} ->
        handle_hook_command_result(cmd_result, workspace, issue_context, hook_name)

      {:error, {:workspace_hook_timeout, ^hook_name, _timeout_ms} = reason} ->
        {:error, reason}

      {:error, reason} ->
        {:error, reason}
    end
  end

  defp handle_hook_command_result({_output, 0}, _workspace, _issue_id, _hook_name) do
    :ok
  end

  defp handle_hook_command_result({output, status}, workspace, issue_context, hook_name) do
    sanitized_output = sanitize_hook_output_for_log(output)

    Logger.warning("Workspace hook failed hook=#{hook_name} #{issue_log_context(issue_context)} workspace=#{workspace} status=#{status} output=#{inspect(sanitized_output)}")

    {:error, {:workspace_hook_failed, hook_name, status, output}}
  end

  defp sanitize_hook_output_for_log(output, max_bytes \\ 2_048) do
    binary_output = IO.iodata_to_binary(output)

    case byte_size(binary_output) <= max_bytes do
      true ->
        binary_output

      false ->
        binary_part(binary_output, 0, max_bytes) <> "... (truncated)"
    end
  end

  defp validate_workspace_path(workspace, nil, settings) when is_binary(workspace) do
    expanded_workspace = Path.expand(workspace)
    expanded_root = Path.expand(settings.workspace.root)
    expanded_root_prefix = expanded_root <> "/"

    with {:ok, canonical_workspace} <- PathSafety.canonicalize(expanded_workspace),
         {:ok, canonical_root} <- PathSafety.canonicalize(expanded_root) do
      canonical_root_prefix = canonical_root <> "/"

      cond do
        canonical_workspace == canonical_root ->
          {:error, {:workspace_equals_root, canonical_workspace, canonical_root}}

        String.starts_with?(canonical_workspace <> "/", canonical_root_prefix) ->
          :ok

        String.starts_with?(expanded_workspace <> "/", expanded_root_prefix) ->
          {:error, {:workspace_symlink_escape, expanded_workspace, canonical_root}}

        true ->
          {:error, {:workspace_outside_root, canonical_workspace, canonical_root}}
      end
    else
      {:error, {:path_canonicalize_failed, path, reason}} ->
        {:error, {:workspace_path_unreadable, path, reason}}
    end
  end

  defp validate_workspace_path(workspace, worker_host, _settings)
       when is_binary(workspace) and is_binary(worker_host) do
    cond do
      String.trim(workspace) == "" ->
        {:error, {:workspace_path_unreadable, workspace, :empty}}

      String.contains?(workspace, ["\n", "\r", <<0>>]) ->
        {:error, {:workspace_path_unreadable, workspace, :invalid_characters}}

      true ->
        :ok
    end
  end

  defp reject_current_working_tree_remove(workspace) when is_binary(workspace) do
    with {:ok, canonical_workspace} <- PathSafety.canonicalize(workspace),
         {:ok, cwd} <- File.cwd(),
         {:ok, canonical_cwd} <- PathSafety.canonicalize(cwd) do
      if same_or_child_path?(canonical_cwd, canonical_workspace) do
        {:error, {:workspace_contains_current_working_directory, canonical_workspace, canonical_cwd}}
      else
        :ok
      end
    else
      {:error, reason} -> {:error, reason}
    end
  end

  defp same_or_child_path?(path, root) do
    path == root or String.starts_with?(path, root <> "/")
  end

  defp remote_shell_assign(variable_name, raw_path)
       when is_binary(variable_name) and is_binary(raw_path) do
    [
      "#{variable_name}=#{shell_escape(raw_path)}",
      "case \"$#{variable_name}\" in",
      "  '~') #{variable_name}=\"$HOME\" ;;",
      "  '~/'*) " <> variable_name <> "=\"$HOME/${" <> variable_name <> "#~/}\" ;;",
      "esac"
    ]
    |> Enum.join("\n")
  end

  defp parse_remote_workspace_output(output) do
    lines = String.split(IO.iodata_to_binary(output), "\n", trim: true)

    payload =
      Enum.find_value(lines, fn line ->
        case String.split(line, "\t", parts: 3) do
          [@remote_workspace_marker, created, path] when created in ["0", "1"] and path != "" ->
            {created == "1", path}

          _ ->
            nil
        end
      end)

    case payload do
      {created?, workspace} when is_boolean(created?) and is_binary(workspace) ->
        {:ok, workspace, created?}

      _ ->
        {:error, {:workspace_prepare_failed, :invalid_output, output}}
    end
  end

  defp run_remote_command(worker_host, script, timeout_ms)
       when is_binary(worker_host) and is_binary(script) and is_integer(timeout_ms) and timeout_ms > 0 do
    task =
      Task.async(fn ->
        SSH.run(worker_host, script, stderr_to_stdout: true)
      end)

    case Task.yield(task, timeout_ms) do
      {:ok, result} ->
        result

      nil ->
        Task.shutdown(task, :brutal_kill)
        {:error, {:workspace_hook_timeout, "remote_command", timeout_ms}}
    end
  end

  defp shell_escape(value) when is_binary(value) do
    "'" <> String.replace(value, "'", "'\"'\"'") <> "'"
  end

  defp worker_host_for_log(nil), do: "local"
  defp worker_host_for_log(worker_host), do: worker_host

  defp issue_context(%{id: issue_id, identifier: identifier}) do
    %{
      issue_id: issue_id,
      issue_identifier: identifier || "issue"
    }
  end

  defp issue_context(identifier) when is_binary(identifier) do
    %{
      issue_id: nil,
      issue_identifier: identifier
    }
  end

  defp issue_context(_identifier) do
    %{
      issue_id: nil,
      issue_identifier: "issue"
    }
  end

  defp issue_log_context(%{issue_id: issue_id, issue_identifier: issue_identifier}) do
    "issue_id=#{issue_id || "n/a"} issue_identifier=#{issue_identifier || "issue"}"
  end
end
