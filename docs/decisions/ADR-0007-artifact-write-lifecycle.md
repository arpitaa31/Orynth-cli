# ADR-0007: Immutable artifact write lifecycle

Status: accepted

## Context

Artifacts are immutable content-addressed payloads referenced by runtime
events. The filesystem adapter must tolerate interrupted writes and concurrent
writers without exposing partial payloads or replacing a committed artifact
with a conflicting value.

## Decision

`FileArtifactStore` serializes writers per content hash with an operating-system
advisory lock. It writes to a deterministic `<hash>.blob.tmp` file, flushes and
syncs that file, and then renames it to `<hash>.blob`. A later write overwrites
any orphaned temporary file under the same lock; a committed payload is always
validated by its encoded metadata, size, checksum, and content hash on read.

SQLite artifacts use the existing `IMMEDIATE` transaction and checksum/content
validation path. The default deterministic hasher remains an identity and
deduplication mechanism, not a cryptographic trust boundary.

Retention, garbage collection, external blob migration, and cryptographic
hashing remain separate lifecycle concerns.

## Alternatives

- Use a process-ID-only temporary filename: rejected because same-process
  concurrent stores could collide.
- Rename un-synced temporary bytes: rejected because a crash could publish
  data that was never durable on disk.
- Delete orphaned temporary files eagerly on startup: rejected because a
  live writer from another implementation could still own the path; the
  per-hash lock plus deterministic overwrite is safer and bounded.

## Consequences

Same-hash artifact writes are deterministic and safe across cooperating local
store instances. Temporary files may remain after a process crash, but they
cannot be returned by `get` and are repaired by the next write for that hash.
