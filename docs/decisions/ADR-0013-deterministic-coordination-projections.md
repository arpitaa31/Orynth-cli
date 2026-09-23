# ADR-0013: Deterministic coordination projections

Status: accepted.

## Context

Multi-agent execution needs budgets and health signals that survive restart and
replay. A policy layer must not infer these values from mutable process state or
an LLM response.

## Decision

`orynth-scheduler` owns versioned `SchedulerTransition` payloads. Budget
configuration and usage are per-agent projections with six bounded dimensions:
tokens, money micros, wall-clock milliseconds, tool calls, child agents, and
context tokens. Runtime reconstructs the projection before each write and
rejects an overrun before appending the event.

Health is a deterministic projection over explicit signals. Repeated failures,
tool errors, assumption conflicts, and verification failures have fixed
thresholds; pressure and invalidation signals are preserved as counters. The
projection derives `Healthy`, `Degraded`, or `Blocked` without an LLM or
wall-clock sampling.

The same transition stream owns resource claims and releases. A resource maps
to at most one agent; a different claimant or a non-owner release is rejected.

Runtime recovery derives a compact manager projection from the ordinary agent,
assumption, and coordination projections. It contains stable summaries and
references, not copied transcripts.

## Consequences

- Restart and fork replay use the same durable inputs.
- Backends only preserve opaque versioned payloads; scheduler policy stays out
  of the kernel and persistence adapters.
- Runtime membership checks prevent unknown agents from creating coordination
  state for a run.
- Automatic notification, ownership, routing, and supervision remain separate
  follow-up slices.
