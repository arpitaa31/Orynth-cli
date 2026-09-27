# ADR-0068: Chat-first Workspace beside Advanced Debugger

## Status

Accepted for the in-progress Workspace redesign.

## Decision

The primary `orynth` and `orynth --demo` commands open a conversation-first
Workspace. `orynth debug` opens the existing full-screen runtime inspector as
Advanced Debugger. The in-app command palette and `/debug` can switch from the
Workspace to the same debugger instance; `Ctrl+W` returns. The older line
debug session remains available as `orynth debug-session`.

Both modes receive `TuiSnapshot` from the same `TuiDataSource`. The Workspace
derives its team, agent views, issues, and coordination activity from
`RecoveredRun`. It owns focus, selection, input editing, scroll position, and
overlays only. No UI state is treated as runtime authority.

## Rationale

The previous nine-screen inspector makes users navigate runtime concepts
before they can understand the team. A separate Workspace puts the
Coordinator and worker team on one screen while preserving detailed
inspection.

## Current boundary

There is no durable Coordinator conversation event or provider in the runtime
yet. Offline natural-language input therefore reports that it was not sent or
recorded. Slash commands are local inspection and navigation controls.
Conversation submission, persistence, replay, and streaming require an
explicit runtime contract before they can be claimed complete.

## Consequences

- The normal screen is read-only over authoritative runtime projections.
- Agent identity and effective model are shown separately.
- Worker conversation is read-only.
- Mutating actions are absent until they can pass through runtime validation
  and event persistence.
- Advanced Debugger retains the existing technical screens and raw detail.
