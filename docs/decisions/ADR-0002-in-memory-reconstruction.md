# ADR-0002: Prove reconstruction before durable storage

Status: accepted

## Context

The runtime requires immutable events, reconstructable materialized state, recorded replay, snapshots, artifacts, and eventually durable local storage. Adding SQLite before the event semantics are stable would make persistence schema decisions conceal unresolved runtime behavior.

## Decision

Phase 2 begins with an in-memory `EventStore` contract and backend. It assigns store-local monotonic sequences, rejects duplicate event IDs, reconstructs run/task/agent projections, validates run event ordering and terminal transitions, and exposes recorded replay over captured events. The provider is not involved in reconstruction.

Durable storage and crash recovery remain separate follow-up slices. The repository has since added SQLite artifact, branch-metadata, versioned snapshot, and live fork-continuation adapters behind the backend-neutral contracts established here.

## Alternatives

- Write SQLite tables first: rejected because schema would become the accidental authority.
- Reconstruct directly from agent traces: rejected because the runtime needs a store contract independent of one execution implementation.
- Treat replay as re-execution: rejected because recorded replay must never invoke model providers.

## Consequences

The current runtime can prove event semantics deterministically without network or database dependencies. It cannot yet survive process loss or store large payloads; those limitations remain explicit in the status and persistence documentation.
