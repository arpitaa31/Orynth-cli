# Persistence

## SQLite boundary

Schema version 2 is implemented with migration metadata plus events, snapshots, artifacts, and branches. Events use a store sequence as the primary key and keep event IDs unique; payloads and checksums remain opaque to the storage layer. Artifact references retain shared trust origins, with a transactional migration for the SQLite trust tag. The current SQLite artifact adapter stores payload bytes inline for deterministic local tests; an external content-addressed blob locator remains a later optimization for large artifacts.

Migrations are transactional, checksummed, idempotent when already current, and fail closed on unknown future versions. The bundled SQLite adapter currently passes reopen, reconstruction, migration-idempotence, duplicate-append atomicity, artifact validation, branch persistence, snapshot codec/load validation, batch-append atomicity, and concurrent-writer tests. It uses a five-second busy timeout, SQLite's rollback journal, and `synchronous = FULL`; `IMMEDIATE` transactions serialize writers for events, branches, snapshots, and artifacts.

Status: canonical specification derived from the supplied research blueprint.

Important transitions are immutable events and durable materialized state is reconstructable from them. SQLite is the default local candidate for events, branches, snapshots, metadata, and debugging; large immutable artifacts may use content-addressed blobs. Versioned opaque context transitions remain in the same event sequence; `orynth-runtime` recovers them into the typed context graph after the store has validated and decoded the event records. Large context payloads can be externalized to the artifact store and are accepted only after their bytes match the context block hash; the block's trust origin is propagated to the artifact event and blob metadata. Provider-reported cached-input usage is preserved in model-completion events and snapshots, while explicit cache observations are persisted as `CacheObserved` events and rebuilt into the recovered telemetry projection. Typed IPC envelopes are persisted as versioned `AgentMessage` events, decoded during runtime recovery, and remapped with their child run scope during fork materialization. IPC schema version 2 persists bounded material-input origins and version-1 envelopes decode without them. Assumption creation, state changes, and deterministic conflicts are persisted as versioned `AssumptionTransition` events and replayed into the recovered graph. Assumption schema version 2 persists direct and bounded material-input trust origins; version-1 transitions decode with generated provenance. Budget configuration/usage, health signals, and resource ownership transitions are persisted as versioned `SchedulerTransition` events and replayed into deterministic projections. Model selection and pause/resume lifecycle changes are immutable agent events. Child creation, parent budget usage, and parent/child linkage are appended as one validated batch. Exact-prefix cache-aware ranking is a read-only projection over recovered telemetry; provider-specific expiry and broader automatic scheduling remain future work.

Recorded replay uses captured outputs and never calls providers. Live re-execution may diverge. Forks preserve ancestry and isolate later state.

Artifact trust tags are backward-compatible: legacy event and blob payloads
default to generated provenance, while new SQLite, filesystem, and snapshot
records persist explicit origins. Tool proposals and transaction state changes
are persisted as versioned
`ToolTransition` events and replayed into the runtime tool-history projection.
Proposal schema version 2 persists bounded material input origins and continues
to decode version-1 proposals with no material-origin list.
The durable audit records intent and lifecycle state; injected executors and
verifiers remain outside the event-store effect boundary.

Repair audits and bounded impact previews now use the same versioned transition
envelope and recover alongside the proposal/state record. Capability
requirements may be registered as multiple independent checks, while the
current event still records the tool transaction rather than platform policy
implementation details.

Phase 1 returns an in-memory execution trace. Phase 2 currently supports in-memory, dependency-free filesystem, and SQLite event/blob adapters: traces can be reopened, reconstructed, replayed from captured events, snapshotted to a sequence, incrementally read, stored as immutable payloads behind content references, and forked into a new child run by remapping an authoritative prefix. SQLite and the filesystem sidecar persist validated branch metadata that pins a fork to an existing parent event sequence and versioned snapshots derived from the authoritative event prefix. `AgentSession::run_fork` can continue a non-terminal child prefix through a provider while preserving logical agent identity, and `persist_fork_continuation` validates the complete child trace before atomically appending only its continuation through each backend's batch contract. The filesystem backend enforces a single-open OS advisory lock for events and per-hash locks for artifacts, writes artifact payloads through synced deterministic temporary files, and repairs orphaned temporary artifacts on the next write. Metadata uses an explicit sibling sidecar (`<event-file>.meta`) with unambiguous naming, legacy migration when safe, and recoverable backup generations during replacement. SQLite serializes concurrent writers with bounded busy waiting and full synchronous transactions. Event, metadata, and artifact records carry deterministic checksums; incomplete final frames are repaired on open and complete corruption is rejected. Inline artifact payloads are bounded at 16 MiB; external content-addressed storage remains future work. Fork remapping re-encodes successfully decoded legacy IPC, assumption, and tool payloads with the current schema tag while preserving opaque undecodable payloads with their original tag. Fault-injection and directory-entry durability remain limited; impossible crash guarantees are not claimed.
