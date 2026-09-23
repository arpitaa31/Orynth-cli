# ADR-0005: SQLite schema and migration boundary

Status: accepted

## Context

The event and artifact contracts now have in-memory and filesystem implementations. The local queryable backend must support events, artifacts, snapshots, branches, metadata, and recovery without changing event semantics. A bundled `rusqlite` dependency is now available, so the first SQLite schema and adapter can be implemented and tested in the repository.

## Proposed schema

Version 1 should contain:

- `schema_meta`: one row per migration version and checksum;
- `events`: store-local sequence primary key, unique event ID, run ID, timestamp, event kind, encoded payload, and checksum;
- `snapshots`: run ID, event sequence, encoded materialized state, and checksum;
- `artifacts`: content hash primary key, byte size, media type, trust tag, storage locator, and checksum;
- `branches`: branch ID, parent run, fork sequence, replay mode, and creation metadata.

Event payloads remain versioned opaque bytes to the storage layer. Runtime reconstruction owns interpretation. Artifact payloads may remain external blob files addressed by content hash; SQLite stores metadata and locator rather than copying large bytes by default.

## Migration contract

A fresh database applies migrations transactionally. Reopening an already-current database is idempotent. Unknown future versions fail closed. Each migration records its version and checksum. A failed migration leaves the previous committed schema usable.

## Backend parity acceptance

The SQLite backend must pass the same tests as the in-memory/filesystem implementations: event ordering and duplicate IDs, successful/failed reconstruction, recorded replay without provider invocation, snapshots and incremental reads, artifact references, reopen, transaction atomicity, migration idempotence, corruption reporting, and branch metadata. SQLite-specific query tests may extend this set but must not redefine runtime authority.

## Consequences

The schema is explicit and preserves backend-neutral contracts. Version 2 is implemented for event/artifact persistence, queryable reconstruction, validated branch metadata, versioned snapshot persistence, durable artifact trust tags, prefix-based child-run materialization, provider-backed continuation, atomic continuation batches, serialized writers, and full synchronous transactions. Fault-injection and directory-entry durability remain follow-up work.
