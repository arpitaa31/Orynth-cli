# ADR-0026: Bounded Plugin Manifest Discovery

Status: accepted

## Context

External plugin discovery must not become implicit activation. Untrusted
directories can contain malformed, oversized, duplicated, or symlinked entries,
and a discovered capability declaration cannot authorize itself.

## Decision

`orynth-plugin-discovery` scans explicitly supplied directories for the exact
`orynth-plugin.manifest` filename. It skips symlinks, applies bounded file and
manifest sizes, parses a strict line-oriented format with explicit resource
limits, validates plugin API contracts, rejects duplicate IDs, and optionally
filters kinds. It returns metadata plus the source path only.

Discovery never launches a command, grants a capability, or constructs a
transport. The process host provides a separate explicit activation function
that re-reads and compares the manifest, rejects relative/symlink/non-file
entrypoints, and rechecks the executable immediately before spawn. The caller
must still cross the host-policy boundary before invocation.

## Consequences

- Discovery is deterministic, dependency-light, and safe to run against an
  untrusted directory within configured bounds.
- Manifest format evolution requires an explicit parser versioning decision.
- Source-file changes after discovery are rejected at activation rather than
  silently being bound to stale metadata.
