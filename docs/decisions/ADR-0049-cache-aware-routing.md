# ADR-0049: Evidence-based cache-aware model ranking

Status: accepted.

## Context

Prompt-prefix locality can make a more expensive model cheaper overall when a
provider has explicitly reused a large prefix. Orynth already records provider
cache metadata, but routing must not infer physical cache state from semantic
context hashes or missing usage fields.

## Decision

Expose a read-only deterministic ranking primitive. The runtime decorates each
caller-supplied model candidate with the latest observation for the exact
provider, model, and prefix hash. The scheduler computes an effective
estimated cost by subtracting caller-supplied value for explicitly observed
cached tokens when `prefer_warm_cache` is enabled. Missing observations add no
value; explicit zero remains an observation with zero savings. Equal scores are
resolved by provider, model, and model class.

## Alternatives

- Infer warmth from a semantic prefix hash alone: rejected because semantic
  identity is not physical provider KV-cache state.
- Treat missing metadata as a cache miss: rejected because providers may not
  report cache usage at all.
- Make routing depend on cache availability: rejected because cache state is
  an optimization and must not affect correctness.

## Consequences

Cache affinity can influence deterministic routing without provider calls or
fabricated hits. Cost estimates, token value, expiry, provider adapters, and
automatic model selection remain explicit policy inputs for later work.
