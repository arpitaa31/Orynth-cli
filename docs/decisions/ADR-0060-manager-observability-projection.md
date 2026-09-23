# ADR-0060: Bounded Manager Observability Projection

Status: accepted.

## Context

Active supervision needs a compact view of the state that affects an agent's
next decision. Identity, model, health, budgets, assumptions, conflicts, and
owned resources are not enough to explain context pressure, cache evidence, or
known failed approaches.

## Decision

Extend the recovered manager projection with bounded context proprioception,
cache observation and artifact counts, and per-agent failure identifiers split
into all and active failures. These values are derived from the authoritative
event stream during recovery; the projection does not inspect providers, infer
cache hits, copy transcripts, or mutate runtime state.

## Consequences

- manager and inspector consumers can identify compact pressure and blocker
  signals before choosing a supervision action;
- failure details remain in failure memory and context details remain behind
  their existing read-only APIs;
- richer recent-artifact detail, automatic alert thresholds, and policy-driven
  supervision remain separate future layers.
