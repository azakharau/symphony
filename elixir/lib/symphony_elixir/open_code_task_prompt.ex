defmodule SymphonyElixir.OpenCode.TaskPrompt do
  @moduledoc """
  Extracts the Codex architect-authored task packet that Symphony may hand to OpenCode.
  """

  defmodule Packet do
    @moduledoc false
    defstruct [:prompt, :slice_id, :fingerprint]

    @type t :: %__MODULE__{prompt: String.t(), slice_id: String.t(), fingerprint: String.t()}
  end

  defmodule Identity do
    @moduledoc false
    defstruct [:slice_id, :fingerprint]

    @type t :: %__MODULE__{slice_id: String.t(), fingerprint: String.t()}
  end

  @marker "symphony:opencode-task-prompt:v1"

  @spec extract(String.t()) :: {:ok, String.t()} | {:error, :opencode_task_prompt_not_found}
  def extract(body) when is_binary(body) do
    with {:ok, packet} <- extract_packet(body) do
      {:ok, packet.prompt}
    end
  end

  def extract(_body), do: {:error, :opencode_task_prompt_not_found}

  @spec marker_present?(String.t()) :: boolean()
  def marker_present?(body) when is_binary(body), do: String.contains?(body, @marker)
  def marker_present?(_body), do: false

  @spec extract_packet(String.t()) :: {:ok, Packet.t()} | {:error, term()}
  def extract_packet(body) when is_binary(body) do
    with {:ok, attrs, after_marker} <- split_marker(body),
         {:ok, slice_id} <- required_attr_value(attrs, "slice_id"),
         {:ok, prompt} <- fenced_prompt(after_marker) do
      {:ok,
       %Packet{
         prompt: prompt,
         slice_id: slice_id,
         fingerprint: fingerprint(prompt)
       }}
    end
  end

  def extract_packet(_body), do: {:error, :opencode_task_prompt_not_found}

  @spec identity(Packet.t()) :: Identity.t()
  def identity(%Packet{slice_id: slice_id, fingerprint: fingerprint}) do
    %Identity{slice_id: slice_id, fingerprint: fingerprint}
  end

  @spec fingerprint_prefix(Packet.t() | Identity.t()) :: String.t()
  def fingerprint_prefix(%{fingerprint: fingerprint}) when is_binary(fingerprint) do
    String.slice(fingerprint, 0, 12)
  end

  @spec title_suffix(Packet.t() | Identity.t()) :: String.t()
  def title_suffix(%Packet{} = packet), do: title_suffix(identity(packet))

  def title_suffix(%Identity{slice_id: slice_id} = identity) do
    "slice=#{slice_id} fp=#{fingerprint_prefix(identity)}"
  end

  defp split_marker(body) do
    case Regex.run(~r/<!--\s*#{Regex.escape(@marker)}(?<attrs>[^>]*)-->(?<after>.*)/s, body, capture: :all_names) do
      [after_marker, attrs] -> {:ok, attrs, after_marker}
      _ -> {:error, :opencode_task_prompt_not_found}
    end
  end

  defp fenced_prompt(after_marker) when is_binary(after_marker) do
    with {:ok, body} <- strip_opening_fence(after_marker),
         {:ok, prompt} <- strip_closing_fence(body) do
      prompt = String.trim(prompt)

      if prompt == "" do
        {:error, :opencode_task_prompt_empty}
      else
        {:ok, prompt}
      end
    end
  end

  defp strip_opening_fence(after_marker) do
    case Regex.run(~r/^\s*```(?:text|markdown)?[ \t]*\r?\n(?<body>.*)$/s, after_marker, capture: :all_names) do
      [body] -> {:ok, body}
      _ -> {:error, :opencode_task_prompt_malformed_fence}
    end
  end

  defp strip_closing_fence(body) do
    case Regex.run(~r/(?<prompt>.*)\r?\n```[ \t]*(?:\r?\n\s*)?$/s, body, capture: :all_names) do
      [prompt] -> {:ok, prompt}
      _ -> {:error, :opencode_task_prompt_malformed_fence}
    end
  end

  defp required_attr_value(attrs, key) do
    case attr_value(attrs, key) do
      value when is_binary(value) and value != "" -> {:ok, value}
      _ -> {:error, :opencode_task_prompt_missing_slice_id}
    end
  end

  defp attr_value(attrs, key) when is_binary(attrs) and is_binary(key) do
    case Regex.run(~r/(?:^|\s)#{Regex.escape(key)}=(?:"([^"]*)"|([^\s>]+))/, attrs, capture: :all_but_first) do
      ["", bare] -> String.trim(bare)
      [quoted] -> String.trim(quoted)
      _ -> nil
    end
  end

  defp fingerprint(prompt) do
    :crypto.hash(:sha256, prompt)
    |> Base.encode16(case: :lower)
  end
end
