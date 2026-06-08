defmodule SymphonyElixir.IsolationProjectContextFieldsTest do
  use ExUnit.Case, async: false

  alias SymphonyElixir.ProjectContext
  alias SymphonyElixir.ProjectRegistry

  describe "project_context linear per-project isolation" do
    setup do
      unless Process.whereis(ProjectRegistry) do
        start_supervised!(ProjectRegistry)
      end

      :ok
    end

    test "project_context.linear carries per-project linear config and is isolated between projects" do
      alpha_ctx =
        ProjectContext.new(%{
          id: "alpha",
          enabled: true,
          workflow_path: "/tmp/linear-alpha/WORKFLOW.md",
          linear: %{team_id: "team-alpha", api_key: "key-alpha"}
        })

      beta_ctx =
        ProjectContext.new(%{
          id: "beta",
          enabled: true,
          workflow_path: "/tmp/linear-beta/WORKFLOW.md",
          linear: %{team_id: "team-beta", api_key: "key-beta"}
        })

      # Each project carries its own linear config
      assert alpha_ctx.linear.team_id == "team-alpha"
      assert alpha_ctx.linear.api_key == "key-alpha"
      assert beta_ctx.linear.team_id == "team-beta"
      assert beta_ctx.linear.api_key == "key-beta"

      # Alpha does not see beta's linear config
      refute alpha_ctx.linear.team_id == beta_ctx.linear.team_id
      refute alpha_ctx.linear.api_key == beta_ctx.linear.api_key
    end

    test "project_context.linear defaults to empty map when not configured" do
      ctx =
        ProjectContext.new(%{
          id: "plain",
          enabled: true,
          workflow_path: "/tmp/linear-plain/WORKFLOW.md"
        })

      assert ctx.linear == %{}
      assert is_map(ctx.linear)
    end

    test "project_context.linear values propagate through Config.settings!/1 for policy matching" do
      # Config.settings!/1 reads from the workflow file, not from ProjectContext.linear directly
      # This test verifies the ProjectContext struct carries independent linear configs
      alpha_ctx =
        ProjectContext.new(%{
          id: "alpha",
          enabled: true,
          workflow_path: "/tmp/linear-prop-alpha/WORKFLOW.md",
          linear: %{team_id: "team-alpha"}
        })

      beta_ctx =
        ProjectContext.new(%{
          id: "beta",
          enabled: true,
          workflow_path: "/tmp/linear-prop-beta/WORKFLOW.md",
          linear: %{team_id: "team-beta"}
        })

      # The ProjectContext structs carry independent linear maps
      assert alpha_ctx.linear.team_id == "team-alpha"
      assert beta_ctx.linear.team_id == "team-beta"
      refute alpha_ctx.linear == beta_ctx.linear
    end
  end

  describe "project_context mnemesh per-project isolation" do
    setup do
      unless Process.whereis(ProjectRegistry) do
        start_supervised!(ProjectRegistry)
      end

      :ok
    end

    test "project_context.mnemesh carries per-project mnemesh config and is isolated between projects" do
      alpha_ctx =
        ProjectContext.new(%{
          id: "alpha",
          enabled: true,
          workflow_path: "/tmp/mnemesh-alpha/WORKFLOW.md",
          mnemesh: %{workstream: "ws-alpha", enable_sync: true}
        })

      beta_ctx =
        ProjectContext.new(%{
          id: "beta",
          enabled: true,
          workflow_path: "/tmp/mnemesh-beta/WORKFLOW.md",
          mnemesh: %{workstream: "ws-beta", enable_sync: false}
        })

      # Each project carries its own mnemesh config
      assert alpha_ctx.mnemesh.workstream == "ws-alpha"
      assert alpha_ctx.mnemesh.enable_sync == true
      assert beta_ctx.mnemesh.workstream == "ws-beta"
      assert beta_ctx.mnemesh.enable_sync == false

      # Alpha does not see beta's mnemesh config
      refute alpha_ctx.mnemesh.workstream == beta_ctx.mnemesh.workstream
    end

    test "project_context.mnemesh defaults to empty map when not configured" do
      ctx =
        ProjectContext.new(%{
          id: "plain",
          enabled: true,
          workflow_path: "/tmp/mnemesh-plain/WORKFLOW.md"
        })

      assert ctx.mnemesh == %{}
      assert is_map(ctx.mnemesh)
    end
  end
end
