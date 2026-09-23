# ADR-0033: Non-Effectful WASM Capability Check Import

Status: accepted

## Context

WASM modules need a way to make capability-aware decisions without receiving
ambient filesystem, network, process, secret, or plugin authority. Passing
policy data into module memory would be ambiguous and could become stale during
execution.

## Decision

The Wasmi linker exposes `orynth.capability_check(domain, ptr, len) -> i32`.
The host reads a bounded UTF-8 resource from the module's exported memory and
returns success only when both conditions hold:

1. the resource is within a matching capability declared by the bound manifest;
2. the current agent/task capability policy authorizes the resource at the
   invocation timestamp.

The function is a gate, not an effect adapter. It performs no I/O and supplies
no handles or host data. Invalid pointers, lengths, domains, or UTF-8 return a
negative validation result; undeclared or unauthorized resources return zero.

## Consequences

WASM code can make deterministic policy-aware choices, and capability checks
remain at the host boundary. ADR-0035 adds one narrow opt-in resource-read
adapter with its own bounded provider and tests; full WASI and broader
effectful host imports still require separate adapters with their own bounded
handles and resource accounting.
