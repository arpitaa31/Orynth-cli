# ADR-0062: Phase B audit remediation boundaries

Status: accepted

Date: 2026-09-24

## Context

The Phase B audit identified correctness gaps at persistence, context
freshness, scheduling, runtime mutation, tool-effect, CLI translation, and
process-policy boundaries. These boundaries share one requirement: derived
state must not silently become stronger than the evidence or authority that
produced it.

## Decisions

1. Snapshots persist logical model identity separately from the effective model
   selected for the current execution. Snapshot format v4 carries both fields;
   older snapshots remain readable with a conservative compatibility fallback.
2. Context invalidation propagates through archived and already-stale
   intermediates. Archived content that becomes dependency-stale has a
   distinct `ArchivedStale` lifecycle, and restore can produce only `Active`
   when dependency/source revisions are proven current. Prompt rendering
   accepts only active references after visibility and trust checks.
3. Scheduler cache ranking consumes only fresh, enabled, exact observations.
   Budget transfer cannot mint capacity from an unlimited source. Health keeps
   historical counters while explicit resolution signals control active
   pressure and status.
4. Runtime service mutation methods validate candidate state through a
   centralized append boundary before persistence. The raw event-store adapter
   accessor remains a low-level fixture/tooling escape hatch.
5. Tool execution carries optional output and explicit effect certainty:
   `NotAttempted`, `MayHaveOccurred`, or `Confirmed`. Compensation metadata is
   retained across verification failure but is never invented when an executor
   returns no safe compensator.
6. Syntax normalization is schema-declared through tool-definition syntax
   fields. Opaque values are validated for bounds/NULs without generic
   whitespace rewriting.
7. Process fixtures use a deny-by-default executable allowlist at the injected
   invocation boundary. Executable identity is case-folded only where the host
   filesystem semantics require it; argv-level command profiles and production
   OS sandboxing remain separate future work.

## Consequences

These decisions preserve replay and recovery semantics while making uncertain
or stale evidence explicit. They introduce versioned snapshot and lifecycle
compatibility handling, add fields to public tool definitions/executions, and
require adapters to declare syntax fields. The changes intentionally do not
claim provider-side cache truth, automatic context refresh policy, or a full OS
sandbox for process execution.

## Alternatives considered

- Reusing one model field in snapshots was rejected because promotion would
  destroy logical identity.
- Treating archived or stale context as active after restore was rejected
  because visibility is not freshness.
- Clearing historical health counters was rejected because it destroys useful
  evidence; active counters provide recovery without erasure.
- A larger blacklist for shells was rejected because aliases and executable
  identities are open-ended; explicit allowlisting fails closed.
