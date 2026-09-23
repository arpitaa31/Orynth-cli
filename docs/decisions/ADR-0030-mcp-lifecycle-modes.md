# ADR-0030: Explicit MCP Lifecycle Modes

Status: accepted

## Context

MCP deployments span a legacy lifecycle with an `initialize`/
`notifications/initialized` exchange and a modern stateless lifecycle that
carries negotiation per request. A transport adapter must not silently apply
the wrong handshake or infer that a connection is ready.

## Decision

`orynth-plugin-mcp` models two explicit modes:

- `Legacy2025`, targeting `2025-11-25`, performs initialize, validates the
  returned server information, sends the initialized notification, and only
  then permits requests;
- `Modern2026`, targeting `2026-07-28`, performs no legacy handshake and still
  requires the host to mark the session connected before requests are routed.

`McpSessionTransport` owns wire framing and request/response encoding. The
session state machine owns lifecycle ordering, bounded request validation, and
mode selection. `SessionMcpInvoker` adapts a connected session to the existing
untrusted-result `McpAdapter` boundary.

The bounded stdio JSON-RPC and Streamable HTTP transports implement this
contract. HTTP POST responses may be JSON or SSE, with active-stream server
requests available only through an explicit host handler. Bounded caller-driven
GET streams and `Last-Event-ID` resumption are supported, with capped SSE
retry reconnects. Broader bidirectional session behavior remains separate
follow-up work. The session contract does not grant capabilities or trust
server output.

## Consequences

Transport implementations can support either protocol era without duplicating
lifecycle policy. Modern stateless operation is not accidentally forced through
the legacy handshake, and unconnected sessions fail closed.
