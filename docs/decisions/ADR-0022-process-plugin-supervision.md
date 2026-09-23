# ADR-0022: Process-Plugin Supervision Contract

Status: accepted

## Context

An injected process adapter can report a crash or timeout, but callers still
need deterministic lifecycle behavior. Continuing to send requests after such a
failure risks using a dead or inconsistent child. Restarting also must not
silently reuse the failed adapter state.

## Decision

`orynth-plugin-process` exposes `ProcessSupervisor` with explicit `Ready`,
`Running`, `Crashed`, `TimedOut`, and `Stopped` states. It refuses calls unless
running, transitions to a terminal failure state on crash/timeout, and accepts a
restart only with a newly supplied adapter whose manifest matches. Capability
policy remains mandatory on every invocation.

This is a deterministic lifecycle contract over injected or command-backed
transports. The command-backed host adapter now supplies bounded one-request
OS process launch and a versioned binary frame, but it does not claim OS
sandboxing; platform-specific isolation remains a separate boundary.

## Consequences

- Failure is observable and follow-up work fails closed.
- Restart boundaries are explicit and auditable at the adapter level.
- The contract can be tested offline without launching arbitrary processes.
