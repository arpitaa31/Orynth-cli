# Phase 03: Context

Status: active.

The first vertical slice adds typed versioned context blocks, deterministic content addressing, global/team/private scopes, references, dependencies, bounded privacy-filtered projections, namespace subscriptions, invalidation, token estimates, prompt rendering, stable-prefix hashing, and a replayable `ContextTransition` log. A versioned opaque kernel-event envelope carries those transitions through the filesystem and SQLite event stores, with bounded decoding and reopen coverage. `orynth-runtime` now hydrates `RuntimeState` and `ContextGraph` together from the same event sequence, including durable reopen recovery, externalizes large content through artifact digests with hash verification and trust propagation, and aggregates only explicit provider cache metadata through durable `CacheObserved` events. Sixteen context tests, four cache tests, 47 event-store tests, and 31 runtime-service tests cover graph semantics, transition encoding, artifact externalization and provenance, cache evidence rules, SQLite reopen, store recovery, malformed events, failure-memory and specialist-profile recovery, exact-prefix cache-aware routing, bounded freshness proprioception, explicit archive/restore and pin/unpin lifecycle control, and bounded search/dependency inspection.

Next: add provider-specific cache metadata adapters and automatic expiry selection, then
extend runtime-service composition toward the Phase 4 manager/agent boundary.
