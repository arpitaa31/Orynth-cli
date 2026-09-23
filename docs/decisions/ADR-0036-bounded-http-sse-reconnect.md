# ADR-0036: Bounded HTTP SSE Reconnect

Status: accepted

## Context

MCP Streamable HTTP permits an SSE response to advertise a retry interval and
resume a disconnected stream with an event cursor. Retrying without bounds
could turn a request into an unobservable background session or allow an
untrusted server to impose an excessive delay.

## Decision

`HttpMcpTransport` records valid SSE event IDs and retry hints. An active
request response may resume through a GET carrying `Last-Event-ID`; an explicit
caller-opened GET stream may do the same when the server advertised a retry
hint. Each operation allows at most two reconnects. Retry delays are clamped
to five seconds, malformed retry fields are ignored, and resumption is never
attempted without a valid event ID.

Reconnects remain synchronous and host-owned. The adapter does not create a
background stream, infer cancellation, or expand the server's authority.
Long-lived bidirectional session semantics remain a separate design.

## Consequences

Disconnects after a resumable SSE event can complete without losing the active
request, and caller-opened streams can honor protocol retry guidance while
remaining bounded. The retry and resume behavior is covered by loopback HTTP
fixtures for both active POST responses and GET streams.
