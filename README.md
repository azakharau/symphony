# Symphony

Symphony turns project work into isolated, autonomous implementation runs, allowing teams to manage
work instead of supervising coding agents.

[![Symphony demo video preview](.github/media/symphony-demo-poster.jpg)](https://player.vimeo.com/video/1186371009?h=5626e4b899)

_In this [demo video](https://player.vimeo.com/video/1186371009?h=5626e4b899), Symphony monitors a Linear board for work and spawns agents to handle the tasks. The agents complete the tasks and provide proof of work: CI status, PR review feedback, complexity analysis, and walkthrough videos. When accepted, the agents land the PR safely. Engineers do not need to supervise Codex; they can manage the work at a higher level._

> [!WARNING]
> Symphony is a low-key engineering preview for testing in trusted environments.

## Running Symphony

### Requirements

Symphony works best in codebases that have adopted
[harness engineering](https://openai.com/index/harness-engineering/). Symphony is the next step --
moving from managing coding agents to managing work that needs to get done.

### Option 1. Make your own

Tell your favorite coding agent to build Symphony in a programming language of your choice:

> Implement Symphony according to the following spec:
> https://github.com/openai/symphony/blob/main/SPEC.md

### Option 2. Use our experimental reference implementation

Check out [elixir/README.md](elixir/README.md) for instructions on how to set up your environment
and run the Elixir-based Symphony implementation. You can also ask your favorite coding agent to
help with the setup:

> Set up Symphony for my repository based on
> https://github.com/openai/symphony/blob/main/elixir/README.md

### Rust vNext foundation

The Rust vNext workspace is an additive foundation for the next Symphony runtime. It currently
contains typed multiproject config loading, OpenCode-only project runtime settings, SQLite runtime
state bootstrap, restart-safe state queries, mocked Linear polling orchestration, and a basic
validation CLI.

The vNext state machine treats `Backlog` as planning inventory, keeps blocked `Todo` issues and
parked `Need Owner Input` issues out of dispatch, moves only eligible work to `In Progress` through
Symphony's Linear writer path, and records deterministic OpenCode session state so a restart does
not duplicate a dispatch.

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

The CLI can validate a root project config or initialize an empty runtime store:

```bash
cargo run -p symphony-vnext -- validate-config --config /path/to/projects.yml
cargo run -p symphony-vnext -- init-store --database /path/to/runtime.sqlite3
cargo run -p symphony-vnext -- daemon --config /path/to/projects.yml --database /path/to/runtime.sqlite3 --once
```

---

## License

This project is licensed under the [Apache License 2.0](LICENSE).
