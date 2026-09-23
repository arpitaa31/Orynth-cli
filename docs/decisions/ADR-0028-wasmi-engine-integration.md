# ADR-0028: Wasmi Engine for the Initial WASM ABI

Status: accepted

## Context

Orynth needs a concrete WASM execution path without turning an untrusted module
into an implicit filesystem, network, process, or secret capability. The host
admission contract from ADR-0023 must remain authoritative, and the first
engine integration must be deterministic and bounded.

## Decision

`orynth-plugin-wasm` embeds Wasmi 2.0 with eager module validation and strict
engine limits. The ABI supplies an explicit non-effectful capability-check
import and the opt-in bounded resource provider described by ADR-0035. A
module must export:

- `memory`, a linear memory export;
- `orynth_run(i32, i32) -> i64`, where the arguments identify the request and
  the result packs the output pointer in the high 32 bits and output length in
  the low 32 bits.

The optional `orynth.capability_check(i32, i32, i32) -> i32` import accepts a
capability-domain tag and a UTF-8 resource slice in the module's memory. It
returns `1` only when the resource is within a manifest declaration and the
current agent/task lease authorizes it; malformed, undeclared, or unauthorized
requests return `0` or a negative validation result. The import performs no
filesystem, network, process, secret, or other host effect.

Each invocation uses a fresh store with one instance, one memory, one table,
the manifest memory limit, and manifest fuel. Input and output lengths are
bounded by the manifest. Fuel exhaustion maps to `ResourceExhausted`, and
successful output remains `External` under the existing host boundary.

The injected `WasmInvoker` contract remains available for independently
sandboxed hosts. `activate_discovered_wasm` re-reads the candidate manifest,
requires an absolute regular module file, and binds the Wasmi transport without
granting capabilities or invoking it. No full WASI implementation, broader
effectful host imports, preemptive wall-time interruption, or unrestricted/
implicit activation is implied by this decision; ADR-0035 records the narrower
resource-read extension.

## Consequences

The repository has a real, testable WASM execution path with explicit
capability absence, import gating, and resource accounting. Modules cannot
access host services through this adapter unless the host explicitly supplies
the bounded resource provider. Fuel provides a deterministic execution bound;
elapsed wall time is still measured and checked
after execution by `WasmPlugin`, so hard preemptive interruption requires a
future engine/host design.
