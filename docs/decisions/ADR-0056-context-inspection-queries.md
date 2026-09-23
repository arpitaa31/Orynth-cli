# ADR-0056: Bounded Context Inspection Queries

Status: accepted.

## Context

Models and operators need to find relevant context and understand dependency
impact without receiving the complete graph or copying large content into every
projection. Inspection must honor scope/trust visibility and must not become a
second source of runtime state.

## Decision

Expose bounded, read-only search and dependency reports on `ContextGraph`, with
runtime facades that recover the authoritative graph before answering. Search
matches namespace or UTF-8 content text, filters by principal, trust, and
lifecycle, and returns deterministic metadata summaries rather than transcript
copies. Dependency reports return direct dependencies, sources, and a bounded
sorted dependent list.

## Consequences

- context discovery remains useful without widening prompt projections;
- inspection cannot silently update access times, lifecycle, or events;
- external callers still need the artifact-backed recovery path for
  externalized content;
- richer semantic/fuzzy search and agent-facing tool authorization remain
  later layers.
