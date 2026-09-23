# Failure Memory

Status: bounded first Phase 9 slice implemented.

Failure memory stores compact structured records of attempted approaches. Each
record contains an owning agent, optional task, stable caller-provided
fingerprint, approach summary, failure reason, and bounded evidence references.
Records are event-sourced through `FailureMemoryTransition` and recovered with
the runtime projection from the same authoritative event stream.

The runtime exposes `record_failure` and `resolve_failure`. Queries can test
whether a fingerprint has already been attempted or retrieve all matching
records. Resolution changes only the record state; it does not erase the
historical attempt.

The boundary is deliberately conservative:

- records are bounded and reject empty or oversized text and evidence lists;
- runtime membership checks validate the owning agent and optional task before
  append;
- exact fingerprints are query keys; semantic similarity and automatic retry
  suppression are not inferred;
- the memory stores references and compact evidence, not full transcripts;
- replay, SQLite recovery, and read-only TUI breakpoint scanning are covered;
- policy-driven failure classification, cross-run memory, archival, and
  automatic scheduler reactions remain future work.
