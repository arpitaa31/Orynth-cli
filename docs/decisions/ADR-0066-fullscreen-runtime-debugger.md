# ADR-0066: Full-screen runtime debugger boundary

## Status

Accepted for Phase F.

## Decision

Implement the full-screen TUI as a read-only client over a caller-supplied
`TuiDataSource` and authoritative `RecoveredRun` projection. The TUI may own
selection, filtering, screen state, bounded event pages, and terminal
lifecycle, but it may not append runtime events, call providers, execute tool
effects, grant permissions, or maintain a second runtime authority.

The SQLite application source lists persisted runs, recovers the selected
projection through `RuntimeService`, and supplies older bounded event pages on
demand. The deterministic demo uses the same runtime event/projection path in
memory so it is useful offline without pretending to be a live provider run.

## Rationale

The event store and runtime projection are already the durable recovery
boundary. A separate UI state model would make displayed agent, model,
permission, context, IPC, and tool state capable of diverging from replay.
Keeping controls read-only also avoids implying that a terminal keypress is a
validated runtime mutation before the runtime-control API and authorization
semantics exist.

## Consequences

- The TUI can inspect all currently projected runtime domains without provider
  calls or side effects.
- Run selection and refresh are explicit and testable through a small source
  trait.
- Event history is bounded in memory and older pages are requested only when
  the operator asks for them.
- Pause/resume/cancel, live model changes, tool approval, replay/fork actions,
  and live breakpoint controls remain deferred rather than being simulated.
- Terminal cleanup is centralized in an RAII guard so input/draw errors do not
  leave raw mode enabled.
