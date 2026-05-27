defmodule SymphonyElixir.OpenCode.TaskPrompt do
  @moduledoc """
  Extracts the Codex architect-authored task packet that Symphony may hand to OpenCode.
  """

  defmodule Packet do
    @moduledoc false
    defstruct [:prompt, :slice_id, :fingerprint]

    @type t :: %__MODULE__{prompt: String.t(), slice_id: String.t() | nil, fingerprint: String.t()}
  end

  @marker "symphony:opencode-task-prompt:v1"

  @spec extract(String.t()) :: {:ok, String.t()} | {:error, :opencode_task_prompt_not_found}
  def extract(body) when is_binary(body) do
    with {:ok, packet} <- extract_packet(body) do
      {:ok, packet.prompt}
    end
  end

  def extract(_body), do: {:error, :opencode_task_prompt_not_found}

  @spec extract_packet(String.t()) :: {:ok, Packet.t()} | {:error, :opencode_task_prompt_not_found}
  def extract_packet(body) when is_binary(body) do
    case Regex.run(~r/<!--\s*#{Regex.escape(@marker)}(?<attrs>[^>]*)-->\s*```(?:text|markdown)?\s*(?<prompt>.*?)```/s, body, capture: :all_names) do
      [attrs, prompt] ->
        prompt = String.trim(prompt)

        if prompt == "" do
          {:error, :opencode_task_prompt_not_found}
        else
          {:ok,
           %Packet{
             prompt: prompt,
             slice_id: attr_value(attrs, "slice_id"),
             fingerprint: fingerprint(prompt)
           }}
        end

      _ ->
        {:error, :opencode_task_prompt_not_found}
    end
  end

  def extract_packet(_body), do: {:error, :opencode_task_prompt_not_found}

  defp attr_value(attrs, key) when is_binary(attrs) and is_binary(key) do
    case Regex.run(~r/(?:^|\s)#{Regex.escape(key)}=(?:"([^"]+)"|([^\s>]+))/, attrs) do
      [_, quoted, ""] -> quoted
      [_, "", bare] -> bare
      _ -> nil
    end
  end

  defp fingerprint(prompt) do
    :crypto.hash(:sha256, prompt)
    |> Base.encode16(case: :lower)
  end
end
