# Context

Status: canonical specification derived from the supplied research blueprint.

Context is a typed, versioned, addressable graph, not a fundamental `Vec<Message>`. A block has identity, revision, kind, owner, scope (GLOBAL, TEAM, or PRIVATE), content hash, token estimate, trust, importance, lifecycle, dependencies, sources, creation event, and last access.

Agents receive projections and references such as `context://schema/users@15` or `artifact://migration/users_uuid` rather than duplicated histories. Superseding a dependency marks dependent state stale and emits targeted invalidation. Namespace subscriptions wake relevant agents only. Deterministic normalized assumption conflicts remain deterministic; fuzzy semantics may use a model later.

The first Phase 3 slice implements an in-memory graph store, immutable content addressing, namespace revisions, global/team/private visibility, dependency invalidation, targeted subscriptions, bounded projections, prompt rendering with a deterministic stable-prefix hash, and a replayable `ContextTransition` log. Content remains referenced by hash rather than copied into block metadata; projected prompt text is a derived view.

Context blocks retain explicit trust levels alongside privacy scope. Projection
requests and prompt rendering can apply deterministic trust policies: allow all
sources, exclude external sources while retaining generated context, or admit
only trusted-project and user-provided context. When a block is derived from
dependencies or sources, publication combines the requested origin with every
referenced origin, and replay rejects persisted trust upgrades. This remains a
provenance invariant rather than content classification; broader cross-domain
propagation is still a later task.

`ContextGraph::proprioception` provides a bounded, read-only freshness
dashboard. `ContextFreshnessPolicy` can cap active tokens, stale-block count,
largest-block summaries, and recent invalidations; the result reports active
tokens, lifecycle counts, bounded summaries, pinned-block counts, and pressure
flags. It never archives, restores, or invalidates a block implicitly. Explicit
`ContextGraph::archive`/`restore` transitions and the corresponding runtime
service operations preserve immutable content while changing only lifecycle;
they are validated, replayable, and durable. Explicit `pin`/`unpin` transitions
now persist retention metadata without changing lifecycle or projection.
Automatic refresh/archival, pin policy, and agent-facing context tools remain
later policy work.

Read-only context inspection now also includes bounded, visibility- and
trust-filtered text/namespace search plus direct dependency/source/dependent
reports. Runtime facades recover the authoritative graph before serving these
queries, so inspection does not create a parallel context authority or append
events.

Context transitions are encoded as versioned opaque kernel events and survive both event-store backends, with bounded decoding and recovery into a `ContextEventLog`. Large blocks may be externalized into the artifact store; the transition carries the opaque artifact digest, and runtime recovery resolves the bytes and verifies the context hash before hydration. The runtime event store preserves event bytes and ordering but does not interpret the context domain; `orynth-runtime` composes the store, artifact, and context crates to hydrate a recovered `ContextGraph`. Provider cache observations are not inferred from semantic hashes. Explicit provider usage metadata is projected into durable `CacheObserved` events; exact-prefix cache-aware candidate ranking consumes only those observations, while caller-supplied freshness is enforced without inferring provider TTLs. Provider-specific adapters, automatic expiry selection, and broader automatic scheduling remain future work.
