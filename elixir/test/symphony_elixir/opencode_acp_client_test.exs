defmodule SymphonyElixir.OpenCodeACPClientTest do
  use SymphonyElixir.TestSupport

  alias SymphonyElixir.OpenCode.ACPClient

  test "initializes over ndjson and records negotiated capabilities" do
    {:ok, client} = start_client("default")

    assert {:ok, %{"capabilities" => caps}} = ACPClient.initialize(client)
    assert caps["session/new"] == true
    assert ACPClient.capability?(client, "session/prompt")
    assert ACPClient.capabilities(client)["session/cancel"] == true
  end

  test "extracts nested initialize capability shapes" do
    {:ok, client} = start_client("nested-caps")

    assert {:ok, %{"agentCapabilities" => _caps}} = ACPClient.initialize(client)
    assert ACPClient.capability?(client, "session/new")
    assert ACPClient.capability?(client, "session/load")
    assert ACPClient.capability?(client, "session/resume")
    assert ACPClient.capability?(client, "session/prompt")
    assert ACPClient.capability?(client, "session/cancel")
  end

  test "returns deterministic error when acp command is not on path" do
    assert {:error, {:acp_command_not_found, "definitely-not-opencode-acp"}} =
             ACPClient.start_link(
               command: "definitely-not-opencode-acp",
               args: [],
               cwd: File.cwd!(),
               handler: self(),
               permission_policy: "reject",
               read_timeout_ms: 50
             )
  end

  test "creates a session with absolute project root cwd" do
    root = File.cwd!()
    {:ok, client} = start_client("default", cwd: root)
    assert {:ok, _} = ACPClient.initialize(client)

    assert {:ok, %{"sessionId" => "new-session", "cwd" => ^root, "processCwd" => ^root}} =
             ACPClient.new_session(client, %{"cwd" => root})
  end

  test "loads an existing session before sending a prompt" do
    {:ok, client} = start_client("default")
    assert {:ok, _} = ACPClient.initialize(client)

    assert {:ok, %{"sessionId" => "existing-session", "loaded" => true}} =
             ACPClient.load_session(client, %{"sessionId" => "existing-session"})

    assert {:ok, %{"loadedBeforePrompt" => true}} =
             ACPClient.prompt(client, %{"sessionId" => "existing-session", "prompt" => "continue"})
  end

  test "prefers session resume when advertised" do
    {:ok, client} = start_client("default")
    assert {:ok, _} = ACPClient.initialize(client)
    assert ACPClient.capability?(client, "session/resume")

    assert {:ok, %{"sessionId" => "existing-session", "resumed" => true}} =
             ACPClient.resume_session(client, %{"sessionId" => "existing-session"})
  end

  test "streams agent text tool plan and usage updates" do
    {:ok, client} = start_client("stream", handler: self())
    assert {:ok, _} = ACPClient.initialize(client)
    assert {:ok, _} = ACPClient.new_session(client, %{"cwd" => File.cwd!()})

    assert {:ok, %{"stopReason" => "end_turn"}} =
             ACPClient.prompt(client, %{"sessionId" => "new-session", "prompt" => "work"})

    assert_received {:acp_notification, "session/update", %{"type" => "agent_text", "text" => "hello"}}

    assert_received {:acp_notification, "session/update", %{"type" => "tool_plan", "tool" => "edit"}}

    assert_received {:acp_notification, "session/update", %{"type" => "usage", "usage" => %{"input" => 1}}}
  end

  test "responds to permission requests according to policy" do
    {:ok, client} = start_client("permission", handler: self(), permission_policy: "reject")
    assert {:ok, _} = ACPClient.initialize(client)

    assert_receive {:acp_request, "session/request_permission", %{"reason" => "edit"}}, 1_000
    assert_receive {:acp_notification, "permission_observed", %{"outcome" => "reject"}}
  end

  test "times out pending requests without hanging" do
    {:ok, client} = start_client("timeout")
    assert {:ok, _} = ACPClient.initialize(client)

    assert {:error, :timeout} =
             ACPClient.prompt(client, %{"sessionId" => "new-session", "prompt" => "hang"}, 20)
  end

  test "installed opencode acp initializes over stdio when available" do
    case System.find_executable("opencode") do
      nil ->
        :ok

      opencode ->
        assert {:ok, client} =
                 ACPClient.start_link(
                   command: opencode,
                   args: ["acp", "--pure", "--cwd", File.cwd!()],
                   cwd: File.cwd!(),
                   handler: self(),
                   permission_policy: "reject",
                   read_timeout_ms: 15_000
                 )

        assert {:ok, %{"agentInfo" => %{"name" => "OpenCode"}, "agentCapabilities" => capabilities}} =
                 ACPClient.initialize(client, %{"protocolVersion" => 1}, 15_000)

        assert is_map(capabilities)
        assert ACPClient.capability?(client, "session/load")
        assert ACPClient.capability?(client, "session/resume")
        assert ACPClient.capability?(client, "session/prompt")
        ACPClient.stop(client)
    end
  end

  defp start_client(scenario, opts \\ []) do
    script = fake_acp_server!()
    python = System.find_executable("python3") || System.find_executable("python")

    ACPClient.start_link(
      command: python,
      args: [script, scenario],
      cwd: Keyword.get(opts, :cwd, File.cwd!()),
      handler: Keyword.get(opts, :handler, self()),
      permission_policy: Keyword.get(opts, :permission_policy, "reject"),
      read_timeout_ms: 50
    )
  end

  defp fake_acp_server! do
    path = Path.join(System.tmp_dir!(), "fake-acp-#{System.unique_integer([:positive])}.py")

    File.write!(path, ~S'''
    import json, os, sys
    scenario = sys.argv[1] if len(sys.argv) > 1 else "default"
    loaded = False

    def send(obj):
        sys.stdout.write(json.dumps(obj) + "\n")
        sys.stdout.flush()

    for line in sys.stdin:
        msg = json.loads(line)
        if "method" not in msg:
            if msg.get("id") == 99:
                send({"jsonrpc":"2.0","method":"permission_observed","params":msg.get("result",{})})
            continue
        mid = msg["id"]
        method = msg["method"]
        params = msg.get("params", {})
        caps = {"session/new": True, "session/load": True, "session/resume": True, "session/prompt": True, "session/cancel": True}
        if method == "initialize":
            if scenario == "nested-caps":
                send({"jsonrpc":"2.0","id":mid,"result":{"agentCapabilities":{"loadSession":True,"promptCapabilities":{"embeddedContext":True},"sessionCapabilities":{"new":{"supported":True},"resume":{},"close":{}}}}})
            else:
                send({"jsonrpc":"2.0","id":mid,"result":{"capabilities":caps,"argv":sys.argv[1:],"cwd":os.getcwd()}})
            if scenario == "permission":
                send({"jsonrpc":"2.0","id":99,"method":"session/request_permission","params":{"reason":"edit"}})
        elif method == "session/new":
            send({"jsonrpc":"2.0","id":mid,"result":{"sessionId":"new-session","cwd":params.get("cwd"),"processCwd":os.getcwd()}})
        elif method == "session/load":
            loaded = True
            send({"jsonrpc":"2.0","id":mid,"result":{"sessionId":params.get("sessionId"),"loaded":True}})
        elif method == "session/resume":
            send({"jsonrpc":"2.0","id":mid,"result":{"sessionId":params.get("sessionId"),"resumed":True}})
        elif method == "session/prompt" and scenario == "timeout":
            pass
        elif method == "session/prompt":
            if scenario == "stream":
                send({"jsonrpc":"2.0","method":"session/update","params":{"type":"agent_text","text":"hello"}})
                send({"jsonrpc":"2.0","method":"session/update","params":{"type":"tool_plan","tool":"edit"}})
                send({"jsonrpc":"2.0","method":"session/update","params":{"type":"usage","usage":{"input":1}}})
            send({"jsonrpc":"2.0","method":"session/update","params":{"type":"end_turn"}})
            send({"jsonrpc":"2.0","id":mid,"result":{"stopReason":"end_turn","loadedBeforePrompt":loaded}})
        elif method == "session/cancel":
            send({"jsonrpc":"2.0","id":mid,"result":{"cancelled":True}})
    ''')

    path
  end
end
