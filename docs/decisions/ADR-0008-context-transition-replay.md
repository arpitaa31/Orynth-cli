# ADR-0008: Versioned opaque context transition events

Status: accepted

## Context

Context blocks are authoritative runtime state, but the kernel event format is
provider-independent. Adding storage-specific or context-crate types to the
kernel would invert the dependency direction, while dropping context events
from the durable log would lose ordering and recovery information.

## Decision

`orynth-context` emits typed `ContextTransition` values for block creation,
namespace supersession, and invalidation. It encodes them as a versioned,
bounded payload carried by the opaque kernel `EventKind::ContextTransition`
variant. The filesystem and SQLite event stores persist and recover the bytes
without interpreting them; `ContextEventLog` decodes the events and replays
them into a fresh `ContextGraph`, validating content hashes, references, and
lifecycle changes. Prompt text remains a derived view and is never an
authoritative event.

The first transition carries immutable content bytes so replay is self-contained
for deterministic tests. Large content is still bounded by the event codec;
future integration will move it behind artifact references without changing
graph semantics.

## Alternatives

- Put context graph types directly in `orynth-kernel`: rejected because it
  would make kernel primitives own a Phase 3 domain model prematurely.
- Persist rendered prompts: rejected because prompts are projections and would
  make provider-facing formatting authoritative state.
- Test only the live graph: rejected because supersession and invalidation must
  be reconstructable independently of the mutating process.

## Consequences

Context freshness and replay semantics are testable independently of storage,
and durable event sequencing is now covered by both backends. Runtime-service
graph recovery and external artifact payload references remain the next
integration boundary.
