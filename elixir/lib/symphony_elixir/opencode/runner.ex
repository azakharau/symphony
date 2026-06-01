defmodule SymphonyElixir.OpenCode.Runner do
  @moduledoc """
  Runs OpenCode as a first-class Symphony runner for implementation states.
  """

  require Logger

  alias SymphonyElixir.{Config, Linear.Issue}
  alias SymphonyElixir.OpenCode.ACPRunner
  alias SymphonyElixir.OpenCode.TaskPrompt

  @max_handoff_bytes 20_000

  @type result :: %{
          output: String.t(),
          command: [String.t()],
          project_root: Path.t(),
          session_id: String.t() | nil,
          attach_url: String.t() | nil,
          runner_owner: String.t()
        }

  @spec run(Path.t(), Issue.t(), TaskPrompt.Packet.t() | String.t(), keyword()) ::
          {:ok, result()} | {:error, term()}
  def run(workspace, %Issue{} = issue, prompt_or_packet, opts \\ [])
      when is_binary(workspace) do
    packet = normalize_task_packet(prompt_or_packet)
    opencode = Config.settings!().opencode
    command = Keyword.get(opts, :command, opencode.command)
    runner = Keyword.get(opts, :runner, &System.cmd/3)
    execution_dir = opencode_project_root(opencode.project_root, workspace)
    on_event = Keyword.get(opts, :on_event, fn _event -> :ok end)
    worker_host = Keyword.get(opts, :worker_host)

    with :ok <- ensure_local_worker_host(worker_host, workspace, execution_dir) do
      context = %{
        workspace: workspace,
        issue: issue,
        packet: packet,
        opts: opts,
        opencode: opencode,
        command: command,
        runner: runner,
        execution_dir: execution_dir,
        on_event: on_event
      }

      case opencode.protocol do
        "acp" -> run_local_acp_opencode(context)
        _protocol -> run_local_opencode(context)
      end
    end
  rescue
    error in [ErlangError, RuntimeError, ArgumentError, File.Error] ->
      reason = {:opencode_failed, Exception.message(error)}

      Keyword.get(opts, :on_event, fn _event -> :ok end).(%{
        event: :failed,
        phase: :failed,
        timestamp: DateTime.utc_now(),
        failure: failure_classification(reason)
      })

      {:error, reason}
  end

  defp run_local_opencode(%{issue: issue, packet: packet, opencode: opencode} = context) do
    title = packet_title(issue, packet)
    session_id = existing_session_id(context.command, context.execution_dir, title, opencode.server_url, context.opts)

    args =
      [
        "run",
        "--dir",
        context.execution_dir,
        "--agent",
        opencode.agent,
        "--format",
        opencode.format,
        "--title",
        title
      ]
      |> maybe_continue_session(session_id)
      |> maybe_attach(opencode.server_url)

    context = Map.merge(context, %{args: args, session_id: session_id, title: title})

    emit_command_prepared(context)
    log_opencode_start(context)

    task = Task.async(fn -> execute_opencode_task(context) end)
    handle_opencode_task_result(Task.yield(task, opencode.timeout_ms) || Task.shutdown(task, :brutal_kill), context)
  end

  defp run_local_acp_opencode(%{issue: issue, packet: packet, opencode: opencode} = context) do
    args = Keyword.get(context.opts, :args, opencode.args || ["acp"])
    title = packet_title(issue, packet)

    context.on_event.(%{
      event: :command_prepared,
      phase: :command,
      timestamp: DateTime.utc_now(),
      runner_owner: "opencode",
      project_root: context.execution_dir,
      workspace_path: context.workspace,
      command: [context.command | args],
      session_id: nil,
      attach_url: opencode.server_url,
      title: title,
      protocol: "acp"
    })

    Logger.info([
      "Starting OpenCode ACP runner for issue_id=#{issue.id} issue_identifier=#{issue.identifier} ",
      "workspace=#{context.workspace} execution_dir=#{context.execution_dir} attach=#{opencode.server_url || "none"}"
    ])

    case ACPRunner.run(
           context.workspace,
           issue,
           packet.prompt,
           Keyword.merge(context.opts,
             command: context.command,
             args: args,
             cwd: context.execution_dir,
             title: title,
             on_event: context.on_event
           )
         ) do
      {:ok, result} ->
        result =
          result
          |> Map.put_new(:command, [context.command | args])
          |> Map.put_new(:project_root, context.execution_dir)
          |> Map.put_new(:session_id, nil)
          |> Map.put_new(:attach_url, opencode.server_url)
          |> Map.put_new(:runner_owner, "opencode")

        emit_completed_event(result, context.on_event)
        {:ok, result}

      {:error, reason} ->
        context.on_event.(%{
          event: :failed,
          phase: :failed,
          timestamp: DateTime.utc_now(),
          failure: failure_classification(reason)
        })

        {:error, reason}
    end
  end

  defp emit_command_prepared(%{} = context) do
    context.on_event.(%{
      event: :command_prepared,
      phase: :command,
      timestamp: DateTime.utc_now(),
      runner_owner: "opencode",
      project_root: context.execution_dir,
      workspace_path: context.workspace,
      command: [context.command | context.args],
      session_id: context.session_id,
      attach_url: context.opencode.server_url
    })
  end

  defp log_opencode_start(%{issue: issue, opencode: opencode} = context) do
    session_suffix = if is_binary(context.session_id), do: " existing_session=#{context.session_id}", else: ""

    Logger.info([
      "Starting OpenCode runner for issue_id=#{issue.id} issue_identifier=#{issue.identifier} ",
      "workspace=#{context.workspace} execution_dir=#{context.execution_dir} ",
      "attach=#{opencode.server_url || "none"}#{session_suffix}"
    ])
  end

  defp execute_opencode_task(%{session_id: session_id} = context) when is_binary(session_id) and session_id != "" do
    session_result_reader = Keyword.get(context.opts, :session_result_reader, &read_completed_session_result/2)

    case session_result_reader.(context.execution_dir, session_id) do
      {:error, {:opencode_session_handoff_incomplete, _session_id}} ->
        continue_incomplete_opencode_session(context)

      {:error, {:opencode_session_not_completed, _session_id}} ->
        continue_incomplete_opencode_session(context)

      other ->
        other
    end
  rescue
    error in [ErlangError, RuntimeError, ArgumentError, File.Error] ->
      {:error, {:opencode_failed, Exception.message(error)}}
  end

  defp execute_opencode_task(%{} = context) do
    run_opencode_command(
      context.runner,
      context.command,
      context.args,
      context.packet.prompt,
      context.execution_dir,
      context.session_id
    )
  end

  defp continue_incomplete_opencode_session(%{} = context) do
    run_opencode_command(
      context.runner,
      context.command,
      context.args,
      incomplete_session_continuation_prompt(),
      context.execution_dir,
      context.session_id
    )
  end

  defp handle_opencode_task_result(task_result, %{opencode: opencode} = context) do
    case task_result do
      nil ->
        context.on_event.(%{
          event: :timeout,
          phase: :failed,
          timestamp: DateTime.utc_now(),
          failure: %{reason: :timeout, timeout_ms: opencode.timeout_ms}
        })

        {:error, {:opencode_timeout, opencode.timeout_ms}}

      {:exit, reason} ->
        context.on_event.(%{
          event: :failed,
          phase: :failed,
          timestamp: DateTime.utc_now(),
          failure: failure_classification({:opencode_failed, reason})
        })

        {:error, {:opencode_failed, reason}}

      {:ok, {:ok, output}} ->
        result =
          opencode_result(
            output,
            [context.command, "session", "resume-result", context.session_id],
            context.execution_dir,
            context.session_id,
            opencode.server_url
          )

        emit_completed_event(result, context.on_event)
        {:ok, result}

      {:ok, {:error, reason}} ->
        context.on_event.(%{
          event: :failed,
          phase: :failed,
          timestamp: DateTime.utc_now(),
          failure: failure_classification(reason)
        })

        {:error, reason}

      {:ok, {output, 0}} ->
        result_session_id =
          result_session_id(
            output,
            context.session_id,
            context.command,
            context.execution_dir,
            context.title,
            opencode.server_url,
            context.opts
          )

        result = opencode_result(output, [context.command | context.args], context.execution_dir, result_session_id, opencode.server_url)
        emit_completed_event(result, context.on_event)
        {:ok, result}

      {:ok, {output, status}} ->
        context.on_event.(%{
          event: :exit_failed,
          phase: :failed,
          timestamp: DateTime.utc_now(),
          failure: %{reason: :opencode_exit, status: status}
        })

        {:error, {:opencode_exit, status, trim_output(output)}}
    end
  end

  defp emit_completed_event(result, on_event) do
    on_event.(Map.merge(result_event(result), %{event: :completed, phase: :completed, timestamp: DateTime.utc_now()}))
  end

  defp ensure_local_worker_host(worker_host, _workspace, _execution_dir)
       when worker_host in [nil, "", "localhost", "127.0.0.1", "::1"],
       do: :ok

  defp ensure_local_worker_host(worker_host, workspace, execution_dir) when is_binary(worker_host) do
    {:error,
     {:opencode_remote_worker_host_unsupported,
      %{
        worker_host: worker_host,
        workspace: workspace,
        project_root: execution_dir
      }}}
  end

  defp ensure_local_worker_host(worker_host, workspace, execution_dir) do
    ensure_local_worker_host(to_string(worker_host), workspace, execution_dir)
  end

  defp failure_classification(reason) when is_atom(reason), do: %{reason: reason}

  defp failure_classification({reason, status, _output}) when is_atom(reason) and is_integer(status) do
    %{reason: reason, status: status}
  end

  defp failure_classification({reason, timeout_ms}) when is_atom(reason) and is_integer(timeout_ms) do
    %{reason: reason, timeout_ms: timeout_ms}
  end

  defp failure_classification({reason, _detail}) when is_atom(reason), do: %{reason: reason}
  defp failure_classification({reason, _detail, _extra}) when is_atom(reason), do: %{reason: reason}
  defp failure_classification(_reason), do: %{reason: :opencode_failed}

  defp opencode_result(output, command, project_root, session_id, attach_url) do
    %{
      output: output,
      command: command,
      project_root: project_root,
      session_id: session_id,
      attach_url: attach_url,
      runner_owner: "opencode"
    }
  end

  defp normalize_task_packet(%TaskPrompt.Packet{} = packet), do: packet

  defp normalize_task_packet(prompt) when is_binary(prompt) do
    %TaskPrompt.Packet{prompt: prompt, slice_id: "legacy", fingerprint: fingerprint(prompt)}
  end

  defp packet_title(%Issue{} = issue, %TaskPrompt.Packet{} = packet) do
    if packet.slice_id == "legacy" do
      issue_title(issue)
    else
      "#{issue_title(issue)} [#{TaskPrompt.title_suffix(packet)}]"
    end
  end

  defp fingerprint(prompt) do
    :crypto.hash(:sha256, prompt)
    |> Base.encode16(case: :lower)
  end

  defp result_event(result) when is_map(result) do
    %{
      runner_owner: Map.get(result, :runner_owner),
      project_root: Map.get(result, :project_root),
      command: Map.get(result, :command),
      session_id: Map.get(result, :session_id),
      attach_url: Map.get(result, :attach_url)
    }
  end

  defp maybe_attach(args, nil), do: args

  defp maybe_attach(args, server_url) when is_binary(server_url) do
    case String.trim(server_url) do
      "" -> args
      url -> args ++ ["--attach", url]
    end
  end

  defp maybe_continue_session(args, session_id) when is_binary(session_id) and session_id != "" do
    args ++ ["--session", session_id]
  end

  defp maybe_continue_session(args, _session_id), do: args

  defp existing_session_id(command, execution_dir, title, server_url, opts) do
    if session_reuse_enabled?(server_url, opts) do
      session_lister = Keyword.get(opts, :session_lister, &list_sessions/3)

      case session_lister.(command, execution_dir, title) do
        {:ok, sessions} ->
          newest_matching_session_id(sessions, execution_dir, title)

        {:error, reason} ->
          Logger.debug("Unable to inspect OpenCode sessions for title=#{inspect(title)} dir=#{execution_dir}: #{inspect(reason)}")

          nil
      end
    end
  end

  defp session_reuse_enabled?(server_url, opts) do
    explicit_test_lister? = Keyword.has_key?(opts, :session_lister)
    fake_runner_without_lister? = Keyword.has_key?(opts, :runner) and not explicit_test_lister?

    explicit_test_lister? or
      (attached_server?(server_url) and not fake_runner_without_lister?)
  end

  defp attached_server?(server_url) when is_binary(server_url), do: String.trim(server_url) != ""
  defp attached_server?(_server_url), do: false

  defp list_sessions(command, execution_dir, _title) do
    case System.cmd(command, ["session", "list", "--format", "json", "--max-count", "50"],
           cd: execution_dir,
           stderr_to_stdout: true
         ) do
      {output, 0} ->
        case Jason.decode(output) do
          {:ok, sessions} when is_list(sessions) -> {:ok, sessions}
          {:ok, _other} -> {:error, :unexpected_session_list_payload}
          {:error, reason} -> {:error, {:invalid_session_list_json, Exception.message(reason)}}
        end

      {output, status} ->
        {:error, {:session_list_exit, status, trim_output(output)}}
    end
  rescue
    error in [ErlangError, RuntimeError, ArgumentError, File.Error] ->
      {:error, {:session_list_failed, Exception.message(error)}}
  end

  defp newest_matching_session_id(sessions, execution_dir, title) when is_list(sessions) do
    sessions
    |> Enum.filter(&matching_session?(&1, execution_dir, title))
    |> Enum.sort_by(&session_updated_sort_key/1, :desc)
    |> List.first()
    |> case do
      %{"id" => session_id} when is_binary(session_id) and session_id != "" -> session_id
      _ -> nil
    end
  end

  defp matching_session?(%{"title" => session_title, "directory" => directory}, execution_dir, title) do
    directory == execution_dir and session_title == title
  end

  defp matching_session?(_session, _execution_dir, _title), do: false

  defp session_updated_sort_key(%{"updated" => updated}) when is_integer(updated), do: updated
  defp session_updated_sort_key(%{"created" => created}) when is_integer(created), do: created
  defp session_updated_sort_key(_session), do: 0

  defp result_session_id(_output, session_id, _command, _execution_dir, _title, _server_url, _opts)
       when is_binary(session_id) and session_id != "",
       do: session_id

  defp result_session_id(output, _session_id, command, execution_dir, title, server_url, opts) do
    extracted_session_id(output) || rediscovered_session_id(command, execution_dir, title, server_url, opts)
  end

  defp extracted_session_id(output) when is_binary(output) do
    output
    |> String.split(~r/\R/, trim: true)
    |> Enum.find_value(fn line ->
      case Jason.decode(line) do
        {:ok, decoded} -> session_id_from_payload(decoded)
        _ -> nil
      end
    end)
  end

  defp extracted_session_id(_output), do: nil

  defp session_id_from_payload(%{"session_id" => session_id}) when is_binary(session_id) and session_id != "", do: session_id
  defp session_id_from_payload(%{"sessionID" => session_id}) when is_binary(session_id) and session_id != "", do: session_id
  defp session_id_from_payload(%{"session" => %{"id" => session_id}}) when is_binary(session_id) and session_id != "", do: session_id
  defp session_id_from_payload(%{"result" => payload}) when is_map(payload), do: session_id_from_payload(payload)
  defp session_id_from_payload(_payload), do: nil

  defp rediscovered_session_id(command, execution_dir, title, server_url, opts) do
    if session_reuse_enabled?(server_url, opts) do
      session_lister = Keyword.get(opts, :session_lister, &list_sessions/3)

      case session_lister.(command, execution_dir, title) do
        {:ok, sessions} ->
          newest_matching_session_id(sessions, execution_dir, title)

        {:error, reason} ->
          Logger.debug("Unable to rediscover OpenCode session for title=#{inspect(title)} dir=#{execution_dir}: #{inspect(reason)}")
          nil
      end
    end
  end

  defp opencode_project_root(project_root, _workspace) when is_binary(project_root) and project_root != "" do
    project_root
  end

  defp opencode_project_root(_project_root, workspace), do: workspace

  defp read_completed_session_result(execution_dir, session_id) do
    db_path = opencode_db_path()

    with true <- File.exists?(db_path) || {:error, {:opencode_db_not_found, db_path}},
         {:ok, session} <- read_session_row(db_path, session_id),
         :ok <- ensure_session_directory(session, execution_dir),
         {:ok, text} <- read_latest_completed_assistant_text(db_path, session_id),
         :ok <- ensure_completed_handoff_text(text, session_id) do
      {:ok, format_existing_session_output(session, text)}
    end
  end

  defp opencode_db_path do
    data_home =
      case System.get_env("XDG_DATA_HOME") do
        value when is_binary(value) and value != "" -> value
        _ -> Path.join(System.user_home!(), ".local/share")
      end

    Path.join([data_home, "opencode", "opencode.db"])
  end

  defp read_session_row(db_path, session_id) do
    sql =
      """
      select id,title,directory,time_created,time_updated,summary_files,summary_additions,summary_deletions,tokens_input,tokens_output
      from session
      where id = #{sql_string(session_id)}
      limit 1;
      """

    case sqlite_json(db_path, sql) do
      {:ok, [session | _]} -> {:ok, session}
      {:ok, []} -> {:error, {:opencode_session_not_found, session_id}}
      {:error, reason} -> {:error, reason}
    end
  end

  defp ensure_session_directory(%{"directory" => execution_dir}, execution_dir), do: :ok

  defp ensure_session_directory(%{"directory" => directory}, execution_dir) do
    {:error, {:opencode_session_directory_mismatch, directory, execution_dir}}
  end

  defp read_latest_completed_assistant_text(db_path, session_id) do
    sql =
      """
      with latest as (
        select id
        from message
        where session_id = #{sql_string(session_id)}
          and json_extract(data, '$.role') = 'assistant'
          and json_extract(data, '$.finish') = 'stop'
        order by time_created desc
        limit 1
      )
      select p.data
      from part p
      join latest on p.message_id = latest.id
      where json_extract(p.data, '$.type') = 'text'
      order by p.time_created asc;
      """

    case sqlite_json(db_path, sql) do
      {:ok, rows} ->
        text =
          rows
          |> Enum.map(&decode_part_text/1)
          |> Enum.reject(&(&1 == ""))
          |> Enum.join("\n\n")

        if String.trim(text) == "" do
          {:error, {:opencode_session_not_completed, session_id}}
        else
          {:ok, text}
        end

      {:error, reason} ->
        {:error, reason}
    end
  end

  defp sqlite_json(db_path, sql) do
    case System.cmd("sqlite3", ["-json", db_path, sql], stderr_to_stdout: true) do
      {output, 0} ->
        case String.trim(output) do
          "" ->
            {:ok, []}

          output ->
            decode_sqlite_json(output)
        end

      {output, status} ->
        {:error, {:opencode_sqlite_failed, status, trim_output(output)}}
    end
  end

  defp decode_sqlite_json(output) do
    case Jason.decode(output) do
      {:ok, rows} when is_list(rows) ->
        {:ok, rows}

      {:ok, other} ->
        {:error, {:opencode_sqlite_unexpected_payload, other}}

      {:error, reason} ->
        {:error, {:opencode_sqlite_json_decode_failed, Exception.message(reason), trim_output(output)}}
    end
  end

  defp decode_part_text(%{"data" => data}) when is_binary(data) do
    case Jason.decode(data) do
      {:ok, %{"text" => text}} when is_binary(text) -> text
      _ -> ""
    end
  end

  defp ensure_completed_handoff_text(text, session_id) when is_binary(text) do
    if incomplete_handoff_text?(text) do
      {:error, {:opencode_session_handoff_incomplete, session_id}}
    else
      :ok
    end
  end

  defp incomplete_handoff_text?(text) when is_binary(text) do
    incomplete_section_present?(text, "In Progress") or incomplete_section_present?(text, "Blocked") or
      owner_input_requested?(text)
  end

  defp owner_input_requested?(text) when is_binary(text) do
    normalized = String.downcase(text)

    Enum.any?(
      [
        "нужно уточнение",
        "требуется уточнение",
        "нужен ответ",
        "нужна авторизация",
        "нужно подтверждение",
        "можно откатить",
        "есть блокер перед финальным",
        "owner input",
        "need owner input",
        "needs owner input",
        "needs clarification",
        "requires clarification",
        "needs confirmation",
        "requires confirmation",
        "needs authorization",
        "requires authorization",
        "blocked before final",
        "evaluator returned `revise`",
        "evaluator returned revise",
        "evaluator вернул `revise`",
        "evaluator вернул revise"
      ],
      &String.contains?(normalized, &1)
    )
  end

  defp incomplete_section_present?(text, section_title) do
    case Regex.run(
           ~r/(?ims)^###\s+#{Regex.escape(section_title)}\s*$(.*?)(?:^###\s+|^##\s+|\z)/,
           text,
           capture: :all_but_first
         ) do
      [body | _] -> substantive_section_body?(body)
      _ -> false
    end
  end

  defp substantive_section_body?(body) do
    body
    |> String.split("\n")
    |> Enum.map(&String.trim/1)
    |> Enum.reject(&non_substantive_section_line?/1)
    |> Enum.any?()
  end

  defp non_substantive_section_line?(line) do
    line == "" or line in ["- `(none)`", "- (none)", "(none)", "`(none)`", "none", "None"]
  end

  defp incomplete_session_continuation_prompt do
    """
    Continue the existing OpenCode task from the current session context.

    Do not restate or replay the original task prompt. Use the session history as authority.
    The previous assistant handoff reported remaining In Progress/Blocked work or asked for owner clarification, so the task is not ready for Symphony handoff yet.

    If the previous handoff asked whether to edit, revert, stage, commit, push, tag, release, or otherwise mutate a forbidden/out-of-scope file, do not do that. Leave unrelated dirty files untouched, report them as unrelated, and complete only the scoped OpenCode task.

    Finish the remaining work, run the required validation, then return a final handoff only when there is no remaining In Progress or Blocked section except `(none)` and no unresolved question for the owner/controller.
    Confirm nothing was staged, committed, pushed, tagged, or released.
    """
  end

  defp format_existing_session_output(session, text) do
    """
    Resumed completed OpenCode session from local OpenCode state.

    Session: #{Map.get(session, "id")}
    Title: #{Map.get(session, "title")}
    Directory: #{Map.get(session, "directory")}
    Summary: #{Map.get(session, "summary_files") || 0} files, +#{Map.get(session, "summary_additions") || 0} -#{Map.get(session, "summary_deletions") || 0}
    Tokens: input #{Map.get(session, "tokens_input") || 0}, output #{Map.get(session, "tokens_output") || 0}

    Latest assistant handoff:

    #{text}
    """
  end

  defp sql_string(value) when is_binary(value) do
    "'" <> String.replace(value, "'", "''") <> "'"
  end

  defp run_opencode_command(runner, command, args, prompt, execution_dir, _session_id) do
    prompt_dir = opencode_prompt_dir(execution_dir)

    prompt_path =
      Path.join(
        prompt_dir,
        "symphony-opencode-prompt-#{System.unique_integer([:positive, :monotonic])}.md"
      )

    File.mkdir_p!(prompt_dir)
    File.write!(prompt_path, prompt)

    try do
      runner.(
        "bash",
        [
          "-lc",
          "prompt_file=$1; shift; exec \"$@\" < \"$prompt_file\"",
          "symphony-opencode",
          prompt_path,
          command | args
        ],
        cd: execution_dir,
        stderr_to_stdout: true
      )
    rescue
      error in [ErlangError, RuntimeError, ArgumentError, File.Error] ->
        {:error, {:opencode_failed, Exception.message(error)}}
    after
      File.rm(prompt_path)
    end
  end

  defp opencode_prompt_dir(execution_dir) do
    execution_dir
    |> prompt_dir_candidates()
    |> Enum.find(&(not temp_path?(&1)))
    |> case do
      nil -> user_state_prompt_dir()
      prompt_dir -> prompt_dir
    end
  end

  defp prompt_dir_candidates(execution_dir) do
    [
      scoped_prompt_dir(execution_dir),
      scoped_prompt_dir(Config.settings!().opencode.project_root),
      user_state_prompt_dir()
    ]
    |> Enum.reject(&is_nil/1)
  end

  defp scoped_prompt_dir(path) when is_binary(path) and path != "",
    do: Path.join([path, ".symphony", "opencode-prompts"])

  defp scoped_prompt_dir(_path), do: nil

  defp user_state_prompt_dir do
    state_home =
      case System.get_env("XDG_STATE_HOME") do
        value when is_binary(value) and value != "" -> value
        _ -> Path.join(System.user_home!(), ".local/state")
      end

    Path.join([state_home, "symphony", "opencode-prompts"])
  end

  defp temp_path?(path) do
    expanded_path = Path.expand(path)

    [System.tmp_dir!(), "/tmp"]
    |> Enum.map(&Path.expand/1)
    |> Enum.uniq()
    |> Enum.any?(fn tmp_root ->
      expanded_path == tmp_root or String.starts_with?(expanded_path, tmp_root <> "/")
    end)
  end

  @spec handoff_comment(Issue.t(), map()) :: String.t()
  def handoff_comment(%Issue{} = issue, %{output: output, command: command} = result) do
    project_root = Map.get(result, :project_root)

    """
    ## OpenCode Handoff

    Issue: #{issue.identifier}
    Runner: OpenCode
    Status: completed
    Project root: #{project_root || "(unknown)"}

    Command:

    ```text
    #{Enum.map_join(command, " ", &shellish/1)}
    ```

    Output:

    ```text
    #{trim_output(output)}
    ```
    """
  end

  defp issue_title(%Issue{identifier: identifier, title: title}) do
    [identifier, title]
    |> Enum.filter(&(is_binary(&1) and String.trim(&1) != ""))
    |> Enum.join(" ")
  end

  defp trim_output(output) when is_binary(output) do
    if byte_size(output) > @max_handoff_bytes do
      binary_part(output, 0, @max_handoff_bytes) <> "\n\n[truncated]"
    else
      output
    end
  end

  defp trim_output(output), do: inspect(output)

  defp shellish(value) when is_binary(value) do
    if String.match?(value, ~r|^[A-Za-z0-9_@%+=:,./-]+$|) do
      value
    else
      "'" <> String.replace(value, "'", "'\"'\"'") <> "'"
    end
  end
end
