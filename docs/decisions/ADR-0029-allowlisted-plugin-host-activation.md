# ADR-0029: Allowlisted Plugin Host Activation

Status: accepted

## Context

Manifest discovery intentionally does not construct transports, but leaving
every adapter to implement its own startup binding would make activation policy
inconsistent. Automatic startup must not turn an untrusted directory into an
implicit capability grant or a process launch.

## Decision

`orynth-plugin-host` owns startup activation. `ActivationPolicy` requires a
bounded plugin count and supports explicit plugin-ID and plugin-kind
allowlists. The default policy admits only process and WASM candidates.

Activation discovers candidates, filters them through the allowlist, reuses the
process/WASM adapter revalidation boundaries, stages all selected transports in
a temporary registry, and commits only if every selected candidate binds
successfully. Process binding constructs a command adapter but does not launch
the child. Invocation looks up the immutable manifest and delegates through
the adapter's existing capability and resource checks.

MCP, native, and builtin candidates are rejected until each has an explicit
session or host transport. Discovery and activation never grant capabilities;
leases remain owned by the caller's `CapabilityPolicy`.

## Consequences

Startup can automatically bind explicitly authorized process/WASM candidates
without partial activation or hidden authority changes. Unsupported transport
types fail closed. A future MCP session host can join the same registry without
moving protocol or capability policy into the kernel.
