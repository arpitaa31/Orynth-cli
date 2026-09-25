# ADR-0065: Phase E benchmark and hardening evidence

Status: accepted

## Context

Phase D intentionally deferred measured performance, fault, fuzz, and stress
evidence. Phase E needs reproducible local measurements without API keys or a
new runtime dependency stack, while keeping unmeasured cross-platform and TUI
claims explicit.

## Decision

Add the dependency-light `orynth-bench` workspace binary and keep its raw TSV
and text captures under `benchmarks/results/`. The harness uses deterministic
mock workloads and reports timing, sampled working set, operation counts, and
limitations. Focused cargo-fuzz targets live in a separate `fuzz` workspace so
they do not become a runtime dependency or a normal workspace test gate.

The assumption graph maintains a private normalized-subject index in addition
to its authoritative assumption map. Publication uses the index to find
possible conflicts without scanning every assumption. Conflict records remain
explicit and are still proportional to the conflicts produced by the input.

## Consequences

Phase E can report measured local evidence and a reproducible baseline without
claiming production CLI/TUI RSS or hosted platform results. The subject index
reduces the measured conflict-heavy 10k publication median by 26.4%; the
workload remains expensive when it intentionally creates many conflict
records. Full cargo-fuzz, Linux/macOS runs, and TUI measurement require later
environment support.
