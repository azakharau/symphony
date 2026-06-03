defmodule SymphonyElixir.PulseLedgerTest do
  use SymphonyElixir.TestSupport

  alias SymphonyElixir.PulseLedger

  defp ledger_path(name) do
    Path.join(System.tmp_dir!(), "symphony-pulse-ledger-#{name}-#{System.unique_integer([:positive])}.json")
  end

  defp start_ledger(path) do
    name = Module.concat([SymphonyElixir.PulseLedger.Test, String.to_atom("temp#{System.unique_integer([:positive])}")])

    {PulseLedger, name: name, file_path: path}
    |> Supervisor.child_spec(id: name)
    |> start_supervised!()
  end

  test "start_link/1 with temp path starts successfully" do
    path = ledger_path("start")
    on_exit(fn -> File.rm(path) end)

    pid = start_ledger(path)

    assert Process.alive?(pid)
  end

  test "owner_input_processed?/2 returns false for unknown and true after record" do
    path = ledger_path("owner-input")
    on_exit(fn -> File.rm(path) end)
    ledger = start_ledger(path)

    refute PulseLedger.owner_input_processed?(ledger, "issue-1:comment-1")
    assert :ok = PulseLedger.record_owner_input(ledger, "issue-1:comment-1")
    assert PulseLedger.owner_input_processed?(ledger, "issue-1:comment-1")
  end

  test "done_continuation_processed?/2 returns false for unknown and true after record" do
    path = ledger_path("done-continuation")
    on_exit(fn -> File.rm(path) end)
    ledger = start_ledger(path)

    refute PulseLedger.done_continuation_processed?(ledger, "issue-1:updated")
    assert :ok = PulseLedger.record_done_continuation(ledger, "issue-1:updated")
    assert PulseLedger.done_continuation_processed?(ledger, "issue-1:updated")
  end

  test "set_active_milestone + active_milestone + clear_active_milestone" do
    path = ledger_path("milestone")
    on_exit(fn -> File.rm(path) end)
    ledger = start_ledger(path)

    assert :ok = PulseLedger.set_active_milestone(ledger, "milestone-1", "Milestone One")

    assert %{
             "milestone_id" => "milestone-1",
             "milestone_name" => "Milestone One",
             "locked_at" => locked_at
           } = PulseLedger.active_milestone(ledger)

    assert is_binary(locked_at)
    assert :ok = PulseLedger.clear_active_milestone(ledger)
    assert PulseLedger.active_milestone(ledger) == nil
  end

  test "record_suppression appends events and increments counts" do
    path = ledger_path("suppression")
    on_exit(fn -> File.rm(path) end)
    ledger = start_ledger(path)

    assert :ok = PulseLedger.record_suppression(ledger, "owner_wait_no_change", "issue-1", "SYM-1", nil, nil, "already processed")
    assert :ok = PulseLedger.record_suppression(ledger, "owner_wait_no_change", "issue-1", "SYM-1", nil, nil, "already processed")

    assert [%{"kind" => "owner_wait_no_change", "count" => 1}, %{"kind" => "owner_wait_no_change", "count" => 2}] =
             PulseLedger.suppression_events(ledger)

    assert %{"owner_wait_no_change" => 2} = PulseLedger.suppression_counts(ledger)
  end

  test "persistence across restart writes and reloads state" do
    path = ledger_path("restart")
    on_exit(fn -> File.rm(path) end)
    ledger = start_ledger(path)

    assert :ok = PulseLedger.record_owner_input(ledger, "owner-key")
    assert :ok = PulseLedger.record_done_continuation(ledger, "done-key")
    assert :ok = PulseLedger.set_active_milestone(ledger, "milestone-1", "Milestone One")
    assert :ok = PulseLedger.record_execution_packet(ledger, %{"issue" => %{"id" => "issue-1"}, "packet_version" => "test"})
    assert :ok = PulseLedger.record_acceptance(ledger, "issue-1", %{"status" => "accepted", "docs_checked" => true})
    assert :ok = PulseLedger.record_active_milestone_closure(ledger, %{"milestone_id" => "milestone-1", "reason" => "all_known_child_issues_terminal"})
    assert PulseLedger.active_milestone_reactivation_blocked_id(ledger) == "milestone-1"
    assert :ok = PulseLedger.clear_active_milestone_reactivation_block(ledger)
    assert PulseLedger.active_milestone_reactivation_blocked_id(ledger) == nil
    assert :ok = PulseLedger.record_active_milestone_closure(ledger, %{"milestone_id" => "milestone-1", "reason" => "all_known_child_issues_terminal"})
    assert :ok = PulseLedger.record_handoff_fingerprint(ledger, "handoff-fp")
    assert :ok = PulseLedger.record_suppression(ledger, "kind", nil, nil, "milestone-1", "Milestone One", "reason")

    GenServer.stop(ledger)

    {:ok, pid} = PulseLedger.start_link(file_path: path)
    on_exit(fn -> if Process.alive?(pid), do: GenServer.stop(pid) end)

    assert PulseLedger.owner_input_processed?(pid, "owner-key")
    assert PulseLedger.done_continuation_processed?(pid, "done-key")
    assert %{"milestone_id" => "milestone-1"} = PulseLedger.active_milestone(pid)
    assert %{"packet_version" => "test"} = PulseLedger.execution_packet(pid, "issue-1")
    assert %{"status" => "accepted", "docs_checked" => true} = PulseLedger.acceptance_record(pid, "issue-1")
    assert %{"reason" => "all_known_child_issues_terminal"} = PulseLedger.active_milestone_closure(pid, "milestone-1")
    assert PulseLedger.active_milestone_reactivation_blocked_id(pid) == "milestone-1"
    assert PulseLedger.handoff_fingerprint_seen?(pid, "handoff-fp")
    assert %{"kind" => 1} = PulseLedger.suppression_counts(pid)
  end

  test "durable execution packet is stored before worker dispatch can close" do
    path = ledger_path("execution-packet")
    on_exit(fn -> File.rm(path) end)
    ledger = start_ledger(path)

    packet = %{
      "packet_version" => "symphony:execution-packet:v1",
      "issue" => %{"id" => "issue-packet", "identifier" => "SYM-41"},
      "active_milestone" => %{"id" => "milestone-current", "name" => "Current"}
    }

    assert :ok = PulseLedger.record_execution_packet(ledger, packet)
    assert packet == PulseLedger.execution_packet(ledger, "issue-packet")
  end

  test "unchanged worker handoff fingerprint suppresses repeated acceptance dispatch" do
    path = ledger_path("handoff-fingerprint")
    on_exit(fn -> File.rm(path) end)
    ledger = start_ledger(path)

    fingerprint = :crypto.hash(:sha256, "same handoff") |> Base.encode16(case: :lower)

    refute PulseLedger.handoff_fingerprint_seen?(ledger, fingerprint)
    assert :ok = PulseLedger.record_handoff_fingerprint(ledger, fingerprint)
    assert PulseLedger.handoff_fingerprint_seen?(ledger, fingerprint)
  end

  test "handoff and acceptance suppression categories are public ledger constants" do
    assert PulseLedger.handoff_unchanged() == "handoff_unchanged"
    assert PulseLedger.acceptance_already_processed() == "acceptance_already_processed"
  end

  test "accepted work requires durable acceptance record before closure" do
    path = ledger_path("acceptance-record")
    on_exit(fn -> File.rm(path) end)
    ledger = start_ledger(path)

    assert PulseLedger.acceptance_record(ledger, "issue-accepted") == nil

    assert :ok =
             PulseLedger.record_acceptance(ledger, "issue-accepted", %{
               "status" => "accepted",
               "slice_id" => "slice-1",
               "docs_checked" => true
             })

    assert %{"status" => "accepted", "docs_checked" => true, "recorded_at" => recorded_at} =
             PulseLedger.acceptance_record(ledger, "issue-accepted")

    assert is_binary(recorded_at)
  end

  test "missing file on init creates empty state gracefully" do
    path = ledger_path("missing")
    File.rm(path)
    on_exit(fn -> File.rm(path) end)
    ledger = start_ledger(path)

    refute PulseLedger.owner_input_processed?(ledger, "missing")
    assert PulseLedger.suppression_events(ledger) == []
    assert PulseLedger.suppression_counts(ledger) == %{}
  end

  test "corrupt file on init logs warning and creates empty state" do
    path = ledger_path("corrupt")
    File.write!(path, "not-json")
    on_exit(fn -> File.rm(path) end)

    log =
      capture_log(fn ->
        ledger = start_ledger(path)
        refute PulseLedger.owner_input_processed?(ledger, "missing")
      end)

    assert log =~ "Failed to decode PulseLedger"
  end

  test "suppression_events bounded to 100" do
    path = ledger_path("bounded")
    on_exit(fn -> File.rm(path) end)
    ledger = start_ledger(path)

    for index <- 1..101 do
      assert :ok = PulseLedger.record_suppression(ledger, "kind", "issue-#{index}", "SYM-#{index}", nil, nil, "reason")
    end

    events = PulseLedger.suppression_events(ledger)
    assert length(events) == 100
    refute Enum.any?(events, &(&1["issue_id"] == "issue-1"))
    assert List.last(events)["issue_id"] == "issue-101"
  end

  test "reset/1 clears all state" do
    path = ledger_path("reset")
    on_exit(fn -> File.rm(path) end)
    ledger = start_ledger(path)

    assert :ok = PulseLedger.record_owner_input(ledger, "owner-key")
    assert :ok = PulseLedger.record_done_continuation(ledger, "done-key")
    assert :ok = PulseLedger.set_active_milestone(ledger, "milestone-1", "Milestone One")
    assert :ok = PulseLedger.record_suppression(ledger, "kind", nil, nil, nil, nil, "reason")

    assert :ok = PulseLedger.reset(ledger)
    refute PulseLedger.owner_input_processed?(ledger, "owner-key")
    refute PulseLedger.done_continuation_processed?(ledger, "done-key")
    assert PulseLedger.active_milestone(ledger) == nil
    assert PulseLedger.active_milestone_reactivation_blocked_id(ledger) == nil
    assert PulseLedger.suppression_events(ledger) == []
    assert PulseLedger.suppression_counts(ledger) == %{}
  end

  test "two ledgers with different paths must not share owner_input_processed state" do
    path1 = ledger_path("iso-1")
    path2 = ledger_path("iso-2")

    on_exit(fn ->
      File.rm(path1)
      File.rm(path2)
    end)

    ledger1 = start_ledger(path1)
    ledger2 = start_ledger(path2)

    assert :ok = PulseLedger.record_owner_input(ledger1, "issue-1:comment-1")
    assert PulseLedger.owner_input_processed?(ledger1, "issue-1:comment-1")
    refute PulseLedger.owner_input_processed?(ledger2, "issue-1:comment-1")
  end

  test "two ledgers with different paths must not share done_continuation state" do
    path1 = ledger_path("iso-dc-1")
    path2 = ledger_path("iso-dc-2")

    on_exit(fn ->
      File.rm(path1)
      File.rm(path2)
    end)

    ledger1 = start_ledger(path1)
    ledger2 = start_ledger(path2)

    assert :ok = PulseLedger.record_done_continuation(ledger1, "issue-1:2026-01-01")
    assert PulseLedger.done_continuation_processed?(ledger1, "issue-1:2026-01-01")
    refute PulseLedger.done_continuation_processed?(ledger2, "issue-1:2026-01-01")
  end

  test "two ledgers with different paths must not share active_milestone state" do
    path1 = ledger_path("iso-am-1")
    path2 = ledger_path("iso-am-2")

    on_exit(fn ->
      File.rm(path1)
      File.rm(path2)
    end)

    ledger1 = start_ledger(path1)
    ledger2 = start_ledger(path2)

    assert :ok = PulseLedger.set_active_milestone(ledger1, "ms-1", "Milestone One")
    assert %{"milestone_id" => "ms-1"} = PulseLedger.active_milestone(ledger1)
    assert PulseLedger.active_milestone(ledger2) == nil
  end

  test "two ledgers must not share suppression_counts" do
    path1 = ledger_path("iso-sc-1")
    path2 = ledger_path("iso-sc-2")

    on_exit(fn ->
      File.rm(path1)
      File.rm(path2)
    end)

    ledger1 = start_ledger(path1)
    ledger2 = start_ledger(path2)

    assert :ok = PulseLedger.record_suppression(ledger1, "owner_wait_no_change", "i1", "SYM-1", nil, nil, "test")
    assert %{"owner_wait_no_change" => 1} = PulseLedger.suppression_counts(ledger1)
    assert PulseLedger.suppression_counts(ledger2) == %{}
  end

  test "pending_owner_input without commit is lost on restart (not persisted)" do
    path = ledger_path("pending-oi")
    on_exit(fn -> File.rm(path) end)
    ledger = start_ledger(path)

    assert :ok = PulseLedger.pending_owner_input(ledger, "issue-1:pending")
    refute PulseLedger.owner_input_processed?(ledger, "issue-1:pending")
    assert PulseLedger.has_pending?(ledger)

    GenServer.stop(ledger)

    {:ok, pid2} = PulseLedger.start_link(file_path: path)
    on_exit(fn -> if Process.alive?(pid2), do: GenServer.stop(pid2) end)

    refute PulseLedger.owner_input_processed?(pid2, "issue-1:pending")
  end

  test "pending_owner_input after commit is persisted across restart" do
    path = ledger_path("pending-oi-commit")
    on_exit(fn -> File.rm(path) end)
    ledger = start_ledger(path)

    assert :ok = PulseLedger.pending_owner_input(ledger, "issue-1:will-commit")
    refute PulseLedger.owner_input_processed?(ledger, "issue-1:will-commit")
    assert :ok = PulseLedger.commit_pending_owner_input(ledger, "issue-1:will-commit")
    assert PulseLedger.owner_input_processed?(ledger, "issue-1:will-commit")

    GenServer.stop(ledger)

    {:ok, pid2} = PulseLedger.start_link(file_path: path)
    on_exit(fn -> if Process.alive?(pid2), do: GenServer.stop(pid2) end)

    assert PulseLedger.owner_input_processed?(pid2, "issue-1:will-commit")
  end

  test "rollback_pending clears pending marks without persisting" do
    path = ledger_path("rollback")
    on_exit(fn -> File.rm(path) end)
    ledger = start_ledger(path)

    assert :ok = PulseLedger.pending_owner_input(ledger, "issue-1:rollback-me")
    assert PulseLedger.has_pending?(ledger)
    assert :ok = PulseLedger.rollback_pending(ledger)
    refute PulseLedger.has_pending?(ledger)
    refute PulseLedger.owner_input_processed?(ledger, "issue-1:rollback-me")
  end

  test "commit is idempotent when mark not pending" do
    path = ledger_path("commit-idempotent")
    on_exit(fn -> File.rm(path) end)
    ledger = start_ledger(path)

    assert :ok = PulseLedger.commit_pending_owner_input(ledger, "never-pending")
    refute PulseLedger.owner_input_processed?(ledger, "never-pending")
  end

  test "pending_done_continuation after commit is persisted across restart" do
    path = ledger_path("pending-dc")
    on_exit(fn -> File.rm(path) end)
    ledger = start_ledger(path)

    assert :ok = PulseLedger.pending_done_continuation(ledger, "issue-2:2026-06-01")
    assert :ok = PulseLedger.commit_pending_done_continuation(ledger, "issue-2:2026-06-01")
    assert PulseLedger.done_continuation_processed?(ledger, "issue-2:2026-06-01")

    GenServer.stop(ledger)

    {:ok, pid2} = PulseLedger.start_link(file_path: path)
    on_exit(fn -> if Process.alive?(pid2), do: GenServer.stop(pid2) end)

    assert PulseLedger.done_continuation_processed?(pid2, "issue-2:2026-06-01")
  end

  test "stable suppression name functions return expected strings" do
    assert PulseLedger.owner_wait_no_change() == "owner_wait_no_change"
    assert PulseLedger.done_continuation_already_processed() == "done_continuation_already_processed"
    assert PulseLedger.active_milestone_locked() == "active_milestone_locked"
    assert PulseLedger.next_milestone_scan_suppressed() == "next_milestone_scan_suppressed"
  end
end
