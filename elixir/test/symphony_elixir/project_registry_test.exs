defmodule SymphonyElixir.ProjectRegistryTest do
  use ExUnit.Case, async: true

  alias SymphonyElixir.ProjectRegistry

  setup do
    start_supervised!(ProjectRegistry)
    :ok
  end

  test "child_spec/1 returns a valid supervisor child spec" do
    assert %{
             id: ProjectRegistry,
             start: {Registry, :start_link, [[keys: :unique, name: ProjectRegistry]]}
           } = ProjectRegistry.child_spec(ignored: true)
  end

  test "via_name/1 returns a Registry via tuple" do
    key = {:project_supervisor, "alpha"}

    assert ProjectRegistry.via_name(key) == {:via, Registry, {ProjectRegistry, key}}
  end

  test "whereis/1 returns nil for non-existent keys" do
    assert ProjectRegistry.whereis({:missing, "project"}) == nil
    refute ProjectRegistry.registered?({:missing, "project"})
  end

  test "whereis/1 returns nil when the registry is not started" do
    stop_supervised!(ProjectRegistry)

    assert ProjectRegistry.whereis({:project_supervisor, "alpha"}) == nil
    refute ProjectRegistry.registered?({:project_supervisor, "alpha"})
  end

  test "whereis/1 returns a pid for registered processes" do
    key = {:project_supervisor, "alpha"}
    {:ok, pid} = Agent.start_link(fn -> :ok end, name: ProjectRegistry.via_name(key))

    assert ProjectRegistry.whereis(key) == pid
    assert ProjectRegistry.registered?(key)
    assert Process.alive?(pid)
  end

  test "whereis/1 handles atom, tuple, and string keys" do
    keys = [:atom_key, {:symphony_project, "alpha", :workflow_store}, "string-key"]

    registered =
      for key <- keys do
        {:ok, pid} = Agent.start_link(fn -> key end, name: ProjectRegistry.via_name(key))
        {key, pid}
      end

    for {key, pid} <- registered do
      assert ProjectRegistry.whereis(key) == pid
      assert ProjectRegistry.registered?(key)
    end
  end

  test "registry isolation keeps distinct keys independent" do
    alpha_key = {:project_supervisor, "alpha"}
    beta_key = {:project_supervisor, "beta"}

    {:ok, alpha_pid} = Agent.start_link(fn -> :alpha end, name: ProjectRegistry.via_name(alpha_key))
    {:ok, beta_pid} = Agent.start_link(fn -> :beta end, name: ProjectRegistry.via_name(beta_key))

    assert ProjectRegistry.whereis(alpha_key) == alpha_pid
    assert ProjectRegistry.whereis(beta_key) == beta_pid
    assert alpha_pid != beta_pid
  end

  test "registry uniqueness rejects duplicate keys and keeps the existing pid" do
    key = {:project_supervisor, "alpha"}
    {:ok, existing_pid} = Agent.start_link(fn -> :existing end, name: ProjectRegistry.via_name(key))

    assert {:error, {:already_started, ^existing_pid}} =
             Agent.start_link(fn -> :duplicate end, name: ProjectRegistry.via_name(key))

    assert ProjectRegistry.whereis(key) == existing_pid
  end

  test "whereis/1 returns nil after a registered process exits" do
    key = {:symphony_project, "alpha", :workflow_store}
    {:ok, pid} = Agent.start_link(fn -> :ok end, name: ProjectRegistry.via_name(key))

    assert ProjectRegistry.whereis(key) == pid

    Agent.stop(pid)

    assert eventually(fn -> ProjectRegistry.whereis(key) == nil end)
    refute ProjectRegistry.registered?(key)
  end

  defp eventually(fun, attempts \\ 20)

  defp eventually(_fun, 0), do: false

  defp eventually(fun, attempts) do
    if fun.() do
      true
    else
      Process.sleep(10)
      eventually(fun, attempts - 1)
    end
  end
end
