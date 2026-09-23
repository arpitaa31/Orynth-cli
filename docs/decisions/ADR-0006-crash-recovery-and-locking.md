# ADR-0006: Local crash-recovery and writer-locking policy

Status: accepted

## Context

The runtime has two local durable event backends with different storage
semantics. Both must preserve immutable event sequences, reject complete
corruption, and make writer contention explicit without pretending to provide
unverified crash guarantees.

## Decision

`FileEventStore` acquires an exclusive operating-system advisory lock on a
sibling `.lock` file for the lifetime of the opened store. A second open of
the same event file fails with a busy storage error. The lock file itself may
remain after a crash; the OS lock is released when the owning process exits,
so a stale lock-file directory entry does not block recovery. This backend is
therefore single-open and must not be used for concurrent readers/writers.

Filesystem event batches are encoded completely before one append, flushed, and
synced. On open, only an incomplete final event or metadata frame is repaired;
complete checksum or semantic corruption fails closed. Metadata snapshots are
written through a synced temporary file and rename. Directory-entry durability
and arbitrary mid-operation fault injection are not yet claimed.

`SqliteEventStore` uses SQLite's rollback journal with `synchronous = FULL`, a
five-second busy timeout, and `IMMEDIATE` transactions for event batches,
branches, and snapshots. SQLite serializes competing writers while allowing
normal readers, and transaction rollback protects a batch from partial commit.

## Alternatives

- Treat the filesystem adapter as concurrently writable: rejected because
  independent in-memory sequence views could race and interleave frames.
- Use a PID-only lock file: rejected because a stale PID is not a portable or
  safe liveness proof after a crash.
- Claim arbitrary crash recovery from unit tests: rejected; fault injection and
  directory-durability testing remain explicit follow-up work.

## Consequences

The SQLite backend is the preferred concurrent local backend. The filesystem
adapter remains useful for dependency-free deterministic recovery tests and
has a deliberately narrow single-open contract. The next hardening slice is a
fault-injection matrix covering event writes, metadata replacement, SQLite
journal recovery, and artifact writes.
