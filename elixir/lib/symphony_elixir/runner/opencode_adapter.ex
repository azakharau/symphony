defmodule SymphonyElixir.Runner.OpenCodeAdapter do
  @moduledoc """
  OpenCode runner adapter.

  Owns OpenCode process/session mechanics behind the shared runner adapter
  boundary. Dispatch policy, task-packet lookup, comments, and Linear state
  transitions live in `SymphonyElixir.Runner.OpenCodeDispatch`.
  """

  alias SymphonyElixir.OpenCode.Runner, as: OpenCodeRunner

  @spec run(map()) :: {:ok, OpenCodeRunner.result()} | {:error, term()}
  def run(%{workspace: workspace, issue: issue, task_packet: task_packet, opts: opts, emit_update: emit_update} = context) do
    runner_opts =
      opts
      |> Keyword.take([:command, :runner, :session_lister, :session_result_reader])
      |> Keyword.put(:on_event, emit_update)
      |> Keyword.put(:worker_host, Map.get(context, :worker_host))

    OpenCodeRunner.run(workspace, issue, task_packet, runner_opts)
  end

  def run(_context), do: {:error, :opencode_task_packet_required}
end
