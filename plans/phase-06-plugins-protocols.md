# Phase 06: Plugins and Protocols

Status: active, contract/adaptation slice implemented.

The first slice adds a dependency-light plugin API with a bounded registry and typed manifests,
protocol-version negotiation, explicit capability declarations, bounded memory,
fuel, wall-time, and message limits, plus untrusted response provenance. The
process adapter validates a bound manifest and supports both injected
transports and a command-based one-request child process over a bounded,
versioned binary frame. Host capability policy can deny an invocation before
the invoker is called, and the command adapter rechecks the executable path at
the effect boundary. The deterministic supervisor tracks ready/running,
crashed, timed-out, and stopped states, fails closed after failure, and only
restarts with a newly supplied adapter. The Windows command host provides Job
Object process containment, but does not claim a complete OS sandbox.

The MCP adapter treats server metadata and results as untrusted, translates
tool descriptions only when host policy supplies capabilities/risk/reversibility,
and marks results as `McpResult`. The A2A adapter validates bounded external
messages and translates supported messages into typed internal IPC with
`RemoteAgent` provenance. Network transport beyond the bounded HTTP adapter,
stronger OS filesystem/network sandboxing, unrestricted plugin activation,
implicit MCP network authority, and broader HTTP bidirectional behavior remain
later slices. The
host-owned activation crate now provides atomic,
allowlisted process/WASM startup binding without granting capabilities or
launching processes. Bounded
external manifest discovery, explicit process/WASM activation with manifest,
module, and executable revalidation, and the progressive
MCP discovery contract, explicit legacy/modern session lifecycle, and bounded
stdio JSON-RPC and HTTP JSON/SSE responses with explicit active-stream
server-request handling plus explicitly configured discovered-MCP host binding
are implemented, including bounded caller-driven GET streams with
`Last-Event-ID` resumption and capped retry reconnects, while broader HTTP
session behavior remains adapter work. The WASM adapter now embeds
Wasmi for a bounded ABI with eager module
validation, strict instance/memory/table limits, fuel metering, bounded
input/output, host capability admission, manifest-and-lease capability
checking, an opt-in bounded resource-read provider, and external output
provenance. Full WASI, broader effectful host imports, and preemptive wall-time
interruption remain later work. Do not move these concerns into the kernel.
