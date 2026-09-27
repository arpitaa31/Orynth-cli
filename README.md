# Orynth

Orynth is an experimental Rust project exploring supervised agent-runtime architecture.

Status: chat-first Workspace redesign in progress. Deep Audit #2 has not started. See [docs/STATUS.md](docs/STATUS.md) and
[plans/CURRENT.md](plans/CURRENT.md).

## Workspace

Run the deterministic offline team demo:

```text
cargo run -p orynth -- --demo
```

Inspect persisted SQLite runs:

```text
cargo run -p orynth --
cargo run -p orynth -- --db .orynth/runtime.db --run <run-id>
```

Open Advanced Debugger with `cargo run -p orynth -- debug --demo`. The TUI is
read-only and projection-backed. Advanced Debugger shows the agent tree,
logical/effective model identity, bounded event timeline and paging, context
visibility, IPC, tool transactions, permissions/ownership, budgets,
assumptions/conflicts, cache observations, persisted runs, and help. The
existing `inspect`, `replay`, `fork`, `diff`, and line-oriented `debug-session` commands
remain available for non-full-screen workflows. See [docs/TUI.md](docs/TUI.md)
and [docs/TUI_IMPLEMENTATION_REPORT.md](docs/TUI_IMPLEMENTATION_REPORT.md).
