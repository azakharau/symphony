defmodule SymphonyElixir.OpenCode.Runner do
  @moduledoc """
  Runs OpenCode as a first-class Symphony runner for implementation states.
  """

  require Logger

  alias SymphonyElixir.{Config, Linear.Issue}

  @max_handoff_bytes 20_000

  @spec run(Path.t(), Issue.t(), String.t(), keyword()) ::
          {:ok, %{output: String.t(), command: [String.t()]}} | {:error, term()}
  def run(workspace, %Issue{} = issue, prompt, opts \\ [])
      when is_binary(workspace) and is_binary(prompt) do
    opencode = Config.settings!().opencode

    if opencode.protocol == "acp" do
      SymphonyElixir.OpenCode.ACPRunner.run(workspace, issue, prompt, opts)
    else
      run_cli(workspace, issue, prompt, opts, opencode)
    end
  end

  defp run_cli(workspace, %Issue{} = issue, prompt, opts, opencode)
       when is_binary(workspace) and is_binary(prompt) do
    command = Keyword.get(opts, :command, opencode.command)
    runner = Keyword.get(opts, :runner, &System.cmd/3)
    execution_dir = opencode_project_root(opencode.project_root, workspace)

    title = issue_title(issue)
    session_id = existing_session_id(command, execution_dir, title, opencode.server_url, opts)

    args =
      [
        "run",
        "--dir",
        execution_dir,
        "--agent",
        opencode.agent,
        "--format",
        opencode.format,
        "--title",
        title
      ]
      |> maybe_continue_session(session_id)
      |> maybe_attach(opencode.server_url)

    session_suffix = if is_binary(session_id), do: " existing_session=#{session_id}", else: ""

    Logger.info(
      "Starting OpenCode runner for issue_id=#{issue.id} issue_identifier=#{issue.identifier} workspace=#{workspace} execution_dir=#{execution_dir} attach=#{opencode.server_url || "none"}#{session_suffix}"
    )

    task =
      Task.async(fn ->
        if is_binary(session_id) and session_id != "" do
          session_result_reader =
            Keyword.get(opts, :session_result_reader, &read_completed_session_result/2)

          case session_result_reader.(execution_dir, session_id) do
            {:error, {:opencode_session_handoff_incomplete, _session_id}} ->
              run_opencode_command(
                runner,
                command,
                args,
                incomplete_session_continuation_prompt(),
                execution_dir,
                session_id
              )

            {:error, {:opencode_session_not_completed, _session_id}} ->
              run_opencode_command(
                runner,
                command,
                args,
                incomplete_session_continuation_prompt(),
                execution_dir,
                session_id
              )

            other ->
              other
          end
        else
          run_opencode_command(runner, command, args, prompt, execution_dir, session_id)
        end
      end)

    case Task.yield(task, opencode.timeout_ms) || Task.shutdown(task, :brutal_kill) do
      nil ->
        {:error, {:opencode_timeout, opencode.timeout_ms}}

      {:exit, reason} ->
        {:error, {:opencode_failed, reason}}

      {:ok, {:ok, output}} ->
        {:ok, %{output: output, command: [command, "session", "resume-result", session_id]}}

      {:ok, {:error, reason}} ->
        {:error, reason}

      {:ok, {output, 0}} ->
        {:ok, %{output: output, command: [command | args]}}

      {:ok, {output, status}} ->
        {:error, {:opencode_exit, status, trim_output(output)}}
    end
  rescue
    error in [ErlangError, RuntimeError, ArgumentError, File.Error] ->
      {:error, {:opencode_failed, Exception.message(error)}}
  end

  defp maybe_attach(args, nil), do: args

  defp maybe_attach(args, server_url) when is_binary(server_url) do
    case String.trim(server_url) do
      "" -> args
      url -> args ++ ["--attach", url]
    end
  end

  defp maybe_attach(args, _server_url), do: args

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

  defp matching_session?(
         %{"title" => session_title, "directory" => directory},
         execution_dir,
         issue_title
       ) do
    directory == execution_dir and
      (session_title == issue_title or same_issue_identifier?(session_title, issue_title))
  end

  defp matching_session?(_session, _execution_dir, _title), do: false

  defp same_issue_identifier?(session_title, issue_title)
       when is_binary(session_title) and is_binary(issue_title) do
    with {:ok, issue_identifier} <- issue_identifier_prefix(issue_title) do
      String.starts_with?(session_title, issue_identifier <> " ")
    else
      _ -> false
    end
  end

  defp same_issue_identifier?(_session_title, _issue_title), do: false

  defp issue_identifier_prefix(title) when is_binary(title) do
    case Regex.run(~r/^[A-Z][A-Z0-9]*-\d+/, title) do
      [identifier] -> {:ok, identifier}
      _ -> :error
    end
  end

  defp session_updated_sort_key(%{"updated" => updated}) when is_integer(updated), do: updated
  defp session_updated_sort_key(%{"created" => created}) when is_integer(created), do: created
  defp session_updated_sort_key(_session), do: 0

  defp opencode_project_root(project_root, _workspace)
       when is_binary(project_root) and project_root != "" do
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
  rescue
    error in [ErlangError, RuntimeError, ArgumentError, File.Error] ->
      {:error, {:opencode_sqlite_failed, Exception.message(error)}}
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

  defp decode_part_text(_row), do: ""

  defp ensure_completed_handoff_text(text, session_id) when is_binary(text) do
    if incomplete_handoff_text?(text) do
      {:error, {:opencode_session_handoff_incomplete, session_id}}
    else
      :ok
    end
  end

  defp incomplete_handoff_text?(text) when is_binary(text) do
    incomplete_section_present?(text, "In Progress") or
      incomplete_section_present?(text, "Blocked") or
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
    |> Enum.reject(&(&1 == ""))
    |> Enum.reject(&(&1 in ["- `(none)`", "- (none)", "(none)", "`(none)`", "none", "None"]))
    |> Enum.any?()
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
    prompt_path =
      Path.join(
        System.tmp_dir!(),
        "symphony-opencode-prompt-#{System.unique_integer([:positive, :monotonic])}.md"
      )

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
    after
      File.rm(prompt_path)
    end
  end

  @spec handoff_comment(Issue.t(), map()) :: String.t()
  def handoff_comment(%Issue{} = issue, %{output: output, command: command}) do
    """
    ## OpenCode Handoff

    Issue: #{issue.identifier}
    Runner: OpenCode
    Status: completed

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
