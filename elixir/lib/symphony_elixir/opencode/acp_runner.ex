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
    opencode = Config.settings!().opencode
    command = Keyword.get(opts, :command, opencode.command)
    args = Keyword.get(opts, :args, opencode.args || ["acp"])
    client_module = Keyword.get(opts, :client, ACPClient)
    session_store = Keyword.get(opts, :session_store, ACPSessionStore)
    cwd = Keyword.get(opts, :cwd, opencode_project_root(opencode.project_root, workspace))
    title = Keyword.get(opts, :title, issue_title(issue))

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
             {:ok, existing_session_id} <- fetch_stored_session_id(session_store, issue, cwd),
             {:ok, session_result} <-
               open_session(client_module, client, existing_session_id, issue, cwd, title, opencode) do
          session_id = session_id(session_result) || existing_session_id

          if resumed_session?(session_result) do
            {:error, {:need_owner_input, {:opencode_acp_session_attached, session_id}}}
          else
            with :ok <- persist_new_session_id(session_store, issue, cwd, session_id) do
              run_prompt(client_module, client, session_id, prompt, opencode, [command | args], cwd)
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

  defp run_prompt(client_module, client, session_id, prompt, opencode, command, cwd) do
    task =
      Task.async(fn ->
        client_module.prompt(
          client,
          %{
            "sessionId" => session_id,
            "prompt" => [
              %{
                "type" => "text",
                "text" => prompt
              }
            ]
          },
          opencode.timeout_ms
        )
      end)

    collect_prompt_result(
      task,
      client_module,
      client,
      session_id,
      opencode,
      command,
      cwd,
      [],
      false
    )
  end

  defp collect_prompt_result(
         task,
         client_module,
         client,
         session_id,
         opencode,
         command,
         cwd,
         events,
         end_turn?
       ) do
    receive do
      {ref, {:ok, result}} when ref == task.ref ->
        Process.demonitor(task.ref, [:flush])

        cond do
          user_input_required?(events) ->
            {:error, {:need_owner_input, events}}

          end_turn? or end_turn_result?(result) ->
            {:ok,
             %{
               output: format_output(events, result),
               command: command,
               project_root: cwd,
               session_id: session_id,
               attach_url: opencode.server_url,
               runner_owner: "opencode"
             }}

          true ->
            {:error, {:opencode_acp_incomplete, result}}
        end

      {ref, {:error, :timeout}} when ref == task.ref ->
        Process.demonitor(task.ref, [:flush])
        maybe_cancel(client_module, client, session_id, opencode)
        {:error, {:opencode_timeout, opencode.timeout_ms}}

      {ref, {:error, reason}} when ref == task.ref ->
        Process.demonitor(task.ref, [:flush])
        {:error, reason}

      {:DOWN, ref, :process, _pid, reason} when ref == task.ref ->
        {:error, {:opencode_acp_prompt_failed, reason}}

      {:acp_notification, method, params} ->
        collect_prompt_result(
          task,
          client_module,
          client,
          session_id,
          opencode,
          command,
          cwd,
          [{method, params} | events],
          end_turn? or end_turn_event?(method, params)
        )

      {:acp_request, method, params} ->
        collect_prompt_result(
          task,
          client_module,
          client,
          session_id,
          opencode,
          command,
          cwd,
          [{method, params} | events],
          end_turn?
        )
    after
      stall_timeout(opencode.stall_timeout_ms) ->
        maybe_cancel(client_module, client, session_id, opencode)
        Task.shutdown(task, :brutal_kill)
        {:error, {:opencode_acp_stalled, opencode.stall_timeout_ms}}
    end
  end

  defp stall_timeout(0), do: :infinity
  defp stall_timeout(timeout_ms), do: timeout_ms

  defp maybe_cancel(client_module, client, session_id, opencode) do
    if client_module.capability?(client, "session/cancel") do
      client_module.cancel(client, %{"sessionId" => session_id}, opencode.read_timeout_ms)
    else
      {:error, {:unsupported_acp_method, "session/cancel"}}
    end
  catch
    _kind, reason -> {:error, reason}
  end

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

  defp fetch_stored_session_id(session_store, issue, cwd) do
    case session_store.fetch(issue, cwd) do
      {:ok, session_id} -> {:ok, session_id}
      {:error, reason} -> {:error, {:opencode_acp_session_store_failed, reason}}
    end
  end

  defp persist_new_session_id(_session_store, _issue, _cwd, nil),
    do: {:error, {:opencode_acp_session_store_failed, :missing_session_id}}

  defp persist_new_session_id(_session_store, _issue, _cwd, ""),
    do: {:error, {:opencode_acp_session_store_failed, :missing_session_id}}

  defp persist_new_session_id(session_store, issue, cwd, session_id) do
    case session_store.put(issue, cwd, session_id) do
      :ok -> :ok
      {:error, reason} -> {:error, {:opencode_acp_session_store_failed, reason}}
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
