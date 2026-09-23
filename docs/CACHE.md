# Cache

Status: canonical specification derived from the supplied research blueprint.

Separate semantic context state, prompt rendering, and provider cache telemetry. Stable prompt layers should precede volatile task/results layers: runtime invariants, conventions, tool contract, shared context, identity/permissions, changing blocks, task, and recent results.

Semantic invalidation does not imply physical provider KV-cache patching. Orynth never fabricates cache hits; observations come from provider usage metadata or are unavailable. Cache affinity is a scheduler optimization, never a correctness dependency.

Phase 1 records usage only. Phase 3 now provides deterministic prompt rendering, stable-prefix hashes, an optional provider-reported `cached_input_tokens` usage field, and an evidence-based `CacheTelemetry` projection keyed by provider, model, and prefix hash. Missing metadata creates no observation; explicit zero is recorded as an observation but is not called a hit; inconsistent values are rejected. These hashes describe semantic renderer output only, and no cache hit is inferred from them. `RuntimeService::record_cache_usage` persists explicit observations and rebuilds the aggregate during recovery.

The bounded Phase 9 routing slice exposes `RuntimeService::rank_cache_candidates`.
It looks up observations for the exact model/prefix pair and delegates to the
deterministic scheduler ranking policy. Callers provide estimated model costs
and the value assigned to each explicitly cached token. A recorded positive
observation can lower the effective estimated cost; missing observations never
receive cache value, and an explicit zero contributes no savings. Ties resolve
by provider, model, and class. The explicit `rank_cache_candidates_at` API can
also apply a caller-supplied maximum observation age: stale, future-dated, or
clock-unverifiable observations remain visible but cannot lower effective cost.
This is a routing hint, not a correctness dependency or a claim that a future
provider request will hit its physical KV cache. Provider-specific adapters,
automatic expiry selection, and broader automatic model selection remain
future work.
