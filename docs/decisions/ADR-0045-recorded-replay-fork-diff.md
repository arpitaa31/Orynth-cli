# ADR-0045: Recorded replay, fork materialization, and projection diff

Status: accepted

## Context

The runtime already persists event sequences, branch metadata, and validated
child prefixes. A debugger needs operator commands that use those primitives
without confusing recorded inspection with live model execution.

## Decision

The operator app exposes three explicit boundaries:

- `replay` stages a selected event prefix, recovers it through the runtime, and
  reports zero provider calls;
- `fork` creates durable branch metadata and materializes a validated child
  prefix with an explicit replay mode and child identity;
- `diff` recovers two runs and compares their persisted projection fields.

Forking does not execute a provider or claim that a live continuation occurred.
Recorded replay preserves source event sequence coordinates in the recovered
view. All commands fail for missing or invalid event boundaries.

## Consequences

The debugger can inspect earlier state, create a durable starting point for a
future alternate execution, and compare real recovered runs. Provider-backed
re-execution, interactive controls, and richer event-level diff views remain
separate follow-up work.
