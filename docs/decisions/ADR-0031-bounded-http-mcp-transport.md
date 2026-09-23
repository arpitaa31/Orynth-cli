# ADR-0031: Bounded HTTP MCP Transport

Status: accepted

## Context

MCP servers may be reached over HTTP, but transport behavior must remain
bounded and subject to the same host-owned capability policy as process-backed
plugins. Request/response adapters need to accept the protocol's JSON and SSE
response forms without implicitly opening an unbounded background session.

## Decision

`orynth-plugin-mcp` provides `HttpMcpTransport` for `http` and `https`
endpoints. Construction validates the endpoint and manifest. Connection
requires an MCP manifest, a matching Network capability declaration, a matching
host policy lease, and a valid lifecycle mode. Requests use bounded JSON-RPC
bodies and responses, validate HTTP success and JSON-RPC identity, and preserve
legacy session headers when a server supplies one. Modern requests carry the
protocol and client metadata required by the selected mode. SSE responses are
parsed incrementally with bounded event buffers. A server request found in the
active response stream is dispatched only through an explicit host handler,
and its result is returned with a policy-bound nested POST.

The implementation opens GET streams only through an explicit caller action,
supports caller-provided `Last-Event-ID` resumption, and honors bounded SSE
retry hints with at most two automatic reconnects. It does not provide the
newer long-lived bidirectional session semantics; that behavior requires a
separate transport extension and explicit tests.

## Consequences

Network MCP calls now have a concrete, policy-checked adapter and a local
loopback fixtures for JSON, active SSE response handling, and caller-driven GET
resumption. The kernel remains
unaware of HTTP framing, and HTTP transport features that could introduce
unbounded concurrency or new trust flows remain outside this slice.
