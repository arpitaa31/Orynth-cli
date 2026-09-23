# ADR-0023: WASM Resource Admission at the Host Boundary

Status: accepted

## Context

WASM/WASI plugins must not turn an untrusted module into an implicit bypass of
capability policy, resource limits, or output provenance. The first safe slice
therefore defined the host boundary independently of engine choice; the
concrete Wasmi integration is recorded in ADR-0028.

## Decision

`orynth-plugin-wasm` uses an injected executor. Before execution it validates
the exact bound manifest, requires the host capability policy to authorize all
declared capabilities, and validates bounded requests. After execution it
rejects executor-reported memory, fuel, or wall-time usage above the manifest
limits, maps wall-time violations to a timeout, validates the bounded response,
and marks the output `External`.

The injected adapter remains an admission and accounting contract. The
Wasmi-backed adapter additionally executes the bounded ABI, its non-effectful
capability-check import, and the narrow opt-in resource provider recorded in
ADR-0035. Full WASI, broader effectful host imports, and preemptive wall-time
interruption remain explicit follow-up work.

## Consequences

Engine selection can happen later without weakening the kernel/security
boundary. Deterministic tests can exercise capability denial and resource
exhaustion without executing arbitrary modules. Reported usage is only
authoritative when the eventual executor enforces or measures it faithfully;
the production engine integration must preserve this contract.
