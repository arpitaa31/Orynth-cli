# ADR-0004: Dependency-free durable adapters precede SQLite

Status: accepted

## Context

The runtime contracts now have deterministic in-memory semantics for events, replay, snapshots, and artifacts. Durable storage must preserve those semantics across reopen, torn writes, and corruption. SQLite remains the preferred queryable local backend, and the filesystem adapter established the parity and recovery tests needed before adding it.

## Decision

Add dependency-free filesystem adapters behind the existing `EventStore` and `ArtifactStore` contracts. Event storage uses length-prefixed frames with deterministic checksums, fsyncs each append, repairs only an incomplete final frame, and rejects complete checksum corruption. Artifact storage writes a validated single-file record through a temporary file and rename.

These adapters are an intermediate backend for contract and recovery tests, not the final SQLite schema or security boundary. The initial SQLite adapter now uses the same reconstruction, replay, snapshot, branch-metadata, and artifact acceptance contracts; child-run fork execution and the complete recovery policy remain separate work.

## Alternatives

- Add SQLite immediately: accepted after schema and migration tests proved the initial backend boundary.
- Append raw text without checksums: rejected because corruption would be indistinguishable from valid data.
- Silently discard any malformed frame: rejected because complete corruption must be surfaced; only a torn final frame is repairable.

## Consequences

The runtime now survives normal reopen and a defined class of interrupted final writes without network dependencies. The filesystem adapter has a single-open OS advisory lock, and SQLite provides queryable persistence, migrations, and serialized durable transactions. The system still lacks arbitrary fault-injection coverage, directory-entry durability guarantees, and cryptographic integrity.
