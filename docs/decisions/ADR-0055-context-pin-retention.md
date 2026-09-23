# ADR-0055: Explicit Context Pin Retention Metadata

Status: accepted.

## Context

Context blocks are immutable content references whose lifecycle is controlled
by durable transitions. Operators and future retention policies need to protect
important blocks from automatic archival without making pinning silently alter
projection, freshness, or content semantics.

## Decision

Represent pinning as explicit `Pinned` and `Unpinned` context transitions. A
pin is valid for an active or stale block, is replay-validated, and is exposed
in bounded dashboard summaries. It does not change lifecycle, content, trust,
or prompt visibility. Runtime pin/unpin operations recover the authoritative
graph and append the transition through the normal event-store boundary.

## Consequences

- retention intent is durable, inspectable, and replayable;
- automatic archival may later consult pins without mutating the graph from a
  read-only dashboard;
- archived and invalidated blocks cannot be newly pinned through this API;
- pin policy and agent-facing context tools remain separate future layers.
