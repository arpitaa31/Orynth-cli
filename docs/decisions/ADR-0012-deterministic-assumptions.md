# ADR-0012: Deterministic normalized assumption conflicts

Status: accepted

## Context

Agents need to publish cross-agent claims such as `schema.users.id = UUID`.
Those claims must be inspectable and durable, and obvious contradictions should
not consume manager-model tokens. Exact or normalized equality is deterministic;
fuzzy semantic equivalence is not a safe kernel primitive.

## Decision

`orynth-assumptions` stores claims with owner, evidence, dependencies, revision,
confidence, and lifecycle state. It normalizes subject/value strings by
trimming, collapsing whitespace, and lowercasing. Different normalized values
for one normalized subject create an `AssumptionConflict`, mark both claims
`Conflicted`, and report their affected owners in sorted order. The graph emits
versioned transitions, and `RuntimeService` appends them atomically after
checking run and owner membership.

## Alternatives

- Ask a model to detect every contradiction: rejected for obvious deterministic
  cases because it is slower, nondeterministic, and less auditable.
- Treat all claims as opaque strings without normalization: rejected because
  trivial formatting differences would hide contradictions.
- Automatically pause affected agents: deferred because pause policy and
  authorization belong to supervision, not the assumption graph.

## Consequences

Conflict detection is reproducible and replayable, and affected owners are
available for later notification or policy. The current slice does not claim
fuzzy semantics, authorization of claims, automatic remediation, or manager
supervision.
