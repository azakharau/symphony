defmodule SymphonyElixirWeb.DashboardLive do
  @moduledoc """
  Live observability dashboard for Symphony.
  """

  use Phoenix.LiveView, layout: {SymphonyElixirWeb.Layouts, :app}

  alias SymphonyElixirWeb.{Endpoint, ObservabilityPubSub, Presenter}
  @runtime_tick_ms 1_000

  @impl true
  def mount(params, _session, socket) do
    payload = load_payload()

    socket =
      socket
      |> assign(:payload, payload)
      |> assign(:project_id, Map.get(params, "project_id"))
      |> assign(:project_payload, load_project_payload(Map.get(params, "project_id"), payload))
      |> assign(:now, DateTime.utc_now())

    if connected?(socket) do
      :ok = ObservabilityPubSub.subscribe()
      schedule_runtime_tick()
    end

    {:ok, socket}
  end

  @impl true
  def handle_info(:runtime_tick, socket) do
    schedule_runtime_tick()
    {:noreply, assign(socket, :now, DateTime.utc_now())}
  end

  @impl true
  def handle_info(:observability_updated, socket) do
    {:noreply,
     socket
     |> reload_payload()
     |> assign(:now, DateTime.utc_now())}
  end

  @impl true
  def render(assigns) do
    ~H"""
    <section class="dashboard-shell">
      <header class="hero-card">
        <div class="hero-grid">
          <div>
            <p class="eyebrow">
              Symphony Observability
            </p>
            <h1 class="hero-title">
              Operations Dashboard
            </h1>
            <p class="hero-copy">
              Current state, retry pressure, token usage, and orchestration health for the active Symphony runtime.
            </p>
          </div>

          <div class="status-stack">
            <span class="status-badge status-badge-live">
              <span class="status-badge-dot"></span>
              Live
            </span>
            <span class="status-badge status-badge-offline">
              <span class="status-badge-dot"></span>
              Offline
            </span>
          </div>
        </div>
      </header>

      <%= if @payload[:error] do %>
        <section class="error-card">
          <h2 class="error-title">
            Snapshot unavailable
          </h2>
          <p class="error-copy">
            <strong><%= @payload.error.code %>:</strong> <%= @payload.error.message %>
          </p>
        </section>
      <% else %>
        <section class="metric-grid">
          <article class="metric-card">
            <p class="metric-label">Running</p>
            <p class="metric-value numeric"><%= @payload.counts.running %></p>
            <p class="metric-detail">Active issue sessions in the current runtime.</p>
          </article>

          <article :if={Map.get(@payload, :projects)} class="metric-card">
            <p class="metric-label">Active projects</p>
            <p class="metric-value numeric"><%= format_int(get_in(@payload, [:attention, :active_projects])) %></p>
            <p class="metric-detail">Configured projects currently enabled for runtime work.</p>
          </article>

          <article :if={Map.get(@payload, :projects)} class="metric-card">
            <p class="metric-label">Needs attention</p>
            <p class="metric-value numeric"><%= format_int(attention_total(@payload)) %></p>
            <p class="metric-detail">Blocked, review, owner input, RCA, stale, and recent failure signals.</p>
          </article>

          <article class="metric-card">
            <p class="metric-label">Retrying</p>
            <p class="metric-value numeric"><%= @payload.counts.retrying %></p>
            <p class="metric-detail">Issues waiting for the next retry window.</p>
          </article>

          <article class="metric-card">
            <p class="metric-label">Blocked</p>
            <p class="metric-value numeric"><%= @payload.counts.blocked %></p>
            <p class="metric-detail">Issues paused for operator input or approval.</p>
          </article>

          <article class="metric-card">
            <p class="metric-label">Total tokens</p>
            <p class="metric-value numeric"><%= format_int(@payload.codex_totals.total_tokens) %></p>
            <p class="metric-detail numeric">
              In <%= format_int(@payload.codex_totals.input_tokens) %> / Out <%= format_int(@payload.codex_totals.output_tokens) %>
            </p>
          </article>

          <article class="metric-card">
            <p class="metric-label">Runner runtime</p>
            <p class="metric-value numeric"><%= format_runtime_seconds(total_runtime_seconds(@payload, @now)) %></p>
            <p class="metric-detail">Total elapsed runtime across completed and active runner sessions.</p>
          </article>
        </section>

        <section class="section-card">
          <div class="section-header">
            <div>
              <p class="eyebrow">Stewardship</p>
              <h2 class="section-title"><%= dispatch_state_label(@payload) %></h2>
              <p class="section-copy"><%= dispatch_reason(@payload) %></p>
            </div>
          </div>
          <div class="project-summary-grid">
            <article class="summary-pill">
              <span>Milestone</span>
              <strong><%= payload_milestone_name(@payload) %></strong>
            </article>
            <article class="summary-pill">
              <span>Eligible</span>
              <strong class="numeric"><%= stewardship_count(@payload, :eligible_issue_count) %></strong>
            </article>
            <article class="summary-pill">
              <span>Running</span>
              <strong class="numeric"><%= stewardship_count(@payload, :running_count) %></strong>
            </article>
            <article class="summary-pill">
              <span>Retrying</span>
              <strong class="numeric"><%= stewardship_count(@payload, :retrying_count) %></strong>
            </article>
            <article class="summary-pill">
              <span>Owner/blocked</span>
              <strong class="numeric"><%= stewardship_count(@payload, :owner_input_count) + stewardship_count(@payload, :blocked_count) %></strong>
            </article>
          </div>
          <h3 class="subsection-title">Recent suppression reasons</h3>
          <%= render_item_list(recent_suppression_items(@payload), "No recent suppression events in runtime state.") %>
        </section>

        <%= if @project_payload do %>
          <section class="section-card project-drilldown-card">
            <div class="section-header">
              <div>
                <p class="eyebrow">Project drilldown</p>
                <h2 class="section-title"><%= @project_payload.project.name || @project_payload.project.id %></h2>
                <p class="section-copy">
                  <%= project_state_summary(@project_payload) %>
                </p>
              </div>
              <a class="subtle-link" href="/">Back to overview</a>
            </div>

            <div class="project-summary-grid">
              <article class="summary-pill">
                <span>Milestone</span>
                <strong><%= active_milestone_name(@project_payload.project) %></strong>
              </article>
              <article class="summary-pill">
                <span>Queue</span>
                <strong class="numeric"><%= format_int(@project_payload.project.queue_depth) %></strong>
              </article>
              <article class="summary-pill">
                <span>Review</span>
                <strong class="numeric"><%= format_int(@project_payload.project.review_count) %></strong>
              </article>
              <article class="summary-pill">
                <span>Owner/RCA</span>
                <strong class="numeric"><%= format_int((@project_payload.project.owner_input_count || 0) + (@project_payload.project.rca_required_count || 0)) %></strong>
              </article>
              <article class="summary-pill">
                <span>Cleanup</span>
                <strong><%= cleanup_label(@project_payload.project.cleanup_status) %></strong>
              </article>
            </div>

            <%= if @project_payload[:drilldown_error] do %>
              <section class="error-card">
                <h3 class="error-title">Project snapshot unavailable</h3>
                <p class="error-copy">
                  <strong><%= @project_payload.drilldown_error.code %>:</strong> <%= @project_payload.drilldown_error.message %>
                </p>
              </section>
            <% end %>

            <div class="drilldown-grid">
              <div>
                <h3 class="subsection-title">Issue queue</h3>
                <%= render_item_list(@project_payload.issue_queue, "No queued issues in runtime state.") %>
              </div>
              <div>
                <h3 class="subsection-title">Dependency blocked</h3>
                <%= render_item_list(@project_payload.dependency_blocked_items, "No dependency-blocked issues in runtime state.") %>
              </div>
              <div>
                <h3 class="subsection-title">Review / owner attention</h3>
                <%= render_item_list(@project_payload.review_items ++ @project_payload.owner_input_items ++ @project_payload.rca_required_items, "No review, owner input, or RCA items in runtime state.") %>
              </div>
              <div>
                <h3 class="subsection-title">Stale / failures</h3>
                <%= render_item_list(@project_payload.stale_states ++ @project_payload.recent_failures, "No stale states or recent failures in runtime state.") %>
              </div>
              <div>
                <h3 class="subsection-title">Recent handoff / acceptance activity</h3>
                <%= render_item_list(@project_payload.recent_activity, "No recent activity in runtime state.") %>
              </div>
              <div>
                <h3 class="subsection-title">Cleanup health</h3>
                <%= render_item_list(cleanup_items(@project_payload.project.cleanup_status), "No cleanup warnings in runtime state.") %>
              </div>
            </div>
          </section>
        <% end %>

        <%= if Map.get(@payload, :projects) do %>
          <section class="section-card">
            <div class="section-header">
              <div>
                <h2 class="section-title">Active project overview</h2>
                <p class="section-copy">Health, queue pressure, milestone, and drilldowns for every configured project.</p>
              </div>
            </div>

            <div class="table-wrap">
              <table class="data-table" style="min-width: 980px;">
                <thead>
                  <tr>
                    <th>Project</th>
                    <th>Status</th>
                    <th>Milestone</th>
                    <th>Runner</th>
                    <th>Health</th>
                    <th>Counts</th>
                    <th>Attention</th>
                    <th>Drilldown</th>
                  </tr>
                </thead>
                <tbody>
                  <tr :for={project <- @payload.projects}>
                    <td>
                      <div class="detail-stack">
                        <span class="issue-id"><%= project.name || project.id %></span>
                        <span class="muted event-meta"><%= project.id %></span>
                      </div>
                    </td>
                    <td><span class={state_badge_class(project.status)}><%= project.status %></span></td>
                    <td><%= active_milestone_name(project) %></td>
                    <td><%= project.runner_kind || "n/a" %></td>
                    <td><%= project.worker_health || "n/a" %></td>
                    <td class="numeric">
                      <%= project.counts.running %> running / <%= project.counts.retrying %> retrying / <%= project.counts.blocked %> blocked
                    </td>
                    <td>
                      <div class="detail-stack">
                        <span class="numeric"><%= attention_summary(project) %></span>
                        <div class="reason-chip-row">
                          <span :for={reason <- attention_reasons(project)} class="reason-chip"><%= reason %></span>
                        </div>
                      </div>
                    </td>
                    <td>
                      <div class="detail-stack">
                        <a class="issue-link" href={"/projects/#{project.id}"}>Project dashboard</a>
                        <a class="issue-link" href={"/api/v1/projects/#{project.id}/state"}>Project JSON</a>
                        <span class="muted event-meta">Refresh API: POST only</span>
                      </div>
                    </td>
                  </tr>
                </tbody>
              </table>
            </div>
          </section>
        <% end %>

        <section class="section-card">
          <div class="section-header">
            <div>
              <h2 class="section-title">Rate limits</h2>
              <p class="section-copy">Latest upstream rate-limit snapshot, when available.</p>
            </div>
          </div>

          <pre class="code-panel"><%= pretty_value(@payload.rate_limits) %></pre>
        </section>

        <section class="section-card">
          <div class="section-header">
            <div>
              <h2 class="section-title">Running sessions</h2>
              <p class="section-copy">Active issues, last known agent activity, and token usage.</p>
            </div>
          </div>

          <%= if @payload.running == [] do %>
            <p class="empty-state">No active sessions.</p>
          <% else %>
            <div class="table-wrap">
              <table class="data-table data-table-running">
                <colgroup>
                  <col style="width: 12rem;" />
                  <col style="width: 8rem;" />
                  <col style="width: 9rem;" />
                  <col style="width: 7.5rem;" />
                  <col style="width: 8.5rem;" />
                  <col />
                  <col style="width: 10rem;" />
                </colgroup>
                <thead>
                  <tr>
                    <th>Issue</th>
                    <th>State</th>
                    <th>Runner</th>
                    <th>Session</th>
                    <th>Runtime / turns</th>
                    <th>Runner update</th>
                    <th>Tokens</th>
                  </tr>
                </thead>
                <tbody>
                  <tr :for={entry <- @payload.running}>
                    <td>
                      <div class="issue-stack">
                        <span class="issue-id"><%= entry.issue_identifier %></span>
                        <a class="issue-link" href={"/api/v1/#{entry.issue_identifier}"}>JSON details</a>
                      </div>
                    </td>
                    <td>
                      <span class={state_badge_class(entry.state)}>
                        <%= entry.state %>
                      </span>
                    </td>
                    <td>
                      <div class="detail-stack">
                        <span><%= runner_owner(entry) %></span>
                        <span class="muted event-meta"><%= runner_phase(entry) %></span>
                      </div>
                    </td>
                    <td>
                      <div class="session-stack">
                        <%= if entry.session_id do %>
                          <button
                            type="button"
                            class="subtle-button"
                            data-label="Copy ID"
                            data-copy={entry.session_id}
                            onclick="navigator.clipboard.writeText(this.dataset.copy); this.textContent = 'Copied'; clearTimeout(this._copyTimer); this._copyTimer = setTimeout(() => { this.textContent = this.dataset.label }, 1200);"
                          >
                            Copy ID
                          </button>
                        <% else %>
                          <span class="muted">n/a</span>
                        <% end %>
                      </div>
                    </td>
                    <td class="numeric"><%= format_runtime_and_turns(entry.started_at, entry.turn_count, @now) %></td>
                    <td>
                      <div class="detail-stack">
                        <span
                          class="event-text"
                          title={entry.last_runner_message || to_string(entry.last_runner_event || "n/a")}
                        ><%= entry.last_runner_message || to_string(entry.last_runner_event || "n/a") %></span>
                        <span class="muted event-meta">
                          <%= entry.last_runner_event || "n/a" %>
                          <%= if entry.last_runner_event_at do %>
                            · <span class="mono numeric"><%= entry.last_runner_event_at %></span>
                          <% end %>
                        </span>
                      </div>
                    </td>
                    <td>
                      <div class="token-stack numeric">
                        <span>Total: <%= format_int(entry.tokens.total_tokens) %></span>
                        <span class="muted">In <%= format_int(entry.tokens.input_tokens) %> / Out <%= format_int(entry.tokens.output_tokens) %></span>
                      </div>
                    </td>
                  </tr>
                </tbody>
              </table>
            </div>
          <% end %>
        </section>

        <section class="section-card">
          <div class="section-header">
            <div>
              <h2 class="section-title">Blocked sessions</h2>
              <p class="section-copy">Issues paused because a runner needs operator input or hit a policy block.</p>
            </div>
          </div>

          <%= if @payload.blocked == [] do %>
            <p class="empty-state">No blocked sessions.</p>
          <% else %>
            <div class="table-wrap">
              <table class="data-table" style="min-width: 840px;">
                <thead>
                  <tr>
                    <th>Issue</th>
                    <th>State</th>
                    <th>Runner</th>
                    <th>Session</th>
                    <th>Blocked at</th>
                    <th>Last update</th>
                    <th>Error</th>
                  </tr>
                </thead>
                <tbody>
                  <tr :for={entry <- @payload.blocked}>
                    <td>
                      <div class="issue-stack">
                        <span class="issue-id"><%= entry.issue_identifier %></span>
                        <a class="issue-link" href={"/api/v1/#{entry.issue_identifier}"}>JSON details</a>
                      </div>
                    </td>
                    <td>
                      <span class={state_badge_class(entry.state || "Blocked")}>
                        <%= entry.state || "Blocked" %>
                      </span>
                    </td>
                    <td>
                      <div class="detail-stack">
                        <span><%= runner_owner(entry) %></span>
                        <span class="muted event-meta"><%= runner_phase(entry) %></span>
                      </div>
                    </td>
                    <td>
                      <%= if entry.session_id do %>
                        <button
                          type="button"
                          class="subtle-button"
                          data-label="Copy ID"
                          data-copy={entry.session_id}
                          onclick="navigator.clipboard.writeText(this.dataset.copy); this.textContent = 'Copied'; clearTimeout(this._copyTimer); this._copyTimer = setTimeout(() => { this.textContent = this.dataset.label }, 1200);"
                        >
                          Copy ID
                        </button>
                      <% else %>
                        <span class="muted">n/a</span>
                      <% end %>
                    </td>
                    <td class="mono"><%= entry.blocked_at || "n/a" %></td>
                    <td>
                      <div class="detail-stack">
                        <span
                          class="event-text"
                          title={entry.last_runner_message || to_string(entry.last_runner_event || "n/a")}
                        ><%= entry.last_runner_message || to_string(entry.last_runner_event || "n/a") %></span>
                        <span class="muted event-meta">
                          <%= entry.last_runner_event || "n/a" %>
                          <%= if entry.last_runner_event_at do %>
                            · <span class="mono numeric"><%= entry.last_runner_event_at %></span>
                          <% end %>
                        </span>
                      </div>
                    </td>
                    <td><%= entry.error || "n/a" %></td>
                  </tr>
                </tbody>
              </table>
            </div>
          <% end %>
        </section>

        <section class="section-card">
          <div class="section-header">
            <div>
              <h2 class="section-title">Retry queue</h2>
              <p class="section-copy">Issues waiting for the next retry window.</p>
            </div>
          </div>

          <%= if @payload.retrying == [] do %>
            <p class="empty-state">No issues are currently backing off.</p>
          <% else %>
            <div class="table-wrap">
              <table class="data-table" style="min-width: 680px;">
                <thead>
                  <tr>
                    <th>Issue</th>
                    <th>Attempt</th>
                    <th>Due at</th>
                    <th>Error</th>
                  </tr>
                </thead>
                <tbody>
                  <tr :for={entry <- @payload.retrying}>
                    <td>
                      <div class="issue-stack">
                        <span class="issue-id"><%= entry.issue_identifier %></span>
                        <a class="issue-link" href={"/api/v1/#{entry.issue_identifier}"}>JSON details</a>
                      </div>
                    </td>
                    <td><%= entry.attempt %></td>
                    <td class="mono"><%= entry.due_at || "n/a" %></td>
                    <td><%= entry.error || "n/a" %></td>
                  </tr>
                </tbody>
              </table>
            </div>
          <% end %>
        </section>
      <% end %>
    </section>
    """
  end

  defp load_payload do
    Presenter.state_payload(orchestrator(), snapshot_timeout_ms(), project_states_provider())
  end

  defp reload_payload(socket) do
    payload = load_payload()

    socket
    |> assign(:payload, payload)
    |> assign(:project_payload, load_project_payload(socket.assigns.project_id, payload))
  end

  defp load_project_payload(nil, _payload), do: nil

  defp load_project_payload(project_id, payload) do
    case Presenter.project_state_payload(
           project_id,
           project_states_provider(),
           snapshot_timeout_ms()
         ) do
      {:ok, %{error: error}} ->
        project_from_aggregate_payload(project_id, payload, error)

      {:ok, project_payload} ->
        normalize_project_payload(project_payload)

      {:error, _reason} ->
        project_from_aggregate_payload(
          project_id,
          payload,
          aggregate_project_error(project_id, payload)
        )
    end
  end

  defp normalize_project_payload(%{project: project} = payload) do
    project = fallback_project(project)

    payload
    |> Map.put(:project, project)
    |> Map.put_new(:counts, fallback_counts(project))
    |> put_new_optional_drilldown_lists()
    |> put_dependency_blocked_count()
  end

  defp normalize_project_payload(%{projects: [project | _]} = payload) do
    payload
    |> Map.put(:project, project)
    |> normalize_project_payload()
  end

  defp project_from_aggregate_payload(project_id, payload, drilldown_error) do
    case Enum.find(Map.get(payload, :projects, []), &(&1.id == project_id)) do
      nil ->
        nil

      project ->
        %{project: project}
        |> normalize_project_payload()
        |> maybe_put_drilldown_error(drilldown_error)
    end
  end

  defp maybe_put_drilldown_error(payload, nil), do: payload
  defp maybe_put_drilldown_error(payload, error), do: Map.put(payload, :drilldown_error, error)

  defp aggregate_project_error(project_id, payload) do
    payload
    |> Map.get(:projects, [])
    |> Enum.find(&(&1.id == project_id))
    |> case do
      %{error: error} -> error
      _ -> nil
    end
  end

  defp put_new_optional_drilldown_lists(payload) do
    [
      :issue_queue,
      :dependency_blocked_items,
      :review_items,
      :owner_input_items,
      :rca_required_items,
      :stale_states,
      :recent_failures,
      :recent_activity
    ]
    |> Enum.reduce(payload, &Map.put_new(&2, &1, []))
  end

  defp put_dependency_blocked_count(payload) do
    dependency_count = payload |> Map.get(:dependency_blocked_items, []) |> length()

    if dependency_count > 0 and
         number_value(Map.get(payload.project, :dependency_blocked_count)) == 0 do
      update_in(payload, [:project], &Map.put(&1, :dependency_blocked_count, dependency_count))
    else
      payload
    end
  end

  defp fallback_counts(project) do
    Map.get(project, :counts) || %{running: 0, retrying: 0, blocked: 0}
  end

  defp fallback_project(project) do
    project
    |> Map.put_new(:counts, %{running: 0, retrying: 0, blocked: 0})
    |> Map.put_new(:queue_depth, 0)
    |> Map.put_new(:review_count, 0)
    |> Map.put_new(:owner_input_count, 0)
    |> Map.put_new(:rca_required_count, 0)
    |> Map.put_new(:stale_count, 0)
    |> Map.put_new(:dependency_blocked_count, 0)
    |> Map.put_new(:recent_failure_count, 0)
    |> Map.put_new(:cleanup_status, %{})
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

  defp completed_runtime_seconds(payload) do
    runtime_totals = Map.get(payload, :runner_runtime_totals)
    codex_totals = Map.get(payload, :codex_totals, %{})

    Map.get(runtime_totals || %{}, :seconds_running) ||
      Map.get(codex_totals || %{}, :seconds_running, 0)
  end

  defp total_runtime_seconds(payload, now) do
    completed_runtime_seconds(payload) +
      Enum.reduce(payload.running, 0, fn entry, total ->
        total + runtime_seconds_from_started_at(entry.started_at, now)
      end)
  end

  defp format_runtime_and_turns(started_at, turn_count, now)
       when is_integer(turn_count) and turn_count > 0 do
    "#{format_runtime_seconds(runtime_seconds_from_started_at(started_at, now))} / #{turn_count}"
  end

  defp format_runtime_and_turns(started_at, _turn_count, now),
    do: format_runtime_seconds(runtime_seconds_from_started_at(started_at, now))

  defp format_runtime_seconds(seconds) when is_number(seconds) do
    whole_seconds = max(trunc(seconds), 0)
    mins = div(whole_seconds, 60)
    secs = rem(whole_seconds, 60)
    "#{mins}m #{secs}s"
  end

  defp runtime_seconds_from_started_at(%DateTime{} = started_at, %DateTime{} = now) do
    DateTime.diff(now, started_at, :second)
  end

  defp runtime_seconds_from_started_at(started_at, %DateTime{} = now)
       when is_binary(started_at) do
    case DateTime.from_iso8601(started_at) do
      {:ok, parsed, _offset} -> runtime_seconds_from_started_at(parsed, now)
      _ -> 0
    end
  end

  defp runtime_seconds_from_started_at(_started_at, _now), do: 0

  defp format_int(value) when is_integer(value) do
    value
    |> Integer.to_string()
    |> String.reverse()
    |> String.replace(~r/.{3}(?=.)/, "\\0,")
    |> String.reverse()
  end

  defp format_int(_value), do: "n/a"

  defp dispatch_state_label(payload) do
    payload
    |> get_in([:dispatch_summary, :dispatch_state])
    |> case do
      nil -> "No dispatch summary"
      state -> state |> to_string() |> String.replace("_", " ") |> String.capitalize()
    end
  end

  defp dispatch_reason(payload) do
    get_in(payload, [:dispatch_summary, :reason]) || "Dispatch state is not available in this snapshot."
  end

  defp stewardship_count(payload, key) do
    number_value(get_in(payload, [:dispatch_summary, key]) || get_in(payload, [:stewardship, key]))
  end

  defp payload_milestone_name(payload) do
    payload
    |> get_in([:stewardship, :active_milestone])
    |> case do
      %{milestone_name: name} when is_binary(name) -> name
      %{"milestone_name" => name} when is_binary(name) -> name
      _ -> active_milestone_name(payload)
    end
  end

  defp recent_suppression_items(payload) do
    payload
    |> Map.get(:suppression_events, [])
    |> Enum.take(5)
  end

  defp attention_total(payload) do
    attention = Map.get(payload, :attention, %{})

    [:blocked, :in_review, :owner_input, :rca_required, :stale, :recent_failures]
    |> Enum.reduce(0, fn key, total -> total + number_value(Map.get(attention, key)) end)
  end

  defp attention_summary(project) do
    [
      {"queue", Map.get(project, :queue_depth)},
      {"deps", Map.get(project, :dependency_blocked_count)},
      {"review", Map.get(project, :review_count)},
      {"owner", Map.get(project, :owner_input_count)},
      {"RCA", Map.get(project, :rca_required_count)},
      {"stale", Map.get(project, :stale_count)},
      {"failures", Map.get(project, :recent_failure_count)}
    ]
    |> Enum.map_join(" / ", fn {label, value} -> "#{number_value(value)} #{label}" end)
  end

  defp project_state_summary(%{project: project, counts: counts}) do
    reasons = attention_reasons(project)

    cond do
      counts.running > 0 ->
        "Running #{counts.running} active session(s): #{reason_sentence(reasons, "latest runner activity is shown below")}."

      counts.blocked > 0 ->
        "Blocked on #{counts.blocked} runtime session(s): #{reason_sentence(reasons, "operator input or policy block details are shown below")}."

      counts.retrying > 0 ->
        "Retrying #{counts.retrying} issue(s): recent failure or due-at details are shown below."

      reasons != [] ->
        "Needs attention: #{Enum.join(reasons, "; ")}."

      true ->
        "Idle: no active, blocked, retrying, review, owner-input, RCA, stale, failure, dependency, or cleanup signals are present."
    end
  end

  defp active_milestone_name(%{active_milestone: %{name: name}}), do: name
  defp active_milestone_name(%{active_milestone: %{"name" => name}}), do: name

  defp active_milestone_name(%{active_milestone: milestone}) when is_binary(milestone),
    do: milestone

  defp active_milestone_name(_project), do: "n/a"

  defp cleanup_label(nil), do: "n/a"
  defp cleanup_label(value) when map_size(value) == 0, do: "n/a"

  defp cleanup_label(value) when is_map(value) do
    if cleanup_attempt_evidence?(value), do: inspect(value, pretty: true, limit: 4), else: "n/a"
  end

  defp cleanup_label(value), do: inspect(value, pretty: true, limit: 4)

  defp attention_reasons(project) do
    [
      count_reason("running", project_count(project, :running)),
      count_reason("runtime blocked", project_count(project, :blocked)),
      count_reason("retrying", project_count(project, :retrying)),
      count_reason("dependency blocked", Map.get(project, :dependency_blocked_count)),
      count_reason("waiting review", Map.get(project, :review_count)),
      count_reason("owner input", Map.get(project, :owner_input_count)),
      count_reason("RCA", Map.get(project, :rca_required_count)),
      count_reason("stale", Map.get(project, :stale_count)),
      count_reason("recent failure", Map.get(project, :recent_failure_count)),
      cleanup_reason(Map.get(project, :cleanup_status))
    ]
    |> Enum.reject(&is_nil/1)
  end

  defp count_reason(label, value) do
    count = number_value(value)

    if count > 0, do: "#{format_int(count)} #{label}", else: nil
  end

  defp project_count(project, key) do
    project
    |> Map.get(:counts)
    |> Kernel.||(%{})
    |> map_value(key)
  end

  defp cleanup_reason(value) when is_map(value) and map_size(value) == 0, do: nil
  defp cleanup_reason(nil), do: nil

  defp cleanup_reason(value) when is_map(value) do
    if cleanup_attempt_evidence?(value) do
      cleanup_evidence_reason(value)
    else
      nil
    end
  end

  defp cleanup_reason(value), do: "cleanup #{cleanup_label(value)}"

  defp cleanup_evidence_reason(value) when is_map(value) do
    status = map_value(value, :status) || map_value(value, :result) || map_value(value, :outcome)
    problem = map_value(value, :error) || map_value(value, :failure) || map_value(value, :problem)

    cond do
      problem -> "cleanup #{display_value(problem)}"
      cleanup_problem_status?(status) -> "cleanup #{display_value(status)}"
      true -> nil
    end
  end

  defp cleanup_problem_status?(value) when value in [nil, "", :ok, :healthy, :success], do: false

  defp cleanup_problem_status?(value) when is_binary(value),
    do: String.downcase(value) not in ["ok", "healthy", "success"]

  defp cleanup_problem_status?(_value), do: true

  defp reason_sentence([], fallback), do: fallback
  defp reason_sentence(reasons, _fallback), do: Enum.join(reasons, "; ")

  defp cleanup_items(value) when is_map(value) and map_size(value) > 0 do
    if cleanup_attempt_evidence?(value), do: [cleanup_item(value)], else: []
  end

  defp cleanup_items(_value), do: []

  defp cleanup_item(value) do
    %{
      identifier: "cleanup",
      status: map_value(value, :status) || map_value(value, :result) || "reported",
      error: map_value(value, :error) || map_value(value, :failure),
      last_attempt: map_value(value, :last_attempt),
      result: value
    }
  end

  defp cleanup_attempt_evidence?(value) when is_map(value) do
    cleanup_attempts(value) != []
  end

  defp cleanup_attempts(value) when is_map(value) do
    attempts =
      value
      |> map_value(:attempts)
      |> case do
        attempts when is_list(attempts) -> attempts
        _ -> []
      end

    case map_value(value, :last_attempt) do
      attempt when is_map(attempt) -> Enum.uniq([attempt | attempts])
      _ -> attempts
    end
  end

  defp render_item_list(items, empty_text) do
    assigns = %{items: items, empty_text: empty_text}

    ~H"""
    <%= if @items == [] do %>
      <p class="empty-state"><%= @empty_text %></p>
    <% else %>
      <ul class="compact-list reason-list">
        <li :for={item <- @items}>
          <span class="item-heading"><%= item_label(item) %></span>
          <span :for={reason <- item_reason_parts(item)} class="item-reason"><%= reason %></span>
        </li>
      </ul>
    <% end %>
    """
  end

  defp item_label(item) when is_map(item) do
    map_value(item, :issue_identifier) || map_value(item, :identifier) || map_value(item, :title) ||
      map_value(item, :event) || inspect(item, pretty: true, limit: 4)
  end

  defp item_label(item), do: to_string(item)

  defp item_reason_parts(item) when is_map(item) do
    (item_status_reasons(item) ++ item_activity_reasons(item))
    |> Enum.reject(&is_nil/1)
  end

  defp item_reason_parts(_item), do: []

  defp item_status_reasons(item) do
    [
      labeled_value("state", map_value(item, :state) || map_value(item, :status)),
      labeled_value("blocker", item_blocker_value(item)),
      labeled_value("reason", map_value(item, :block_reason) || map_value(item, :reason)),
      labeled_value("error", map_value(item, :error) || map_value(item, :failure)),
      labeled_value("age", age_reason(item))
    ]
  end

  defp item_activity_reasons(item) do
    [
      labeled_value("event", map_value(item, :event)),
      labeled_value("message", map_value(item, :message)),
      labeled_value("cleanup", map_value(item, :cleanup_result) || map_value(item, :result)),
      labeled_value("last attempt", map_value(item, :last_attempt)),
      labeled_value("updated", item_updated_value(item))
    ]
  end

  defp item_blocker_value(item) do
    map_value(item, :blocker) || map_value(item, :blocked_by) || map_value(item, :dependency) ||
      map_value(item, :blockers)
  end

  defp item_updated_value(item), do: map_value(item, :updated_at) || map_value(item, :at) || map_value(item, :due_at)

  defp labeled_value(_label, value) when value in [nil, ""], do: nil
  defp labeled_value(label, value), do: "#{label}: #{display_value(value)}"

  defp age_reason(item) do
    age = map_value(item, :age_ms)
    timeout = map_value(item, :timeout_ms)

    cond do
      is_number(age) and is_number(timeout) ->
        "#{format_duration_ms(age)} / timeout #{format_duration_ms(timeout)}"

      is_number(age) ->
        format_duration_ms(age)

      is_number(timeout) ->
        "timeout #{format_duration_ms(timeout)}"

      true ->
        nil
    end
  end

  defp format_duration_ms(ms) when is_number(ms) do
    seconds = div(trunc(ms), 1_000)

    cond do
      seconds >= 86_400 -> "#{div(seconds, 86_400)}d"
      seconds >= 3_600 -> "#{div(seconds, 3_600)}h"
      seconds >= 60 -> "#{div(seconds, 60)}m"
      true -> "#{seconds}s"
    end
  end

  defp display_value(value) when is_binary(value), do: value
  defp display_value(value) when is_atom(value), do: Atom.to_string(value)
  defp display_value(%DateTime{} = value), do: DateTime.to_iso8601(value)
  defp display_value(value), do: inspect(value, pretty: true, limit: 4)

  defp map_value(map, key) when is_map(map) do
    Map.get(map, key) || Map.get(map, Atom.to_string(key))
  end

  defp number_value(value) when is_number(value), do: value
  defp number_value(_value), do: 0

  defp state_badge_class(state) do
    base = "state-badge"
    normalized = state |> to_string() |> String.downcase()

    cond do
      String.contains?(normalized, ["progress", "running", "active"]) ->
        "#{base} state-badge-active"

      String.contains?(normalized, ["blocked", "error", "failed"]) ->
        "#{base} state-badge-danger"

      String.contains?(normalized, ["todo", "queued", "pending", "retry"]) ->
        "#{base} state-badge-warning"

      true ->
        base
    end
  end

  defp runner_owner(entry) when is_map(entry) do
    runner_value(entry, :owner) || runner_value(entry, :kind) || "n/a"
  end

  defp runner_owner(_entry), do: "n/a"

  defp runner_phase(entry) when is_map(entry) do
    runner_value(entry, :phase) || "starting"
  end

  defp runner_phase(_entry), do: "starting"

  defp runner_value(entry, field) do
    nested_runner_value(Map.get(entry, :runner), field) ||
      Map.get(entry, legacy_runner_key(field))
  end

  defp nested_runner_value(runner, field) when is_map(runner) do
    Map.get(runner, field) || Map.get(runner, to_string(field))
  end

  defp nested_runner_value(_runner, _field), do: nil

  defp legacy_runner_key(:kind), do: :runner_kind
  defp legacy_runner_key(:owner), do: :runner_owner
  defp legacy_runner_key(:phase), do: :runner_phase

  defp schedule_runtime_tick do
    Process.send_after(self(), :runtime_tick, @runtime_tick_ms)
  end

  defp pretty_value(nil), do: "n/a"
  defp pretty_value(value), do: inspect(value, pretty: true, limit: :infinity)
end
