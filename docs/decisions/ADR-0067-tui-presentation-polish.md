# ADR-0067: Human-oriented TUI presentation layer

## Status

Accepted for Phase F.1.

## Decision

Keep runtime projections and typed event payloads as the source of truth, but
introduce a central presentation layer in `orynth-tui` for the primary control
room. It maps agents, events, messages, context, tools, permissions,
conflicts, assumptions, and runs to stable plain-English summaries. Ratatui
renders those summaries in responsive bordered panels and reusable detail
overlays; technical IDs, payloads, provenance, and policy values remain in
the overlays.

## Rationale

The earlier debugger exposed correct runtime data but required users to decode
internal names and opaque identifiers before understanding what was happening.
A shared presentation mapping keeps wording consistent across Overview,
Activity, and inspectors without duplicating runtime authority or inventing
fake activity.

## Consequences

- Primary screens are understandable to a non-expert operator while retaining
  technical inspectability.
- The TUI remains read-only and projection-backed; presentation state cannot
  mutate runtime state.
- New runtime event kinds need a readable mapping and a bounded fallback.
- Responsive layouts, overlays, and human-readable labels become part of the
  maintained interaction contract and are covered by focused tests.
