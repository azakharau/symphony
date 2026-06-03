defmodule SymphonyElixir.OpenCode.ACPSessionStore do
  @moduledoc false

  alias SymphonyElixir.{Config, Linear.Issue, PathSafety}

  @file_name "opencode_acp_sessions.json"

  @spec path(keyword()) :: Path.t()
  def path(opts \\ []) do
    settings = Keyword.get(opts, :settings) || Config.settings!(Keyword.get(opts, :project_context))
    Path.join([settings.workspace.root, ".symphony", @file_name])
  end

  @spec fetch(Issue.t(), Path.t()) :: {:ok, String.t() | nil} | {:error, term()}
  def fetch(%Issue{} = issue, project_root) when is_binary(project_root) do
    fetch(issue, project_root, nil)
  end

  @spec fetch(Issue.t(), Path.t(), String.t() | nil) :: {:ok, String.t() | nil} | {:error, term()}
  def fetch(%Issue{} = issue, project_root, session_scope) when is_binary(project_root) do
    fetch(issue, project_root, session_scope, [])
  end

  @spec fetch(Issue.t(), Path.t(), String.t() | nil, keyword()) :: {:ok, String.t() | nil} | {:error, term()}
  def fetch(%Issue{} = issue, project_root, session_scope, opts) when is_binary(project_root) and is_list(opts) do
    with {:ok, key} <- session_key(issue, project_root, session_scope),
         {:ok, sessions} <- read_sessions(opts) do
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
    put(issue, project_root, session_id, nil)
  end

  def put(_issue, _project_root, _session_id), do: :ok

  @spec put(Issue.t(), Path.t(), String.t(), String.t() | nil) :: :ok | {:error, term()}
  def put(%Issue{} = issue, project_root, session_id, session_scope)
      when is_binary(project_root) and is_binary(session_id) and session_id != "" do
    put(issue, project_root, session_id, session_scope, [])
  end

  def put(_issue, _project_root, _session_id, _session_scope), do: :ok

  @spec put(Issue.t(), Path.t(), String.t(), String.t() | nil, keyword()) :: :ok | {:error, term()}
  def put(%Issue{} = issue, project_root, session_id, session_scope, opts)
      when is_binary(project_root) and is_binary(session_id) and session_id != "" and is_list(opts) do
    with {:ok, key} <- session_key(issue, project_root, session_scope),
         {:ok, canonical_root} <- PathSafety.canonicalize(project_root),
         {:ok, sessions} <- read_sessions(opts) do
      write_sessions(Map.put(sessions, key, session_entry(issue, canonical_root, session_id, session_scope)), opts)
    end
  end

  def put(_issue, _project_root, _session_id, _session_scope, _opts), do: :ok

  @spec prompt_scope(String.t()) :: String.t()
  def prompt_scope(prompt) when is_binary(prompt) do
    :crypto.hash(:sha256, prompt) |> Base.encode16(case: :lower)
  end

  defp session_key(%Issue{} = issue, project_root, session_scope) do
    issue_key(issue)
    |> case do
      {:ok, issue_identifier} ->
        with {:ok, canonical_root} <- PathSafety.canonicalize(project_root) do
          digest_input =
            [canonical_root, issue_identifier, normalize_scope(session_scope)]
            |> Enum.join("\0")

          {:ok, :crypto.hash(:sha256, digest_input) |> Base.encode16(case: :lower)}
        end

      {:error, reason} ->
        {:error, reason}
    end
  end

  defp issue_key(%Issue{id: issue_id})
       when is_binary(issue_id) and issue_id != "" do
    {:ok, issue_id}
  end

  defp issue_key(%Issue{identifier: identifier})
       when is_binary(identifier) and identifier != "" do
    {:ok, identifier}
  end

  defp issue_key(_issue),
    do: {:error, :opencode_acp_session_key_missing_issue_id}

  defp normalize_scope(session_scope) when is_binary(session_scope) and session_scope != "", do: session_scope
  defp normalize_scope(_session_scope), do: "legacy"

  defp session_entry(%Issue{} = issue, canonical_root, session_id, session_scope) do
    %{
      "issue_id" => issue.id,
      "issue_identifier" => issue.identifier,
      "project_root" => canonical_root,
      "session_scope" => normalize_scope(session_scope),
      "session_id" => session_id,
      "updated_at" => DateTime.utc_now() |> DateTime.to_iso8601()
    }
  end

  defp read_sessions(opts) do
    store_path = path(opts)

    case File.read(store_path) do
      {:ok, content} ->
        case Jason.decode(content) do
          {:ok, sessions} when is_map(sessions) ->
            {:ok, sessions}

          {:ok, _other} ->
            {:error, {:opencode_acp_session_store_invalid, store_path}}

          {:error, reason} ->
            {:error, {:opencode_acp_session_store_decode_failed, Exception.message(reason)}}
        end

      {:error, :enoent} ->
        {:ok, %{}}

      {:error, reason} ->
        {:error, {:opencode_acp_session_store_read_failed, reason}}
    end
  end

  defp write_sessions(sessions, opts) when is_map(sessions) do
    store_path = path(opts)
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
