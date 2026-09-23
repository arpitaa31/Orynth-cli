# ADR-0003: Artifact references separate metadata from payload storage

Status: accepted

## Context

Runtime events need to identify artifacts without copying large immutable payloads into reconstructed state. The long-term design calls for content-addressed blobs, while the current workspace intentionally has no storage or cryptographic dependency.

## Decision

Define an `ArtifactRef` containing a content hash, byte size, media type, and
shared trust origin. Store payload bytes behind that reference in an
`ArtifactStore`; reconstructable events carry only artifact metadata. The
initial in-memory backend deduplicates identical bytes and rejects same-hash
byte collisions or metadata mismatches; same-content writes combine origins
by retaining the least-trusted class.

The default dependency-free hasher is deterministic but explicitly non-cryptographic. It exists for identity and deduplication tests, not integrity, trust, or security decisions. A later durable backend may use a cryptographic digest without changing runtime references.

## Alternatives

- Copy payload bytes into every event and snapshot: rejected because it violates bounded memory and causes transcript-like duplication.
- Use generated artifact IDs only: rejected because identical immutable content would not deduplicate and references would not be content-addressed.
- Treat the initial hash as a security primitive: rejected because its collision resistance is not established.

## Consequences

Runtime state can reconstruct artifact metadata while payload storage remains separate. The filesystem and SQLite adapters now provide durable validated payload storage and trust metadata with bounded write-recovery behavior; the default deterministic hash still must not be presented as cryptographic integrity or a security boundary. Retention and garbage collection remain separate lifecycle concerns.
