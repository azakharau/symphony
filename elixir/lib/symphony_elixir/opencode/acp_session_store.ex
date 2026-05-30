defmodule SymphonyElixir.OpenCode.ACPSessionStore do
  @moduledoc false

  alias SymphonyElixir.{Config, Linear.Issue, PathSafety}

  @file_name "opencode_acp_sessions.json"

  @spec path() :: Path.t()
  def path do
    Path.join([Config.settings!().workspace.root, ".symphony", @file_name])
  end

  @spec fetch(Issue.t(), Path.t()) :: {:ok, String.t() | nil} | {:error, term()}
  def fetch(%Issue{} = issue, project_root) when is_binary(project_root) do
    with {:ok, key} <- session_key(issue, project_root),
         {:ok, sessions} <- read_sessions() do
      case Map.get(sessions, key) do
        %{"session_id" => session_id} when is_binary(session_id) and session_id != "" ->
          {:ok, session_id}

        _missing ->
          {:ok, nil}
      end
    end
  end

  @spec put(Issue.t(), Path.t(), String.t()) :: :ok | {:error, term()}
  def put(%Issue{} = issue, project_root, session_id)
      when is_binary(project_root) and is_binary(session_id) and session_id != "" do
    with {:ok, key} <- session_key(issue, project_root),
         {:ok, canonical_root} <- PathSafety.canonicalize(project_root),
         {:ok, sessions} <- read_sessions(),
         :ok <-
           write_sessions(Map.put(sessions, key, session_entry(issue, canonical_root, session_id))) do
      :ok
    end
  end

  def put(_issue, _project_root, _session_id), do: :ok

  defp session_key(%Issue{id: issue_id}, project_root)
       when is_binary(issue_id) and issue_id != "" do
    with {:ok, canonical_root} <- PathSafety.canonicalize(project_root) do
      digest =
        :crypto.hash(:sha256, canonical_root <> "\0" <> issue_id) |> Base.encode16(case: :lower)

      {:ok, digest}
    end
  end

  defp session_key(%Issue{identifier: identifier}, project_root)
       when is_binary(identifier) and identifier != "" do
    with {:ok, canonical_root} <- PathSafety.canonicalize(project_root) do
      digest =
        :crypto.hash(:sha256, canonical_root <> "\0" <> identifier) |> Base.encode16(case: :lower)

      {:ok, digest}
    end
  end

  defp session_key(_issue, _project_root),
    do: {:error, :opencode_acp_session_key_missing_issue_id}

  defp session_entry(%Issue{} = issue, canonical_root, session_id) do
    %{
      "issue_id" => issue.id,
      "issue_identifier" => issue.identifier,
      "project_root" => canonical_root,
      "session_id" => session_id,
      "updated_at" => DateTime.utc_now() |> DateTime.to_iso8601()
    }
  end

  defp read_sessions do
    case File.read(path()) do
      {:ok, content} ->
        case Jason.decode(content) do
          {:ok, sessions} when is_map(sessions) ->
            {:ok, sessions}

          {:ok, _other} ->
            {:error, {:opencode_acp_session_store_invalid, path()}}

          {:error, reason} ->
            {:error, {:opencode_acp_session_store_decode_failed, Exception.message(reason)}}
        end

      {:error, :enoent} ->
        {:ok, %{}}

      {:error, reason} ->
        {:error, {:opencode_acp_session_store_read_failed, reason}}
    end
  end

  defp write_sessions(sessions) when is_map(sessions) do
    store_path = path()
    tmp_path = store_path <> ".tmp"

    with :ok <- File.mkdir_p(Path.dirname(store_path)),
         :ok <- File.write(tmp_path, Jason.encode!(sessions, pretty: true)),
         :ok <- File.rename(tmp_path, store_path) do
      :ok
    else
      {:error, reason} -> {:error, {:opencode_acp_session_store_write_failed, reason}}
    end
  end
end
