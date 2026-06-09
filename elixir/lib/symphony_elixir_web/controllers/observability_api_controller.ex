defmodule SymphonyElixirWeb.ObservabilityApiController do
  @moduledoc """
  JSON API for Symphony observability data.
  """

  use Phoenix.Controller, formats: [:json]

  alias Plug.Conn
  alias SymphonyElixirWeb.{Endpoint, Presenter}

  @spec state(Conn.t(), map()) :: Conn.t()
  def state(conn, _params) do
    json(conn, Presenter.state_payload(orchestrator(), snapshot_timeout_ms(), project_states_provider()))
  end

  @spec project_state(Conn.t(), map()) :: Conn.t()
  def project_state(conn, %{"project_id" => project_id}) do
    case Presenter.project_state_payload(project_id, project_states_provider(), snapshot_timeout_ms()) do
      {:ok, payload} -> json(conn, payload)
      {:error, :project_not_found} -> error_response(conn, 404, "project_not_found", "Project not found")
      {:error, :project_unavailable} -> error_response(conn, 503, "project_unavailable", "Project is unavailable")
    end
  end

  @spec issue(Conn.t(), map()) :: Conn.t()
  def issue(conn, %{"issue_identifier" => issue_identifier}) do
    case Presenter.issue_payload(issue_identifier, orchestrator(), snapshot_timeout_ms(), project_states_provider()) do
      {:ok, payload} ->
        json(conn, payload)

      {:error, :issue_not_found} ->
        error_response(conn, 404, "issue_not_found", "Issue not found")
    end
  end

  @spec project_issue(Conn.t(), map()) :: Conn.t()
  def project_issue(conn, %{"project_id" => project_id, "issue_identifier" => issue_identifier}) do
    case Presenter.project_issue_payload(
           project_id,
           issue_identifier,
           project_states_provider(),
           snapshot_timeout_ms()
         ) do
      {:ok, payload} -> json(conn, payload)
      {:error, :project_not_found} -> error_response(conn, 404, "project_not_found", "Project not found")
      {:error, :project_unavailable} -> error_response(conn, 503, "project_unavailable", "Project is unavailable")
      {:error, :issue_not_found} -> error_response(conn, 404, "issue_not_found", "Issue not found")
    end
  end

  @spec refresh(Conn.t(), map()) :: Conn.t()
  def refresh(conn, _params) do
    case Presenter.refresh_payload(orchestrator(), project_states_provider()) do
      {:ok, payload} ->
        conn
        |> put_status(202)
        |> json(payload)

      {:error, :unavailable} ->
        error_response(conn, 503, "orchestrator_unavailable", "Orchestrator is unavailable")
    end
  end

  @spec project_refresh(Conn.t(), map()) :: Conn.t()
  def project_refresh(conn, %{"project_id" => project_id}) do
    case Presenter.project_refresh_payload(project_id, project_states_provider()) do
      {:ok, payload} ->
        conn
        |> put_status(202)
        |> json(payload)

      {:error, :project_not_found} ->
        error_response(conn, 404, "project_not_found", "Project not found")

      {:error, :project_unavailable} ->
        error_response(conn, 503, "project_unavailable", "Project is unavailable")

      {:error, :unavailable} ->
        error_response(conn, 503, "project_unavailable", "Project is unavailable")
    end
  end

  @spec method_not_allowed(Conn.t(), map()) :: Conn.t()
  def method_not_allowed(conn, _params) do
    error_response(conn, 405, "method_not_allowed", "Method not allowed")
  end

  @spec not_found(Conn.t(), map()) :: Conn.t()
  def not_found(conn, _params) do
    error_response(conn, 404, "not_found", "Route not found")
  end

  defp error_response(conn, status, code, message) do
    conn
    |> put_status(status)
    |> json(%{error: %{code: code, message: message}})
  end

  defp orchestrator do
    Endpoint.config(:orchestrator) || SymphonyElixir.Orchestrator
  end

  defp snapshot_timeout_ms do
    Endpoint.config(:snapshot_timeout_ms) || 15_000
  end

  defp project_states_provider do
    Endpoint.config(:project_states_provider)
  end
end
