defmodule SymphonyElixir.OpenCode.ACPClient do
  @moduledoc """
  Minimal stdio JSON-RPC client for OpenCode ACP.
  """

  use GenServer

  alias SymphonyElixir.PathSafety

  @type t :: pid()
  @type start_option ::
          {:command, String.t()}
          | {:args, [String.t()]}
          | {:cwd, Path.t()}
          | {:handler, pid()}
          | {:permission_policy, String.t()}
          | {:read_timeout_ms, pos_integer()}

  @spec start_link([start_option()]) :: GenServer.on_start()
  def start_link(opts) when is_list(opts) do
    with {:ok, executable} <- resolve_executable(Keyword.fetch!(opts, :command)),
         {:ok, cwd} <- PathSafety.canonicalize(Keyword.fetch!(opts, :cwd)) do
      opts = opts |> Keyword.put(:executable, executable) |> Keyword.put(:cwd, cwd)
      GenServer.start_link(__MODULE__, opts)
    end
  end

  @spec stop(t()) :: :ok
  def stop(client) when is_pid(client) do
    GenServer.stop(client, :normal)
  end

  @spec initialize(t(), map(), timeout()) :: {:ok, map()} | {:error, term()}
  def initialize(client, params \\ %{}, timeout \\ 5_000) do
    request(client, "initialize", params, timeout)
  end

  @spec new_session(t(), map(), timeout()) :: {:ok, map()} | {:error, term()}
  def new_session(client, params, timeout \\ 5_000),
    do: gated_request(client, "session/new", params, timeout)

  @spec load_session(t(), map(), timeout()) :: {:ok, map()} | {:error, term()}
  def load_session(client, params, timeout \\ 5_000),
    do: gated_request(client, "session/load", params, timeout)

  @spec resume_session(t(), map(), timeout()) :: {:ok, map()} | {:error, term()}
  def resume_session(client, params, timeout \\ 5_000),
    do: gated_request(client, "session/resume", params, timeout)

  @spec prompt(t(), map(), timeout()) :: {:ok, map()} | {:error, term()}
  def prompt(client, params, timeout \\ 5_000),
    do: gated_request(client, "session/prompt", params, timeout)

  @spec cancel(t(), map(), timeout()) :: {:ok, map()} | {:error, term()}
  def cancel(client, params, timeout \\ 5_000),
    do: gated_request(client, "session/cancel", params, timeout)

  @spec capabilities(t()) :: map()
  def capabilities(client), do: GenServer.call(client, :capabilities)

  @spec request(t(), String.t(), map(), timeout()) :: {:ok, map()} | {:error, term()}
  def request(client, method, params, timeout \\ 5_000)
      when is_binary(method) and is_map(params) do
    GenServer.call(client, {:request, method, params}, timeout)
  catch
    :exit, {:timeout, _call} -> {:error, :timeout}
  end

  @spec gated_request(t(), String.t(), map(), timeout()) :: {:ok, map()} | {:error, term()}
  def gated_request(client, method, params, timeout \\ 5_000) do
    if capability?(client, method) do
      request(client, method, params, timeout)
    else
      {:error, {:unsupported_acp_method, method}}
    end
  end

  @spec capability?(t(), String.t()) :: boolean()
  def capability?(client, method) do
    GenServer.call(client, {:capability?, method})
  end

  @impl true
  def init(opts) do
    executable = Keyword.fetch!(opts, :executable)
    args = Keyword.get(opts, :args, [])
    cwd = Keyword.fetch!(opts, :cwd)

    port =
      Port.open({:spawn_executable, executable}, [
        :binary,
        :exit_status,
        {:args, args},
        {:cd, cwd},
        {:line, 65_536}
      ])

    {:ok,
     %{
       port: port,
       next_id: 1,
       pending: %{},
       capabilities: %{},
       handler: Keyword.get(opts, :handler, self()),
       permission_policy: Keyword.get(opts, :permission_policy, "reject"),
       cwd: cwd
     }}
  end

  @impl true
  def handle_call(:capabilities, _from, state), do: {:reply, state.capabilities, state}

  def handle_call({:capability?, method}, _from, state) do
    {:reply, capability_supported?(state.capabilities, method), state}
  end

  def handle_call({:request, method, params}, from, state) do
    id = state.next_id

    payload = %{
      "jsonrpc" => "2.0",
      "id" => id,
      "method" => method,
      "params" => params
    }

    send_json(state.port, payload)
    {:noreply, %{state | next_id: id + 1, pending: Map.put(state.pending, id, {from, method})}}
  end

  @impl true
  def handle_info({_port, {:data, {:eol, line}}}, state), do: handle_line(line, state)
  def handle_info({_port, {:data, {:noeol, line}}}, state), do: handle_line(line, state)

  def handle_info({_port, {:exit_status, status}}, state) do
    Enum.each(state.pending, fn {_id, {from, _method}} ->
      GenServer.reply(from, {:error, {:acp_subprocess_exit, status}})
    end)

    {:stop, {:acp_subprocess_exit, status}, %{state | pending: %{}}}
  end

  def handle_info(_message, state), do: {:noreply, state}

  @impl true
  def terminate(_reason, state) do
    if Map.has_key?(state, :port), do: Port.close(state.port)
    :ok
  catch
    _kind, _reason -> :ok
  end

  defp handle_line(line, state) when is_binary(line) do
    case Jason.decode(line) do
      {:ok, payload} -> handle_payload(payload, state)
      {:error, reason} -> {:stop, {:invalid_acp_json, Exception.message(reason), line}, state}
    end
  end

  defp handle_payload(%{"id" => id, "result" => result}, state) do
    case Map.pop(state.pending, id) do
      {{from, "initialize"}, pending} ->
        capabilities = extract_capabilities(result)
        GenServer.reply(from, {:ok, result})
        {:noreply, %{state | pending: pending, capabilities: capabilities}}

      {{from, _method}, pending} ->
        GenServer.reply(from, {:ok, result})
        {:noreply, %{state | pending: pending}}

      {nil, _pending} ->
        {:noreply, state}
    end
  end

  defp handle_payload(%{"id" => id, "error" => error}, state) do
    case Map.pop(state.pending, id) do
      {{from, _method}, pending} ->
        GenServer.reply(from, {:error, error})
        {:noreply, %{state | pending: pending}}

      {nil, _pending} ->
        {:noreply, state}
    end
  end

  defp handle_payload(%{"id" => id, "method" => method, "params" => params}, state) do
    if permission_request?(method) do
      send_json(state.port, %{
        "jsonrpc" => "2.0",
        "id" => id,
        "result" => permission_result(state.permission_policy)
      })
    end

    send(state.handler, {:acp_request, method, params})
    {:noreply, state}
  end

  defp handle_payload(%{"method" => method, "params" => params}, state) do
    send(state.handler, {:acp_notification, method, params})
    {:noreply, state}
  end

  defp handle_payload(_payload, state), do: {:noreply, state}

  defp send_json(port, payload) do
    Port.command(port, Jason.encode!(payload) <> "\n")
  end

  defp resolve_executable(command) when is_binary(command) do
    cond do
      String.contains?(command, "/") and File.exists?(command) ->
        {:ok, Path.expand(command)}

      String.contains?(command, "/") ->
        {:error, {:acp_command_not_found, command}}

      executable = System.find_executable(command) ->
        {:ok, executable}

      true ->
        {:error, {:acp_command_not_found, command}}
    end
  end

  defp resolve_executable(command), do: {:error, {:acp_command_not_found, command}}

  defp permission_request?(method) when is_binary(method) do
    String.contains?(method, "permission") or String.contains?(method, "user_input")
  end

  defp permission_result("cancel"), do: %{"outcome" => "cancel"}
  defp permission_result(_policy), do: %{"outcome" => "reject"}

  defp extract_capabilities(result) when is_map(result) do
    [
      Map.get(result, "capabilities"),
      Map.get(result, "serverCapabilities"),
      Map.get(result, "server_capabilities"),
      Map.get(result, "agentCapabilities"),
      Map.get(result, "agent_capabilities"),
      result
    ]
    |> Enum.reduce(%{}, &Map.merge(&2, capability_map(&1)))
  end

  defp extract_capabilities(_result), do: %{}

  defp capability_map(nil), do: %{}

  defp capability_map(methods) when is_list(methods) do
    methods
    |> Enum.filter(&is_binary/1)
    |> Map.new(&{&1, true})
  end

  defp capability_map(capabilities) when is_map(capabilities) do
    base = if agent_capability_shape?(capabilities), do: %{"session/new" => true}, else: %{}

    direct =
      Enum.reduce(capabilities, base, fn {key, value}, acc ->
        key = to_string(key)

        cond do
          key in ["methods", "requests", "requestMethods"] and is_list(value) ->
            Map.merge(acc, capability_map(value))

          key in ["session", "sessions"] and is_map(value) ->
            Map.merge(acc, nested_capability_map("session", value))

          key in ["sessionCapabilities", "session_capabilities"] and is_map(value) ->
            Map.merge(acc, nested_capability_map("session", value))

          key in ["promptCapabilities", "prompt_capabilities"] and is_map(value) and value != %{} ->
            Map.put(acc, "session/prompt", true)

          key in ["loadSession", "load_session"] and truthy?(value) ->
            Map.put(acc, "session/load", true)

          String.contains?(key, "/") and truthy?(value) ->
            Map.put(acc, key, true)

          String.contains?(key, "_") and truthy?(value) ->
            Map.put(acc, String.replace(key, "_", "/"), true)

          true ->
            acc
        end
      end)

    nested =
      capabilities
      |> Map.values()
      |> Enum.filter(&is_map/1)
      |> Enum.reduce(%{}, &Map.merge(&2, capability_map(&1)))

    Map.merge(direct, nested)
  end

  defp capability_map(_capabilities), do: %{}

  defp agent_capability_shape?(capabilities) do
    Enum.any?(capabilities, fn {key, _value} ->
      to_string(key) in [
        "agentCapabilities",
        "agent_capabilities",
        "loadSession",
        "load_session",
        "promptCapabilities",
        "prompt_capabilities",
        "sessionCapabilities",
        "session_capabilities"
      ]
    end)
  end

  defp nested_capability_map(prefix, values) do
    Enum.reduce(values, %{}, fn {key, value}, acc ->
      method = nested_method_name(prefix, to_string(key))

      if truthy?(value) and is_binary(method) do
        Map.put(acc, method, true)
      else
        acc
      end
    end)
  end

  defp nested_method_name("session", "new"), do: "session/new"
  defp nested_method_name("session", "load"), do: "session/load"
  defp nested_method_name("session", "resume"), do: "session/resume"
  defp nested_method_name("session", "close"), do: "session/cancel"
  defp nested_method_name("session", key), do: "session/" <> key

  defp capability_supported?(capabilities, method) when is_map(capabilities) do
    method_key = method
    underscore_key = String.replace(method, "/", "_")
    short_key = method |> String.split("/") |> List.last()

    Enum.any?([method_key, underscore_key, short_key], fn key ->
      truthy?(Map.get(capabilities, key))
    end)
  end

  defp truthy?(true), do: true
  defp truthy?(%{"supported" => true}), do: true
  defp truthy?(value) when is_map(value), do: true
  defp truthy?(_value), do: false
end
