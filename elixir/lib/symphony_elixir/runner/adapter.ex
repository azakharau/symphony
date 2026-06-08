defmodule SymphonyElixir.Runner.Adapter do
  @moduledoc """
  Narrow contract for Symphony issue runners.

  AgentRunner owns workspace lifecycle and shared dispatch policy. Concrete
  adapters own runner-specific session/process mechanics.
  """

  alias SymphonyElixir.Linear.Issue
  alias SymphonyElixir.Runner.Outcome

  @type capabilities :: %{
          optional(:remote_worker_hosts) => boolean()
        }

  @type context :: %{
          workspace: Path.t(),
          issue: Issue.t(),
          update_recipient: pid() | nil,
          opts: keyword(),
          worker_host: String.t() | nil,
          emit_update: (map() -> :ok)
        }

  @callback capabilities() :: capabilities()
  @callback run(context()) :: :ok | Outcome.t() | {:error, term()}
end
