# ADR-0021: Plugin and External Protocol Boundaries

Status: accepted

## Context

Process plugins, MCP servers, and A2A agents are external execution or
communication domains. Their metadata and outputs cannot be treated as trusted,
and network or process concerns must not leak into the Orynth kernel.

## Decision

`orynth-plugin-api` owns a bounded registry plus manifests, protocol versions,
explicit capability declarations, resource limits, requests, and responses. Process,
MCP, and A2A crates adapt those contracts at the edge. Process responses are
external, MCP metadata/results use their specific untrusted origins, and A2A
messages translate to typed IPC with `RemoteAgent` provenance. MCP tool
capabilities, risk, and reversibility come from host policy, never server
annotations.

The process adapter checks host capability policy before invoking its injected
or command-backed transport. The command host provides bounded process launch,
Windows Job Object containment, and lifecycle control, but does not claim a
complete filesystem/network sandbox, network transport, discovery, or
WASM/WASI execution. Those capabilities require later platform-specific
slices.

## Consequences

- The kernel remains independent of MCP, A2A, and process APIs.
- External values enter the runtime with explicit bounded provenance.
- Injected adapter tests remain deterministic and offline; the command-host
  integration fixture separately verifies real launch without making network
  calls.
