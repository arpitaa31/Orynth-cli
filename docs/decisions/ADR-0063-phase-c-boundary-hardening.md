# ADR-0063: Phase C boundary hardening

Status: Accepted

Date: 2026-09-24

## Context

The Phase C audit found unbounded process-plugin I/O, post-start Windows
containment, whole-file manifest/WASM reads, per-batch rather than cumulative
plugin admission, unchecked MCP wire negotiation, and descriptive scheduler
ownership that was not enforced at effect boundaries.

## Decision

1. Process sessions use bounded frame-and-byte response queues and a dedicated
   synchronous stdin writer. Timeout and termination paths close queues, kill
   the child, close platform containment, and join I/O workers. Request send
   and response receive use one deadline, also capped by the manifest
   wall-time limit.
2. Windows contained children are created suspended. Job Object configuration
   and assignment complete before the primary thread is resumed. Unsupported
   platforms fail closed when containment is required.
3. File policy is enforced while reading: bounded readers consume at most the
   configured limit plus one byte and reject the extra byte. Metadata checks
   remain an optimization only. Discovery retains only bounded manifest paths,
   rather than materializing an entire directory listing.
4. Plugin admission is checked against existing plus incoming active adapters
   before staged activation. Deactivation explicitly releases a slot.
5. MCP sessions require a supported server-returned wire version and exact
   agreement with the selected legacy mode. Modern mode remains explicitly
   per-request.
6. Ownership is a separate read/write policy from capability leases. Scheduler
   claims reject overlapping cross-agent writes; tool and plugin effect APIs
   receive the authoritative policy; cancellation durably releases claims.
   Canonical resource identities are shared by scheduler claims and effect
   checks. Legacy tool helpers deny write effects unless an explicit ownership
   policy is supplied. Manifest effects are conservatively treated as writes
   until manifests carry an explicit access mode. HTTP MCP event streams are
   usable only after the same connection-bound ownership admission check.

## Consequences

Phase C callers must provide an ownership policy when invoking plugin adapters
and MCP process sessions. Existing isolated tests use the explicit
`AllowAllOwnership` adapter; the CLI uses an exclusive workspace owner, and a
runtime caller can pass its recovered `SchedulerState`. Bounded queues apply
backpressure instead of allowing unbounded host memory growth. The Windows
adapter improves launch atomicity but is not a complete AppContainer or
network/filesystem sandbox. Filesystem ownership and final-object validation
retain the Phase A TOCTOU limitation; cloned capability and ownership inputs
on an already-connected HTTP transport are connection-bound snapshots rather
than live revocation handles.

## Validation

Focused process, MCP HTTP, host, WASM, scheduler, tool-runtime, terminal,
runtime, and process integration suites pass. The discovery and MCP stdio
test binaries, and the exact all-features workspace test command, are
environment-blocked when Windows Application Control launches generated test
binaries (OS error 4551). Workspace check, Clippy, formatting, and the
release workspace build pass. This limitation is recorded in the remediation
document.
