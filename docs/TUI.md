# Orynth TUI

Status: Phase F full-screen runtime debugger implemented.

The TUI is a read-only client over authoritative `RecoveredRun` projections.
It does not call providers, append events, execute tools, grant permissions,
or create a second runtime state model. SQLite sources recover the selected
run through `RuntimeService`; the offline demo creates the same event-backed
projection in memory.

## Launch

```text
cargo run -p orynth -- tui --demo
cargo run -p orynth -- tui --db .orynth/runtime.db
cargo run -p orynth -- tui --db .orynth/runtime.db --run <run-id>
```

`--demo` is deterministic and offline. It contains a manager, three
specialists, a model switch, context, IPC, an assumption conflict, health,
budgets, ownership, a capability lease, cache telemetry, and a tool
transaction. Set `ORYNTH_TUI_DEMO_AGENTS=10` for the bounded ten-agent
responsiveness scenario used by the Phase F measurement.

## Views

The numbered views are dashboard, agents, events, context, IPC/messages,
tools, policy/permissions, assumptions/conflicts, and persisted runs. The
dashboard combines the agent tree, selected-agent projection, and recent
events. Agent rows retain the logical `AgentId` while displaying the current
model assignment. Context uses a visibility-scoped `ContextPrincipal` and
bounded projection limits. Details are bounded by character/row limits.

The events view displays a bounded newest window. `[` requests an older page
from the SQLite event store and `]` returns to the newest window. Paging is
read-only and does not reconstruct or mutate a live run. Replay, fork, and
diff remain available through the existing CLI commands and are not live TUI
mutations.

## Controls

`Tab`/`Shift-Tab` changes views; `1`-`9` selects a view; arrows or `j`/`k`
move selection; `Home`/`End` jump; `Enter` selects a run or opens event
details; `/` filters the active list; `r` refreshes authoritative state; `R`
opens persisted runs; `?` opens help; `Esc` closes details/help; `q` or
`Ctrl-C` exits. The TUI restores raw mode, the alternate screen, and cursor
visibility on normal exit, input errors, and panic unwinding through RAII.

The interface remains read-only. Interactive pause, resume, cancel, model
switch, capability grant, tool approval, replay execution, fork execution,
and live breakpoint actions are deferred until a safe runtime-control API is
defined.
