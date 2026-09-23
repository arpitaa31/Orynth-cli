# ADR-0018: Context-Derived Trust Closure

Status: accepted

## Context

Context blocks can reference dependency and source blocks. A caller may also
provide an explicit origin for the new block. Treating that requested origin as
authoritative would allow a block marked `TrustedProject` to be derived from a
web or MCP result without retaining the weaker provenance.

## Decision

`ContextGraph::publish` combines the requested trust with every referenced
dependency and source using the kernel `TrustOrigin` least-trusted rule. The
published block therefore carries the effective origin of its material inputs.

Replay validates the same invariant and rejects a persisted block that claims a
trust upgrade. It does not silently rewrite an invalid event, because the
event log must remain auditable and fail closed.

This is a provenance invariant, not content classification: the runtime does
not decide whether bytes are safe by inspecting their text. Capability policy
and projection filters remain separate concerns.

## Consequences

- Derived context cannot regain trust merely because a producer labels it as
  trusted.
- Existing context wire fields remain compatible; the rule is enforced at
  publication and replay boundaries.
- Other derived domains (assumptions, manager projections, plugins, and
  protocol adapters) must adopt the same invariant before the runtime can
  claim full cross-domain information-flow enforcement.
