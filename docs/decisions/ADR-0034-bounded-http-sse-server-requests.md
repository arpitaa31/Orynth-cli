# ADR-0034: Bounded HTTP SSE Server Requests

Status: accepted

## Context

MCP Streamable HTTP POST responses may use either JSON or server-sent events.
An active SSE response can carry a server request before the response matching
the client's request. Treating the stream as JSON-only loses protocol behavior,
while accepting arbitrary background messages would create an implicit session
and effect authority.

## Decision

`HttpMcpTransport` advertises both JSON and SSE, parses SSE incrementally, and
limits each buffered response/event by the plugin manifest message bound and a
fixed 1 MiB ceiling. The transport returns when it receives the matching
JSON-RPC response, while notifications are ignored and malformed or oversized
events fail closed.

Server requests are accepted only on the active response stream and only when
the host explicitly supplies `McpServerRequestHandler`. The handler receives
the numeric request ID, method, and untrusted JSON parameters. Its result or a
generic JSON-RPC failure is returned as a bounded nested POST to the same
policy-authorized endpoint. No handler is installed by default.

Caller-driven GET streams and `Last-Event-ID` resumption are now supported by
the same bounded SSE parser. Server retry hints trigger at most two automatic
reconnects, while cancellation coordination and newer long-lived
bidirectional session semantics remain deferred and require separate lifecycle,
replay, and policy tests.

## Consequences

The HTTP adapter now covers bounded JSON/SSE POST responses and an explicit
active-stream server-request path without moving HTTP framing or authority
into the kernel. The explicit handler preserves host ownership of effects, but
does not claim full MCP streaming-session support.
