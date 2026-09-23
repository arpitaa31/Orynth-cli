# ADR-0054: Caller-Supplied Cache Observation Freshness

Status: accepted.

## Context

Provider cache telemetry is explicit evidence keyed by provider, model, and
stable prompt-prefix hash. An observation can become stale, but Orynth does not
own provider-specific TTL knowledge and must not infer a physical KV-cache hit
from semantic context state.

## Decision

Carry the observation timestamp into cache-aware routing and allow the caller to
provide both a routing timestamp and a maximum observation age. A timestamped
observation lowers effective estimated cost only when it is not future-dated and
is within the configured window. Missing timestamps, missing routing time,
future observations, and stale observations remain visible as telemetry but do
not create cache savings. The compatibility ranking API keeps no-expiry
behavior when no age limit is configured.

## Consequences

- freshness decisions are deterministic and replayable at the routing boundary;
- provider adapters may later supply provider-specific age policy without
  changing the evidence model;
- semantic invalidation and physical KV-cache mutation remain separate;
- automatic expiry selection and provider-specific adapters remain future work.
