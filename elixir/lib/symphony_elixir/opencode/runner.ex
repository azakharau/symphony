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
    command = Keyword.get(opts, :command, opencode.command)
    runner = Keyword.get(opts, :runner, &System.cmd/3)

    args = [
      "run",
      "--dir",
      workspace,
      "--agent",
      opencode.agent,
      "--format",
      opencode.format,
      "--title",
      issue_title(issue),
      prompt
    ]

    Logger.info("Starting OpenCode runner for issue_id=#{issue.id} issue_identifier=#{issue.identifier} workspace=#{workspace}")

    task =
      Task.async(fn ->
        runner.(command, args, cd: workspace, stderr_to_stdout: true)
      end)

    case Task.yield(task, opencode.timeout_ms) || Task.shutdown(task, :brutal_kill) do
      nil ->
        {:error, {:opencode_timeout, opencode.timeout_ms}}

      {:exit, reason} ->
        {:error, {:opencode_failed, reason}}

      {:ok, {output, 0}} ->
        {:ok, %{output: output, command: [command | args]}}

      {:ok, {output, status}} ->
        {:error, {:opencode_exit, status, trim_output(output)}}
    end
  rescue
    error in [ErlangError, RuntimeError, ArgumentError] ->
      {:error, {:opencode_failed, Exception.message(error)}}
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
