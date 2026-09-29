# ADR-0069: Durable Coordinator conversation turns

## Status

Accepted for the Phase F.3 Workspace.

## Decision

User and root-Coordinator turns are versioned opaque `ConversationTurn` events
in the existing run event log. The runtime owns a bounded UTF-8 codec and
validates turns before append. Coordinator speakers must be members of the run
and have no scheduler parent; a worker cannot impersonate the Coordinator.
The enclosing event owns run scope, so recorded replay and fork keep the turn
without a second chat database or an embedded run ID to remap.

Recovery validates every stored turn. The Workspace renders turns in the same
Coordinator pane as deterministic coordination activity. Older turns use the
event source's bounded page API. The offline demo persists a user goal and a
Coordinator conflict summary derived from the recovered conflict record.

## Boundary

This is a complete-turn contract, not a provider or a streaming transcript.
Offline free-form input is still reported as unsent. A future provider stream
can refer to the starting event ID when adding chunk and completion events,
then project them into the same message blocks. That stream contract, live
submission, and source-backed paging beyond the current bounded cache remain
separate work. The selected SQLite run is still fully reconstructed on refresh.

## Rationale

The user conversation belongs to the authoritative run history so it can
survive restart and be inspected alongside team state. An opaque versioned
payload keeps the dependency-light kernel from owning transcript semantics.
