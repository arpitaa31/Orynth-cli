# ADR-0014: Capability-gated tool transactions

Status: accepted.

## Context

Models and external metadata are untrusted proposal sources. Tool execution
must be explicit, inspectable, and separable from the platform adapter that
performs an effect.

## Decision

`orynth-security` owns capability leases scoped by agent, optional task,
capability domain, resource subtree, and expiry. `orynth-tool-runtime` owns
typed definitions and proposals and runs a transaction through normalization,
required-field validation, capability authorization, risk classification,
approval, injected execution, verification, commit, and compensation where an
inverse is declared and actually available.

The core crates do not perform external effects. Controlled filesystem and
typed injected-process fixtures live in `orynth-terminal-tools` behind these
contracts and cannot bypass capability checks. Platform sandboxing remains a
separate enforcement boundary.

## Consequences

- Missing, expired, or out-of-scope capabilities fail closed.
- High-risk operations cannot execute without an explicit approval transition.
- Verification precedes commit, and irreversible operations cannot claim
  compensation.
- Trust/provenance policy flow and platform sandboxing remain follow-up work.
