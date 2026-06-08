defmodule SymphonyElixir.OpenCode.ACPRunner do
  @moduledoc """
  Runs OpenCode through ACP while preserving Symphony runner result shape.
  """

  alias SymphonyElixir.{Config, Linear.Issue, PathSafety}
  alias SymphonyElixir.OpenCode.{ACPClient, ACPSessionStore, Runner}

  @spec run(Path.t(), Issue.t(), String.t(), keyword()) ::
          {:ok, Runner.result()} | {:error, term()}
  def run(workspace, %Issue{} = issue, prompt, opts \\ [])
      when is_binary(workspace) and is_binary(prompt) do
    settings = settings_from_opts(opts)
    opencode = settings.opencode
    command = Keyword.get(opts, :command, opencode.command)
    args = Keyword.get(opts, :args, opencode.args || ["acp"])
    full_command = [command | args]
    client_module = Keyword.get(opts, :client, ACPClient)
    session_store = Keyword.get(opts, :session_store, ACPSessionStore)

    session_result_reader =
      Keyword.get(opts, :session_result_reader, &Runner.read_completed_session_result/2)

    session_usage_reader = Keyword.get(opts, :session_usage_reader, &Runner.read_session_usage/2)
    session_scope = session_store.prompt_scope(prompt)
    cwd = Keyword.get(opts, :cwd, opencode_project_root(opencode.project_root, workspace))
    title = Keyword.get(opts, :title, issue_title(issue))
    on_event = Keyword.get(opts, :on_event, fn _event -> :ok end)

    with {:ok, cwd} <- safe_cwd(cwd),
         {:ok, client} <-
           client_module.start_link(
             command: command,
             args: args,
             cwd: cwd,
             handler: self(),
             permission_policy: opencode.permission_policy,
             read_timeout_ms: opencode.read_timeout_ms
           ) do
      try do
        with {:ok, _initialized} <-
               client_module.initialize(
                 client,
                 initialize_params(opencode),
                 opencode.read_timeout_ms
               ),
             {:ok, existing_session_id} <-
               fetch_stored_session_id(session_store, issue, cwd, session_scope, settings),
             {:ok, session_result} <-
               open_session(
                 client_module,
                 client,
                 existing_session_id,
                 issue,
                 cwd,
                 title,
                 opencode
               ) do
          session_id = session_id(session_result) || existing_session_id

          with :ok <- configure_session(client_module, client, session_id, opencode) do
            if resumed_session?(session_result) do
              case completed_session_result(
                     session_result_reader,
                     cwd,
                     session_id,
                     full_command,
                     opencode
                   ) do
                {:ok, result} ->
                  {:ok, result}

                {:error, _reason} ->
                  emit_session_started(on_event, session_id, full_command, cwd, opencode)

                  run_prompt(
                    prompt_context(
                      client_module: client_module,
                      client: client,
                      session_id: session_id,
                      opencode: opencode,
                      command: full_command,
                      cwd: cwd,
                      on_event: on_event,
                      session_result_reader: session_result_reader,
                      session_usage_reader: session_usage_reader
                    ),
                    prompt
                  )
              end
            else
              with :ok <-
                     persist_new_session_id(
                       session_store,
                       issue,
                       cwd,
                       session_id,
                       session_scope,
                       settings
                     ) do
                emit_session_started(on_event, session_id, full_command, cwd, opencode)

                run_prompt(
                  prompt_context(
                    client_module: client_module,
                    client: client,
                    session_id: session_id,
                    opencode: opencode,
                    command: full_command,
                    cwd: cwd,
                    on_event: on_event,
                    session_result_reader: session_result_reader,
                    session_usage_reader: session_usage_reader
                  ),
                  prompt
                )
              end
            end
          end
        end
      after
        if function_exported?(client_module, :stop, 1), do: client_module.stop(client)
      end
    end
  rescue
    error in [ErlangError, RuntimeError, ArgumentError, File.Error] ->
      {:error, {:opencode_acp_failed, Exception.message(error)}}
  end

  @spec handoff_comment(Issue.t(), map()) :: String.t()
  def handoff_comment(%Issue{} = issue, %{output: output, command: command}) do
    Runner.handoff_comment(issue, %{output: output, command: command})
  end

  defp open_session(client_module, client, existing_session_id, issue, cwd, title, opencode) do
    params = session_params(issue, cwd, title, opencode)

    case existing_session_id do
      session_id when is_binary(session_id) and session_id != "" ->
        cond do
          client_module.capability?(client, "session/resume") ->
            client_module.resume_session(
              client,
              Map.put(params, "sessionId", session_id),
              opencode.read_timeout_ms
            )
            |> tag_resumed()

          client_module.capability?(client, "session/load") ->
            client_module.load_session(
              client,
              Map.put(params, "sessionId", session_id),
              opencode.read_timeout_ms
            )
            |> tag_resumed()

          true ->
            {:error, {:unsupported_acp_resume, session_id}}
        end

      _missing ->
        client_module.new_session(client, params, opencode.read_timeout_ms)
    end
  end

  defp tag_resumed({:ok, result}) when is_map(result),
    do: {:ok, Map.put(result, "symphony_resumed", true)}

  defp tag_resumed(other), do: other

  defp configure_session(_client_module, _client, nil, _opencode),
    do: {:error, :opencode_session_id_missing}

  defp configure_session(_client_module, _client, "", _opencode),
    do: {:error, :opencode_session_id_missing}

  defp configure_session(client_module, client, session_id, opencode) do
    with :ok <-
           set_session_config_option(client_module, client, session_id, "mode", opencode.agent, opencode),
         :ok <-
           set_session_config_option(client_module, client, session_id, "model", opencode.model, opencode) do
      :ok
    end
  end

  defp set_session_config_option(_client_module, _client, _session_id, _config_id, nil, _opencode),
    do: :ok

  defp set_session_config_option(_client_module, _client, _session_id, _config_id, "", _opencode),
    do: :ok

  defp set_session_config_option(client_module, client, session_id, config_id, value, opencode) do
    if function_exported?(client_module, :set_config_option, 3) do
      case client_module.set_config_option(
             client,
             %{"sessionId" => session_id, "configId" => config_id, "value" => value},
             opencode.read_timeout_ms
           ) do
        {:ok, _result} ->
          :ok

        {:error, reason} ->
          if unsupported_config_option_reason?(reason) do
            :ok
          else
            {:error, {:opencode_acp_config_option_failed, config_id, reason}}
          end
      end
    else
      :ok
    end
  end

  defp unsupported_config_option_reason?({:unsupported_acp_method, "session/set_config_option"}), do: true

  defp unsupported_config_option_reason?(%{"code" => -32601}), do: true

  defp unsupported_config_option_reason?(%{"message" => message}) when is_binary(message) do
    message
    |> String.downcase()
    |> String.contains?("method not found")
  end

  defp unsupported_config_option_reason?(_reason), do: false

  defp prompt_context(attrs) when is_list(attrs) do
    %{
      client_module: Keyword.fetch!(attrs, :client_module),
      client: Keyword.fetch!(attrs, :client),
      session_id: Keyword.fetch!(attrs, :session_id),
      opencode: Keyword.fetch!(attrs, :opencode),
      command: Keyword.fetch!(attrs, :command),
      cwd: Keyword.fetch!(attrs, :cwd),
      on_event: Keyword.fetch!(attrs, :on_event),
      session_result_reader: Keyword.fetch!(attrs, :session_result_reader),
      session_usage_reader: Keyword.fetch!(attrs, :session_usage_reader)
    }
  end

  defp run_prompt(context, prompt) do
    task =
      Task.async(fn ->
        context.client_module.prompt(
          context.client,
          %{
            "sessionId" => context.session_id,
            "prompt" => [
              %{
                "type" => "text",
                "text" => prompt
              }
            ]
          },
          context.opencode.timeout_ms
        )
      end)

    collect_prompt_result(task, context, [], false)
  end

  defp collect_prompt_result(task, context, events, end_turn?) do
    task_ref = task.ref

    receive do
      {^task_ref, {:ok, result}} ->
        demonitor_task(task)

        cond do
          user_input_required?(events) ->
            {:error, {:need_owner_input, events}}

          end_turn? or end_turn_result?(result) ->
            case completed_session_result(
                   context.session_result_reader,
                   context.cwd,
                   context.session_id,
                   context.command,
                   context.opencode
                 ) do
              {:ok, result} ->
                {:ok, result}

              {:error, _reason} ->
                {:ok,
                 %{
                   output: format_output(events, result),
                   command: context.command,
                   project_root: context.cwd,
                   session_id: context.session_id,
                   attach_url: context.opencode.server_url,
                   runner_owner: "opencode"
                 }}
            end

          true ->
            {:error, {:opencode_acp_incomplete, result}}
        end

      {^task_ref, {:error, :timeout}} ->
        demonitor_task(task)
        maybe_cancel(context.client_module, context.client, context.session_id, context.opencode)
        {:error, {:opencode_timeout, context.opencode.timeout_ms}}

      {^task_ref, {:error, reason}} ->
        demonitor_task(task)
        {:error, reason}

      {:DOWN, ^task_ref, :process, _pid, reason} ->
        {:error, {:opencode_acp_prompt_failed, reason}}

      {:acp_notification, method, params} ->
        emit_acp_update(context, :notification, method, params)

        collect_prompt_result(
          task,
          context,
          [{method, params} | events],
          end_turn? or end_turn_event?(method, params)
        )

      {:acp_request, method, params} ->
        emit_acp_update(context, :request, method, params)

        collect_prompt_result(task, context, [{method, params} | events], end_turn?)
    after
      stall_timeout(context.opencode.stall_timeout_ms) ->
        maybe_cancel(context.client_module, context.client, context.session_id, context.opencode)
        Task.shutdown(task, :brutal_kill)
        {:error, {:opencode_acp_stalled, context.opencode.stall_timeout_ms}}
    end
  end

  defp stall_timeout(0), do: :infinity
  defp stall_timeout(timeout_ms), do: timeout_ms

  @dialyzer {:no_opaque, demonitor_task: 1}
  defp demonitor_task(%Task{ref: ref}) do
    Process.demonitor(ref, [:flush])
  end

  defp completed_session_result(session_result_reader, cwd, session_id, command, opencode)
       when is_function(session_result_reader, 2) and is_binary(session_id) and session_id != "" do
    case session_result_reader.(cwd, session_id) do
      {:ok, output} when is_binary(output) ->
        {:ok,
         %{
           output: output,
           command: command,
           project_root: cwd,
           session_id: session_id,
           attach_url: opencode.server_url,
           runner_owner: "opencode"
         }}

      {:error, reason} ->
        {:error, reason}

      other ->
        {:error, {:opencode_session_result_reader_unexpected, other}}
    end
  rescue
    error in [ErlangError, RuntimeError, ArgumentError, File.Error] ->
      {:error, {:opencode_session_result_reader_failed, Exception.message(error)}}
  end

  defp completed_session_result(_session_result_reader, _cwd, _session_id, _command, _opencode),
    do: {:error, :opencode_session_id_missing}

  defp maybe_cancel(client_module, client, session_id, opencode) do
    if client_module.capability?(client, "session/cancel") do
      client_module.cancel(client, %{"sessionId" => session_id}, opencode.read_timeout_ms)
    else
      {:error, {:unsupported_acp_method, "session/cancel"}}
    end
  catch
    _kind, reason -> {:error, reason}
  end

  defp emit_session_started(on_event, session_id, command, cwd, opencode) do
    on_event.(%{
      event: :session_started,
      phase: :session,
      timestamp: DateTime.utc_now(),
      runner_owner: "opencode",
      project_root: cwd,
      command: command,
      session_id: session_id,
      attach_url: opencode.server_url
    })
  end

  defp emit_acp_update(context, event, method, params) do
    payload = %{"method" => method, "params" => params}

    update =
      %{
        event: event,
        phase: acp_update_phase(method, params),
        timestamp: DateTime.utc_now(),
        runner_owner: "opencode",
        project_root: context.cwd,
        command: context.command,
        session_id: context.session_id,
        attach_url: context.opencode.server_url,
        payload: payload
      }
      |> maybe_put_usage(params)
      |> maybe_put_persisted_usage(context.session_usage_reader, context.cwd, context.session_id)

    context.on_event.(update)
  end

  defp acp_update_phase(_method, %{"type" => "usage"}), do: :usage

  defp acp_update_phase(method, params) do
    cond do
      end_turn_event?(method, params) -> :completed
      user_input_required?([{method, params}]) -> :blocked
      true -> :running
    end
  end

  defp maybe_put_usage(update, %{"usage" => usage}) when is_map(usage),
    do: Map.put(update, :usage, usage)

  defp maybe_put_usage(update, _params), do: update

  defp maybe_put_persisted_usage(
         %{usage: usage} = update,
         _session_usage_reader,
         _cwd,
         _session_id
       )
       when is_map(usage),
       do: update

  defp maybe_put_persisted_usage(update, session_usage_reader, cwd, session_id)
       when is_function(session_usage_reader, 2) and is_binary(session_id) and session_id != "" do
    case session_usage_reader.(cwd, session_id) do
      {:ok, usage} when is_map(usage) -> Map.put(update, :usage, usage)
      _other -> update
    end
  rescue
    _error in [ErlangError, RuntimeError, ArgumentError, File.Error] -> update
  end

  defp maybe_put_persisted_usage(update, _session_usage_reader, _cwd, _session_id), do: update

  defp safe_cwd(cwd) do
    with {:ok, canonical} <- PathSafety.canonicalize(cwd),
         true <-
           Path.type(canonical) == :absolute || {:error, {:opencode_acp_relative_cwd, canonical}} do
      {:ok, canonical}
    end
  end

  defp initialize_params(opencode) do
    %{"protocolVersion" => 1}
    |> put_if_present("agent", opencode.agent)
    |> put_if_present("model", opencode.model)
  end

  defp session_params(%Issue{}, cwd, title, opencode) do
    %{
      "cwd" => cwd,
      "title" => title,
      "agent" => opencode.agent,
      "mcpServers" => []
    }
    |> put_if_present("model", opencode.model)
  end

  defp put_if_present(map, _key, nil), do: map
  defp put_if_present(map, _key, ""), do: map
  defp put_if_present(map, key, value), do: Map.put(map, key, value)

  defp opencode_project_root(project_root, _workspace)
       when is_binary(project_root) and project_root != "", do: project_root

  defp opencode_project_root(_project_root, workspace), do: workspace

  defp settings_from_opts(opts) do
    case Keyword.get(opts, :settings) do
      nil -> Config.settings!(Keyword.get(opts, :project_context))
      settings -> settings
    end
  end

  defp fetch_stored_session_id(session_store, issue, cwd, session_scope, settings) do
    case session_store_fetch(session_store, issue, cwd, session_scope, settings) do
      {:ok, session_id} -> {:ok, session_id}
      {:error, reason} -> {:error, {:opencode_acp_session_store_failed, reason}}
    end
  end

  defp persist_new_session_id(_session_store, _issue, _cwd, nil, _session_scope, _settings),
    do: {:error, {:opencode_acp_session_store_failed, :missing_session_id}}

  defp persist_new_session_id(_session_store, _issue, _cwd, "", _session_scope, _settings),
    do: {:error, {:opencode_acp_session_store_failed, :missing_session_id}}

  defp persist_new_session_id(session_store, issue, cwd, session_id, session_scope, settings) do
    case session_store_put(session_store, issue, cwd, session_id, session_scope, settings) do
      :ok -> :ok
      {:error, reason} -> {:error, {:opencode_acp_session_store_failed, reason}}
    end
  end

  defp session_store_fetch(session_store, issue, cwd, session_scope, settings) do
    if function_exported?(session_store, :fetch, 4) do
      session_store.fetch(issue, cwd, session_scope, settings: settings)
    else
      session_store.fetch(issue, cwd, session_scope)
    end
  end

  defp session_store_put(session_store, issue, cwd, session_id, session_scope, settings) do
    if function_exported?(session_store, :put, 5) do
      session_store.put(issue, cwd, session_id, session_scope, settings: settings)
    else
      session_store.put(issue, cwd, session_id, session_scope)
    end
  end

  defp session_id(%{"sessionId" => session_id}), do: session_id
  defp session_id(%{"session_id" => session_id}), do: session_id
  defp session_id(%{"id" => session_id}), do: session_id
  defp session_id(_result), do: nil

  defp resumed_session?(%{"symphony_resumed" => true}), do: true
  defp resumed_session?(_result), do: false

  defp issue_title(%Issue{identifier: identifier, title: title}) do
    [identifier, title]
    |> Enum.filter(&(is_binary(&1) and String.trim(&1) != ""))
    |> Enum.join(" ")
  end

  defp end_turn_result?(%{"stopReason" => "end_turn"}), do: true
  defp end_turn_result?(%{"stop_reason" => "end_turn"}), do: true
  defp end_turn_result?(%{"type" => "end_turn"}), do: true
  defp end_turn_result?(_result), do: false

  defp end_turn_event?(_method, %{"type" => type}) when type in ["end_turn", "stop"], do: true

  defp end_turn_event?(method, _params) when method in ["session/end_turn", "session/stop"],
    do: true

  defp end_turn_event?(_method, _params), do: false

  defp user_input_required?(events) do
    Enum.any?(events, fn
      {method, _params}
      when method in ["session/user_input_required", "session/request_permission"] ->
        true

      {_method, %{"type" => type}} when type in ["user_input_required", "permission"] ->
        true

      _event ->
        false
    end)
  end

  defp format_output(events, result) do
    text =
      events
      |> Enum.reverse()
      |> Enum.flat_map(&event_lines/1)
      |> Enum.join("\n")

    result_line = "ACP result: #{inspect(result)}"

    case String.trim(text) do
      "" -> result_line <> "\n"
      _ -> text <> "\n" <> result_line <> "\n"
    end
  end

  defp event_lines({_method, %{"text" => text}}) when is_binary(text), do: [text]
  defp event_lines({_method, %{"message" => text}}) when is_binary(text), do: [text]
  defp event_lines({_method, %{"usage" => usage}}), do: ["usage: #{inspect(usage)}"]
  defp event_lines({_method, %{"tool" => tool}}), do: ["tool: #{inspect(tool)}"]
  defp event_lines({_method, _params}), do: []
end
