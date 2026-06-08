defmodule SymphonyElixir.LiveWorkerDockerAuthTest do
  use ExUnit.Case, async: true

  @support_dir Path.expand("../support/live_e2e_docker", __DIR__)

  test "live worker docker auth mounts Codex auth for root SSH worker" do
    compose = File.read!(Path.join(@support_dir, "docker-compose.yml"))
    entrypoint = File.read!(Path.join(@support_dir, "live_worker_entrypoint.sh"))

    assert compose =~ "${SYMPHONY_LIVE_DOCKER_AUTH_JSON}:/root/.codex/auth.json:ro"
    refute compose =~ ":/home/agent/.codex/auth.json:ro"
    assert entrypoint =~ "install -d -m 700 /root/.ssh /root/.codex"
  end
end
