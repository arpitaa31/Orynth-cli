# ADR-0009: Runtime service owns cross-domain recovery

Status: accepted

## Context

The event store can validate and reconstruct the generic runtime projection,
while `orynth-context` owns typed context transition encoding and graph replay.
Putting context interpretation into the event store would make persistence
depend on a Phase 3 domain model and would blur the boundary between storage
and runtime orchestration.

## Decision

Add `orynth-runtime` as the composition layer. `RuntimeService<S>` owns an
`EventStore` implementation, reads one authoritative run event sequence,
reconstructs `RuntimeState` through the store, and passes the same events to
`ContextEventLog` before replaying a `ContextGraph`. Opaque context events are
preserved by the store; typed interpretation happens only in this service and
the context crate.

Recovery returns both projections and the stored event sequence so callers can
inspect the evidence used to hydrate state. Unknown store runs and malformed
context payloads fail recovery rather than producing a partial graph.

## Alternatives

- Make `orynth-event-store` depend on `orynth-context`: rejected because it
  inverts the domain/storage dependency direction.
- Reconstruct context only in the context crate: rejected because no single
  runtime boundary would coordinate ordinary and context projections.
- Rebuild context from rendered prompts: rejected because prompts are derived
  views, not authoritative state.

## Consequences

In-memory, filesystem, and SQLite backends share one recovery contract, and
context hydration is tested through durable reopen paths. Large context content
can now travel through the artifact store as an opaque digest and is verified
against the context hash during recovery. Provider cache telemetry remains a
subsequent slice.
