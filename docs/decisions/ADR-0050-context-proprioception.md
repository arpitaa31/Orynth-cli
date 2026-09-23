# ADR-0050: Read-only context freshness proprioception

Status: accepted.

## Context

The runtime already tracks lifecycle, token estimates, importance, and
invalidation history for context blocks. Agents and operators need a compact
dashboard to notice stale pressure and large active blocks without receiving a
full context graph or allowing model output to mutate authoritative state.

## Decision

Add `ContextFreshnessPolicy` and `ContextGraph::proprioception`. The bounded
report counts active/stale/archived/invalidated blocks, sums active token
estimates, returns deterministic largest-block summaries and recent
invalidations, and exposes explicit active-token and stale-count pressure
flags. Limits are caller-supplied and report-only.

## Alternatives

- Let a model archive or invalidate blocks directly: rejected because context
  lifecycle remains runtime-authoritative.
- Copy the entire graph into each agent prompt: rejected because it violates
  bounded context and memory goals.
- Infer freshness from wall-clock age alone: deferred because access timestamps
  are not consistently populated and lifecycle transitions are stronger
  evidence.

## Consequences

Freshness pressure is observable and deterministic without new persistence or
provider calls. Automatic refresh/archive, pinned blocks, and agent-facing
context management tools remain explicit future extensions.
