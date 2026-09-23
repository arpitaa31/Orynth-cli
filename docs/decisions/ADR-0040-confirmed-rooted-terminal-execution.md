# ADR-0040: Confirmed rooted terminal execution

Status: accepted; persistent undo extended by ADR-0042

## Context

The plan-only shell established a safe inspection boundary, but the Phase 7
terminal milestone also requires genuine execution, verification, and
compensation semantics. A CLI effect path must not bypass the existing tool
runtime or turn unsupported operations into an ambient shell.

## Decision

`orynth-cli` exposes an explicit `execute` command for the rooted filesystem
subset currently implemented by `FilesystemFixture`: find, move, copy, and
quarantine-backed remove. The command reconstructs a bounded plan, requires
confirmation for mutating risk levels, creates an ephemeral user-scoped
filesystem capability lease, and drives the existing tool runtime through
preview, execute, verify, and commit. The fixture remains rooted at the
detected working directory and rejects traversal.

Process listing, Git, and raw-shell operations remain unsupported by this
execution adapter and fail closed. The fixture’s compensation token is
verified in-process; persistent cross-process `undo` storage is deferred until
its journal and crash-recovery semantics are designed and tested.

## Consequences

The shell now performs a real, bounded filesystem effect without creating a
second authorization or transaction system. Confirmation and capability checks
remain visible in the same runtime boundary. ADR-0042 adds bounded rooted-file
undo; the CLI is not yet the complete natural-language terminal assistant and
does not claim process execution, Git mutation, or OS sandboxing.
