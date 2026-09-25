# Orynth

Orynth is an experimental Rust project exploring supervised agent-runtime architecture.

Status: Phase F full-screen TUI/runtime debugger implemented. Phase E
benchmark and hardening evidence remains the baseline; the next planned step
is Deep Audit #2. See [docs/STATUS.md](docs/STATUS.md) and
[plans/CURRENT.md](plans/CURRENT.md).

## Runtime debugger

Run the deterministic offline control-room demo:

```text
cargo run -p orynth -- tui --demo
```

Inspect persisted SQLite runs:

```text
cargo run -p orynth -- tui --db .orynth/runtime.db
cargo run -p orynth -- tui --db .orynth/runtime.db --run <run-id>
```

The TUI is read-only and projection-backed. It shows the agent tree,
logical/effective model identity, bounded event timeline and paging, context
visibility, IPC, tool transactions, permissions/ownership, budgets,
assumptions/conflicts, cache observations, persisted runs, and help. The
existing `inspect`, `replay`, `fork`, `diff`, and terminal `debug` commands
remain available for non-full-screen workflows. See [docs/TUI.md](docs/TUI.md)
and [docs/TUI_IMPLEMENTATION_REPORT.md](docs/TUI_IMPLEMENTATION_REPORT.md).
