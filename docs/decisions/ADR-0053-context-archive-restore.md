# ADR-0053: Explicit context archive and restore transitions

Status: accepted.

## Context

The context graph already models archived blocks and reports archived counts,
but automatic archival would make a freshness policy mutate authoritative state
implicitly. The brief also requires archived state to remain recoverable without
copying context content into new prompts.

## Decision

Add versioned `Archived(ContextRef)` and `Restored(ContextRef)` transitions to
the context event schema. `ContextGraph::archive` permits only active or stale
blocks and retains their immutable content; `ContextGraph::restore` permits only
archived blocks and returns them to the active projection set. Both operations
validate references and lifecycle state, and replay applies the same checks.

`RuntimeService` exposes archive/restore methods plus artifact-store variants for
runs whose content is externalized. The runtime appends the transition only
after recovering and validating the authoritative graph. Automatic refresh,
archival, pinning, and agent-facing tools remain policy slices rather than
implicit side effects of inspection.

## Alternatives

- Archive during proprioception calculation: rejected because read-only
  inspection must not mutate runtime state.
- Delete content when archiving: rejected because archived blocks are
  recoverable references and content remains content-addressed.
- Add a separate mutable archive database: rejected because lifecycle changes
  must replay from the authoritative event stream.

## Consequences

Context lifecycle changes are observable, replayable, and durable across both
event-store backends. The graph can now support future bounded retention policy
without conflating policy decisions with projection calculation.
