# ADR-0035: Bounded WASM Resource Provider

Status: accepted

## Context

The Wasmi adapter previously exposed only a non-effectful capability check.
That allowed modules to make policy-aware decisions but did not provide a
tested path for host-owned effects. Adding WASI wholesale would introduce a
large filesystem, network, clock, and descriptor surface before Orynth has a
platform sandbox.

## Decision

`orynth-plugin-wasm` exposes an opt-in `orynth.resource_read` import with the
ABI `(domain, resource_ptr, resource_len, output_ptr, output_capacity) -> i32`.
The host reads the resource name from bounded module memory, maps the domain,
requires a matching manifest capability and current agent/task lease, and only
then calls an explicitly installed `WasmResourceProvider`. Provider output is
bounded to 64 KiB and copied back into module memory; negative return codes
represent malformed input, denied capability, absent provider, provider
failure, or output overflow. The provider is serialized behind an owned mutex
and is never installed by default.

The import is a narrow host adapter, not full WASI. Network/process/secrets
effects, directory descriptors, ambient environment access, and preemptive
wall-time interruption remain separate work.

## Consequences

WASM now has a real, testable effect boundary while the kernel remains unaware
of Wasmi memory or host I/O. Capability checks happen immediately before the
provider is called, and a module cannot create authority by importing the
function alone. Hosts must deliberately supply a provider and define its
resource interpretation.
