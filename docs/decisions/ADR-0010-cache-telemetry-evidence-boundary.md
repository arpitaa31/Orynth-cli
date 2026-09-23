# ADR-0010: Cache telemetry requires provider evidence

Status: accepted

## Context

Stable prompt-prefix hashes describe semantic renderer output. They do not
prove that a provider reused physical KV-cache state, and providers differ in
whether they report cached input tokens at all.

## Decision

Extend kernel `Usage` with optional `cached_input_tokens`. Providers that have
no cache metadata leave it absent. `orynth-cache` records observations only
when the field is present, keys them by provider, model, and prefix hash, and
rejects values larger than reported input tokens. `RuntimeService` exposes this
projection without turning a missing value into a cache miss or hit claim.

The usage field is preserved in durable model-completion events and versioned
snapshots. Explicit observations are also persisted as versioned
`CacheObserved` events; recovery rebuilds the aggregate projection from those
events. Scheduler integration remains a separate concern.

## Alternatives

- Infer hits from stable-prefix hashes: rejected because semantic equality is
  not physical provider cache evidence.
- Treat absent metadata as a miss: rejected because absence means unknown.
- Add provider-specific cache fields to the kernel: rejected because the
  kernel should carry the portable observation, not adapter-specific details.

## Consequences

Cache-aware behavior can be tested deterministically without fabricating
provider behavior. Provider adapters must opt in by supplying explicit usage
metadata. Durable observations are replayable, but cache-aware scheduling
remains later work.
