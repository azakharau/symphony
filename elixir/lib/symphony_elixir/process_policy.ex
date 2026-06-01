defmodule SymphonyElixir.ProcessPolicy do
  @moduledoc """
  Process guardrails that keep Symphony from repeating the same failed repair loop.
  """

  alias SymphonyElixir.{Config, ReviewDecision}
  alias SymphonyElixir.OpenCode.TaskPrompt

  @spec codex_owned_rca_required_state(term()) :: {:ok, String.t()}
  def codex_owned_rca_required_state(project_context \\ nil) do
    settings = Config.settings!(project_context)
    {:ok, settings.process_policy.rca_required_state}
  end

  @spec opencode_dispatch_decision(TaskPrompt.Packet.t(), [ReviewDecision.t()]) :: :allow | {:block, map()}
  def opencode_dispatch_decision(packet, decisions), do: opencode_dispatch_decision(packet, decisions, nil)

  @spec opencode_dispatch_decision(TaskPrompt.Packet.t(), [ReviewDecision.t()], term()) :: :allow | {:block, map()}
  def opencode_dispatch_decision(%TaskPrompt.Packet{slice_id: slice_id}, _decisions, project_context)
      when not is_binary(slice_id) or slice_id == "" do
    {:block,
     %{
       reason: :opencode_task_prompt_missing_slice_id,
       slice_id: slice_id,
       rejection_count: 0,
       rca_required_state: Config.settings!(project_context).process_policy.rca_required_state
     }}
  end

  def opencode_dispatch_decision(%TaskPrompt.Packet{} = packet, decisions, project_context) when is_list(decisions) do
    settings = Config.settings!(project_context)
    max_rejections = settings.process_policy.max_rejections_per_slice
    rejection_count = same_slice_rejection_count(packet, decisions)

    if rejection_count >= max_rejections do
      {:block,
       %{
         reason: :repair_loop_breaker,
         slice_id: packet.slice_id,
         rejection_count: rejection_count,
         rca_required_state: settings.process_policy.rca_required_state
       }}
    else
      :allow
    end
  end

  @spec loop_breaker_comment(map()) :: String.t()
  def loop_breaker_comment(block) when is_map(block) do
    """
    ## Symphony Stop Rule

    OpenCode dispatch was blocked by the repair loop breaker.

    slice_id: #{block[:slice_id] || "unknown"}
    rejection_count: #{block[:rejection_count] || 0}
    next_state: #{block[:rca_required_state] || "RCA Required"}

    Codex must perform RCA and create a redesigned implementation prompt before another coding run.
    """
  end

  defp same_slice_rejection_count(%TaskPrompt.Packet{slice_id: slice_id}, decisions) when is_binary(slice_id) and slice_id != "" do
    Enum.count(decisions, fn
      %ReviewDecision{status: "rejected", slice_id: ^slice_id} -> true
      _ -> false
    end)
  end
end
