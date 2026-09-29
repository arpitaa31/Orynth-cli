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

The Workspace polls this source every two seconds for offline/live observation.
Its conversation window and team rows are bounded; a selected issue is a
sidebar entry, not an independent runtime record. The two interfaces share
semantic colors through the TUI theme module. The current SQLite source
still reconstructs the selected run before bounding the displayed event
window, so incremental recovery remains a separate runtime/performance task.
Older Workspace activity pages now use a bounded indexed SQLite query and a
bounded UI cache; the selected run's initial projection still uses full
recovery, and the older-history cache is finite.

## Rationale

The previous nine-screen inspector makes users navigate runtime concepts
before they can understand the team. A separate Workspace puts the
Coordinator and worker team on one screen while preserving detailed
inspection.

## Current boundary

ADR-0069 adds durable, validated complete conversation turns to the run event
log. The offline demo records real user/Coordinator turns, but there is no
provider. Offline natural-language input therefore reports that it was not
sent or recorded. Slash commands are local inspection and navigation
controls. Live submission and streaming still require additional runtime
contracts before they can be claimed complete.

## Consequences

- The normal screen is read-only over authoritative runtime projections.
- Agent identity and effective model are shown separately.
- Worker conversation is read-only.
- Mutating actions are absent until they can pass through runtime validation
  and event persistence.
- Advanced Debugger retains the existing technical screens and raw detail.
