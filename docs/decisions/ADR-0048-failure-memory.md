# ADR-0048: Compact event-sourced failure memory

Status: accepted.

## Context

Agents need to know when an approach has already been attempted and why it
failed. Requiring them to rediscover that fact from complete transcripts would
increase context usage and copy mutable history between agents. The information
must also survive replay, fork materialization, and local-store recovery.

## Decision

Add a bounded `orynth-failure-memory` domain with `FailureRecord` values and
versioned `Recorded`/`Resolved` transitions carried in the kernel event
envelope. A record stores an agent, optional task, exact fingerprint, approach,
reason, and bounded evidence references. Runtime membership validation happens
before append. Recovery rebuilds the projection from events, and exact
fingerprint queries expose prior attempts without claiming semantic similarity.

Failure records are copied as opaque transition payloads during fork
materialization; their agent/task identities remain stable in the child prefix.
The inspector reports recorded failures as semantic breakpoint coordinates.

## Alternatives

- Search complete model/tool transcripts on demand: rejected because it copies
  too much context and is not a compact deterministic projection.
- Keep an in-memory manager cache: rejected because it is not durable or
  replayable.
- Automatically suppress matching work: deferred because exact fingerprint
  matches do not prove semantic equivalence or that a retry is invalid.

## Consequences

Failure history is durable, bounded, queryable, and observable without adding a
provider call. The current primitive does not classify failures, archive old
records, share memory across runs, or trigger automatic retries/model changes;
those remain explicit orchestration work.
