defmodule SymphonyElixir.PulseLedger do
  @moduledoc """
  Durable ledger for cheap-first orchestration pulse suppression state.
  """

  use GenServer
  require Logger

  @max_suppression_events 100
  @owner_wait_no_change "owner_wait_no_change"
  @done_continuation_already_processed "done_continuation_already_processed"
  @active_milestone_locked "active_milestone_locked"
  @next_milestone_scan_suppressed "next_milestone_scan_suppressed"

  @type ledger :: GenServer.server()
  @type milestone :: map() | nil

  @empty_state %{
    owner_input_processed: MapSet.new(),
    done_continuation_processed: MapSet.new(),
    active_milestone: nil,
    suppression_events: [],
    suppression_counts: %{}
  }

  @spec owner_wait_no_change() :: String.t()
  def owner_wait_no_change, do: @owner_wait_no_change

  @spec done_continuation_already_processed() :: String.t()
  def done_continuation_already_processed, do: @done_continuation_already_processed

  @spec active_milestone_locked() :: String.t()
  def active_milestone_locked, do: @active_milestone_locked

  @spec next_milestone_scan_suppressed() :: String.t()
  def next_milestone_scan_suppressed, do: @next_milestone_scan_suppressed

  @spec start_link(keyword()) :: GenServer.on_start()
  def start_link(opts) do
    case process_name(opts) do
      nil -> GenServer.start_link(__MODULE__, opts)
      name -> GenServer.start_link(__MODULE__, opts, name: name)
    end
  end

  @spec process_name(keyword()) :: GenServer.name() | nil
  defp process_name(opts) do
    case Keyword.fetch(opts, :project_context) do
      {:ok, project_context} -> {:global, {__MODULE__, project_context_id(project_context)}}
      :error -> Keyword.get(opts, :name)
    end
  end

  @doc false
  @spec process_name_for_test(keyword()) :: GenServer.name() | nil
  def process_name_for_test(opts) when is_list(opts), do: process_name(opts)

  defp project_context_id(%{id: id}) when is_binary(id), do: id
  defp project_context_id(%{"id" => id}) when is_binary(id), do: id
  defp project_context_id(%{project_id: project_id}) when is_binary(project_id), do: project_id
  defp project_context_id(%{"project_id" => project_id}) when is_binary(project_id), do: project_id
  defp project_context_id(project_context) when is_binary(project_context), do: project_context
  defp project_context_id(project_context), do: :erlang.phash2(project_context)

  @spec owner_input_processed?(ledger(), String.t()) :: boolean()
  def owner_input_processed?(ledger, fingerprint) when is_binary(fingerprint) do
    GenServer.call(ledger, {:owner_input_processed?, fingerprint})
  end

  @spec record_owner_input(ledger(), String.t()) :: :ok
  def record_owner_input(ledger, fingerprint) when is_binary(fingerprint) do
    GenServer.call(ledger, {:record_owner_input, fingerprint})
  end

  @spec pending_owner_input(ledger(), String.t()) :: :ok
  def pending_owner_input(ledger, fingerprint) when is_binary(fingerprint) do
    GenServer.call(ledger, {:pending_owner_input, fingerprint})
  end

  @spec commit_pending_owner_input(ledger(), String.t()) :: :ok
  def commit_pending_owner_input(ledger, fingerprint) when is_binary(fingerprint) do
    GenServer.call(ledger, {:commit_pending_owner_input, fingerprint})
  end

  @spec pending_owner_inputs(ledger()) :: [String.t()]
  def pending_owner_inputs(ledger), do: GenServer.call(ledger, :pending_owner_inputs)

  @spec has_pending?(ledger()) :: boolean()
  def has_pending?(ledger), do: GenServer.call(ledger, :has_pending?)

  @spec rollback_pending(ledger()) :: :ok
  def rollback_pending(ledger), do: GenServer.call(ledger, :rollback_pending)

  @spec done_continuation_processed?(ledger(), String.t()) :: boolean()
  def done_continuation_processed?(ledger, key) when is_binary(key) do
    GenServer.call(ledger, {:done_continuation_processed?, key})
  end

  @spec record_done_continuation(ledger(), String.t()) :: :ok
  def record_done_continuation(ledger, key) when is_binary(key) do
    GenServer.call(ledger, {:record_done_continuation, key})
  end

  @spec pending_done_continuation(ledger(), String.t()) :: :ok
  def pending_done_continuation(ledger, key) when is_binary(key) do
    GenServer.call(ledger, {:pending_done_continuation, key})
  end

  @spec commit_pending_done_continuation(ledger(), String.t()) :: :ok
  def commit_pending_done_continuation(ledger, key) when is_binary(key) do
    GenServer.call(ledger, {:commit_pending_done_continuation, key})
  end

  @spec active_milestone(ledger()) :: milestone()
  def active_milestone(ledger), do: GenServer.call(ledger, :active_milestone)

  @spec set_active_milestone(ledger(), String.t(), String.t() | nil, String.t() | nil) :: :ok
  def set_active_milestone(ledger, milestone_id, milestone_name, phase_state) when is_binary(milestone_id) do
    GenServer.call(ledger, {:set_active_milestone, milestone_id, milestone_name, phase_state})
  end

  @spec clear_active_milestone(ledger()) :: :ok
  def clear_active_milestone(ledger), do: GenServer.call(ledger, :clear_active_milestone)

  @spec record_suppression(ledger(), String.t(), String.t() | nil, String.t() | nil, String.t() | nil, String.t() | nil, String.t()) :: :ok
  def record_suppression(ledger, kind, issue_id, issue_identifier, milestone_id, milestone_name, reason)
      when is_binary(kind) and is_binary(reason) do
    GenServer.call(ledger, {:record_suppression, kind, issue_id, issue_identifier, milestone_id, milestone_name, reason})
  end

  @spec suppression_events(ledger()) :: [map()]
  def suppression_events(ledger), do: GenServer.call(ledger, :suppression_events)

  @spec suppression_counts(ledger()) :: map()
  def suppression_counts(ledger), do: GenServer.call(ledger, :suppression_counts)

  @spec reset(ledger()) :: :ok
  def reset(ledger), do: GenServer.call(ledger, :reset)

  @impl true
  def init(opts) do
    file_path = Keyword.fetch!(opts, :file_path)

    {:ok,
     %{
       file_path: file_path,
       data: load_state(file_path),
       pending_owner_inputs: MapSet.new(),
       pending_done_continuations: MapSet.new()
     }}
  end

  @impl true
  def handle_call({:owner_input_processed?, fingerprint}, _from, state) do
    {:reply, MapSet.member?(state.data.owner_input_processed, fingerprint), state}
  end

  def handle_call({:record_owner_input, fingerprint}, _from, state) do
    data = %{state.data | owner_input_processed: MapSet.put(state.data.owner_input_processed, fingerprint)}
    {:reply, :ok, persist!(%{state | data: data})}
  end

  def handle_call({:pending_owner_input, fingerprint}, _from, state) do
    {:reply, :ok, %{state | pending_owner_inputs: MapSet.put(state.pending_owner_inputs, fingerprint)}}
  end

  def handle_call({:commit_pending_owner_input, fingerprint}, _from, state) do
    if MapSet.member?(state.pending_owner_inputs, fingerprint) do
      data = %{state.data | owner_input_processed: MapSet.put(state.data.owner_input_processed, fingerprint)}

      state = %{
        state
        | data: data,
          pending_owner_inputs: MapSet.delete(state.pending_owner_inputs, fingerprint)
      }

      {:reply, :ok, persist!(state)}
    else
      {:reply, :ok, state}
    end
  end

  def handle_call(:pending_owner_inputs, _from, state) do
    {:reply, state.pending_owner_inputs |> MapSet.to_list() |> Enum.sort(), state}
  end

  def handle_call(:has_pending?, _from, state) do
    has_pending? =
      MapSet.size(state.pending_owner_inputs) > 0 or MapSet.size(state.pending_done_continuations) > 0

    {:reply, has_pending?, state}
  end

  def handle_call(:rollback_pending, _from, state) do
    {:reply, :ok, %{state | pending_owner_inputs: MapSet.new(), pending_done_continuations: MapSet.new()}}
  end

  def handle_call({:done_continuation_processed?, key}, _from, state) do
    {:reply, MapSet.member?(state.data.done_continuation_processed, key), state}
  end

  def handle_call({:record_done_continuation, key}, _from, state) do
    data = %{state.data | done_continuation_processed: MapSet.put(state.data.done_continuation_processed, key)}
    {:reply, :ok, persist!(%{state | data: data})}
  end

  def handle_call({:pending_done_continuation, key}, _from, state) do
    {:reply, :ok, %{state | pending_done_continuations: MapSet.put(state.pending_done_continuations, key)}}
  end

  def handle_call({:commit_pending_done_continuation, key}, _from, state) do
    if MapSet.member?(state.pending_done_continuations, key) do
      data = %{state.data | done_continuation_processed: MapSet.put(state.data.done_continuation_processed, key)}

      state = %{
        state
        | data: data,
          pending_done_continuations: MapSet.delete(state.pending_done_continuations, key)
      }

      {:reply, :ok, persist!(state)}
    else
      {:reply, :ok, state}
    end
  end

  def handle_call(:active_milestone, _from, state), do: {:reply, state.data.active_milestone, state}

  def handle_call({:set_active_milestone, milestone_id, milestone_name, phase_state}, _from, state) do
    active_milestone = %{
      "milestone_id" => milestone_id,
      "milestone_name" => milestone_name,
      "phase_state" => phase_state,
      "locked_at" => DateTime.utc_now() |> DateTime.truncate(:second) |> DateTime.to_iso8601()
    }

    {:reply, :ok, persist!(%{state | data: %{state.data | active_milestone: active_milestone}})}
  end

  def handle_call(:clear_active_milestone, _from, state) do
    {:reply, :ok, persist!(%{state | data: %{state.data | active_milestone: nil}})}
  end

  def handle_call({:record_suppression, kind, issue_id, issue_identifier, milestone_id, milestone_name, reason}, _from, state) do
    count = Map.get(state.data.suppression_counts, kind, 0) + 1

    event = %{
      "kind" => kind,
      "issue_id" => issue_id,
      "issue_identifier" => issue_identifier,
      "milestone_id" => milestone_id,
      "milestone_name" => milestone_name,
      "reason" => reason,
      "timestamp" => DateTime.utc_now() |> DateTime.truncate(:second) |> DateTime.to_iso8601(),
      "count" => count
    }

    data = %{
      state.data
      | suppression_events: Enum.take(state.data.suppression_events ++ [event], -@max_suppression_events),
        suppression_counts: Map.put(state.data.suppression_counts, kind, count)
    }

    {:reply, :ok, persist!(%{state | data: data})}
  end

  def handle_call(:suppression_events, _from, state), do: {:reply, state.data.suppression_events, state}
  def handle_call(:suppression_counts, _from, state), do: {:reply, state.data.suppression_counts, state}

  def handle_call(:reset, _from, state) do
    {:reply, :ok,
     persist!(%{
       state
       | data: @empty_state,
         pending_owner_inputs: MapSet.new(),
         pending_done_continuations: MapSet.new()
     })}
  end

  defp load_state(file_path) do
    case File.read(file_path) do
      {:ok, contents} ->
        decode_state(contents, file_path)

      {:error, :enoent} ->
        @empty_state

      {:error, reason} ->
        Logger.warning("Failed to read PulseLedger #{file_path}: #{inspect(reason)}")
        @empty_state
    end
  end

  defp decode_state(contents, file_path) do
    case Jason.decode(contents) do
      {:ok, decoded} when is_map(decoded) ->
        state_from_json(decoded)

      {:ok, _other} ->
        Logger.warning("PulseLedger #{file_path} did not contain a JSON object; starting empty")
        @empty_state

      {:error, reason} ->
        Logger.warning("Failed to decode PulseLedger #{file_path}: #{inspect(reason)}")
        @empty_state
    end
  end

  defp state_from_json(decoded) do
    %{
      owner_input_processed: decoded |> Map.get("owner_input_processed", []) |> MapSet.new(),
      done_continuation_processed: decoded |> Map.get("done_continuation_processed", []) |> MapSet.new(),
      active_milestone: Map.get(decoded, "active_milestone"),
      suppression_events: Map.get(decoded, "suppression_events", []),
      suppression_counts: Map.get(decoded, "suppression_counts", %{})
    }
  end

  defp persist!(%{file_path: file_path, data: data} = state) do
    File.mkdir_p!(Path.dirname(file_path))

    payload = %{
      "owner_input_processed" => data.owner_input_processed |> MapSet.to_list() |> Enum.sort(),
      "done_continuation_processed" => data.done_continuation_processed |> MapSet.to_list() |> Enum.sort(),
      "active_milestone" => data.active_milestone,
      "suppression_events" => data.suppression_events,
      "suppression_counts" => data.suppression_counts
    }

    temp_path = file_path <> ".tmp"
    File.write!(temp_path, Jason.encode!(payload))
    File.rename!(temp_path, file_path)
    state
  end
end
