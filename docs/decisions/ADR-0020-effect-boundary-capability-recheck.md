# ADR-0020: Effect-Boundary Capability Rechecks

Status: accepted

## Context

Tool preflight validates a proposal before approval and execution. Capability
leases may expire or be revoked between those phases, so a preflight decision
cannot be treated as a durable authorization to perform an external effect.

## Decision

`ToolRuntime` reauthorizes all registered capability requirements immediately
before execution and immediately before compensation using the current wall
clock. Failure leaves the transaction in its pre-effect state and no executor
call is made.

## Consequences

- Lease expiry and revocation are enforced at the last runtime-controlled point
  before an injected adapter can mutate external state.
- The executor boundary remains adapter-owned; this is not an OS sandbox or
  process isolation mechanism.
- Tests use long-lived leases for ordinary pipeline cases and explicitly cover
  post-preflight expiry.
