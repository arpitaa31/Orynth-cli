# ADR-0025: Progressive MCP Discovery

Status: accepted

## Context

MCP servers can expose more tools and resources than should be eagerly loaded
into an agent context. Server-provided names, descriptions, schemas, and
cursors are external metadata and must not expand memory or authority without
host policy.

## Decision

`orynth-plugin-mcp` exposes caller-driven paged discovery for tools and
resources. The host chooses a bounded page size, supplies an optional bounded
cursor, and validates every returned page, cursor, description, URI, and input
schema before making it available to the runtime. Discovery does not grant
capabilities; tool binding still requires host-owned capability, risk,
reversibility, and trust policy.

The adapter also owns an explicit lifecycle contract for legacy handshake and
modern sessionless modes. Concrete server sessions over stdio/HTTP remain
outside the kernel; the bounded stdio transport is implemented and HTTP can be
supplied by a later MCP transport.

## Consequences

- Large server catalogs can be inspected incrementally without eager context
  growth.
- Malformed or oversized metadata fails closed at the edge.
- A discovered tool remains only a description until an explicit host policy
  binds it.
