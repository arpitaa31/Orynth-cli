# ADR-0032: Explicitly Configured MCP Host Binding

Status: accepted

## Context

Discovery can validate MCP metadata, but it cannot safely infer where a server
may be reached or which agent is allowed to reach it. The host also needs a
client identity, lifecycle mode, expected server metadata, and a timeout before
it can construct a usable adapter.

## Decision

`PluginHost::activate_directories_with_mcp` accepts an explicit
`McpActivationSpec` map keyed by discovered plugin ID and an
`McpActivationContext` containing the agent-scoped capability policy. Every
selected MCP candidate must have a spec. The host revalidates the manifest,
constructs the bounded HTTP transport, connects it in the selected lifecycle
mode, and stages the resulting adapter atomically with other candidates.

The HTTP endpoint must be declared by the MCP manifest and authorized by the
provided network lease. The default activation method continues to exclude MCP
and never creates network authority implicitly. MCP metadata and results remain
untrusted at the adapter boundary.

## Consequences

Configured discovered MCP servers can now be routed through the same host
registry as process and WASM plugins. Activation is explicit and testable, but
unrestricted discovery, implicit endpoint selection, and broader bidirectional
MCP behavior remain outside this slice. The bounded HTTP adapter handles SSE
retry reconnects only within its explicit request/stream lifecycle.
