# ADR-0043: Projection-backed runtime inspector

Status: accepted

## Context

Phase 8 requires a runtime debugger that exposes agents, events, context,
assumptions, tools, budgets, permissions, and cache state. The event store and
runtime service already provide a durable recovery boundary, while the TUI and
operator app were empty scaffolds.

## Decision

Build the first inspector slice as a read-only renderer over `RecoveredRun`.
`orynth inspect --db <path> --run <id>` opens the SQLite event store, recovers
the requested run, and renders the projection through `orynth-tui`. The
renderer uses bounded recent-event output and reports only persisted/recovered
state. It does not call a model provider, append events, execute tools, or
invent sample data.

## Consequences

The inspector is usable against real local runs and has a narrow side-effect
boundary that can later support interactive panes safely. Replay, fork,
comparison, semantic breakpoints, live subscriptions, and a full interactive
terminal UI remain explicit follow-up work rather than being implied by the
text view.
