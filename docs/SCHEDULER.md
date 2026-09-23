# Scheduler

Status: canonical specification derived from the supplied research blueprint.

Scheduling considers task complexity, capabilities, latency, cost, context size, reliability, tool reliability, cache affinity, user preference, privacy, and explicit model pins. Routing is always overridable.

Budgets cover tokens, money, wall-clock time, tool calls, children, and context. Health signals include repeated failures, no progress, context/budget pressure, assumption conflicts, verification failures, and dependency invalidation. Model class is policy metadata, not a hardcoded persona.

The active coordination projection exposes deterministic per-agent budget
limits for tokens, money in integer micros, wall-clock milliseconds, tool
calls, child agents, and context tokens. Usage is event-sourced and a runtime
write is rejected before append if it would exceed a configured limit.

Health is also event-sourced from explicit signals: repeated failures/tool
errors, no progress, context pressure, budget pressure, assumption conflicts,
verification failures, and dependency invalidation. Replay derives stable
`Healthy`, `Degraded`, or `Blocked` states using fixed thresholds; no LLM or
wall-clock sampling is involved in the projection.

Ownership is a deterministic resource-to-agent projection in the same
coordination event stream. Claims by a different current owner are rejected;
only the current owner can release a resource. Runtime APIs enforce that both
claimants and releasers belong to the run before appending transitions.

Runtime recovery also derives a compact manager projection per run. It joins
agent identity/model/status and usage with health, budget, owned resources,
assumption IDs, affected conflict IDs, and all/active failure IDs without
embedding full transcripts. The run-level projection also exposes bounded
context freshness/pressure, cache-observation count, artifact count, and active
failure count. These are replay-derived observability fields, not provider
sampling or automatic supervision decisions.

`RuntimeService::select_model` appends a durable `ModelRequested` transition
for an existing agent. This permits explicit promotion or demotion decisions
while keeping the logical agent identity stable; automatic policy triggers and
provider selection remain separate. `RuntimeService::supervise_agent_with_policy`
adds a deterministic policy boundary: it can promote a profile-marked
promotable agent after a caller-supplied consecutive-failure threshold, using
only caller-supplied stronger candidates, or pause a blocked agent when no
promotion applies. A caller-supplied user-pin bit prevents promotion. The
policy has no provider, clock, or model-quality inference and preserves the
same `AgentId`.

`RuntimeService::spawn_agent` appends a child identity and relationship with a
parent `child_agents` usage charge as one batch. The scheduler rejects the
operation before append when the parent budget would be exceeded.

`RuntimeService::transfer_budget` appends a durable `BudgetTransferred`
transition for an existing source and recipient agent. A transfer moves only
configured capacity for the selected dimensions; usage and agent identity do
not move. It is rejected when the source lacks sufficient configured capacity,
when the source limit would fall below its existing usage, or when the
recipient's resulting limit would fall below its existing usage. Unlimited
dimensions remain unlimited and cannot be converted into finite capacity by a
transfer. The transition is replayed through the same scheduler projection and
is encoded with an explicit versioned tag.

The read-only `RuntimeService::rank_cache_candidates` path decorates caller
model options with an observation only when `CacheTelemetry` contains an exact
provider/model/prefix record. `rank_cache_aware_candidates` applies an
explicit `CacheRoutingPolicy` to caller-supplied estimated costs and cached
token value, then resolves ties deterministically. The `_at` variant accepts an
explicit routing timestamp and only applies a configured maximum observation
age when the evidence is non-future and within that window. Missing, stale, or
clock-unverifiable observations do not become cache misses or hits, and the
ranking never mutates state or invokes a provider.

Automatic non-cache model routing, provider-specific cost/expiry adapters,
provider discovery, and broader policy-driven supervision remain future work.
