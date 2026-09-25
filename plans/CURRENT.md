# Current Plan

## Phase 1: Foundation vertical slice - complete

Implemented and tested provider-independent IDs and lifecycle values, an in-memory immutable execution trace, a streaming provider contract, usage and cancellation, a deterministic mock provider, a basic agent loop, and readable configuration parsing.

Acceptance:
- the loop preserves logical AgentId while using a replaceable model assignment;
- ordered stream chunks produce one complete result and usage;
- request, chunk, completion, failure, and cancellation transitions are observable in the trace;
- cancellation stops acceptance of later chunks;
- valid and invalid configuration are tested;
- fmt, Clippy, check, and workspace tests pass offline.

Non-goals retained: persistence, context graph, cache claims, multi-agent transport, tools, security enforcement, network providers, plugins, MCP, A2A, terminal operations, and TUI.

## Phase 2: Runtime Core - hardened baseline

Implemented the Phase 2 runtime-core slices: an event-store contract, immutable sequencing, reconstruction of run/task/agent state, terminal transition validation, duplicate-event rejection, recorded replay, snapshots, incremental event reads, an in-memory content-addressed artifact boundary, dependency-free filesystem event/blob adapters, a bundled SQLite schema/backend, validated branch metadata for fork boundaries, versioned SQLite snapshot persistence, filesystem branch/snapshot sidecar persistence, child-run fork materialization, provider-backed child continuation, validated atomic continuation persistence, an explicit local locking/durability policy, and the initial durable artifact write lifecycle.

Acceptance evidence:
- thirty-seven event-store tests and five agent tests pass, including successful and failed runs, replay, snapshots, incremental reads, artifact deduplication, filesystem reopen, interrupted multi-frame batch recovery, torn-tail repair, filesystem metadata persistence/recovery, orphan-temp recovery, concurrent same-hash artifact writes, SQLite reopen, branch persistence/reopen, snapshot codec/load validation, fork materialization, provider-backed continuation, atomic batch append, filesystem lock release, concurrent SQLite writers, migration idempotence, duplicate-append atomicity, corruption rejection, invalid ordering, duplicate IDs, opaque context-event persistence, cached-usage event/snapshot preservation, durable cache-observation round trips, and unknown runs;
- reconstruction consumes stored events and never invokes a provider.

Next slice:
- retain fault-injection and directory-durability as hardening work; the active implementation phase is now the context graph below.

## Phase 3: Context - current slice complete

Implemented the first context vertical slice: typed versioned blocks, deterministic content addressing, global/team/private scopes, dependency invalidation, namespace subscriptions, bounded privacy-filtered projections, prompt rendering with stable-prefix hashing, replayable context transitions, a versioned opaque kernel-event envelope persisted by both event-store backends, runtime-service recovery that hydrates the graph from stored events, artifact-backed externalization for large content with hash-verified recovery, and an evidence-based provider cache telemetry projection with durable observation events and recovery. Provider cache hits are not inferred from semantic hashes.

Acceptance evidence:
- the Phase 3 acceptance subset covered sixteen context tests, four cache tests,
  47 event-store tests, and 31 runtime-service tests for graph hydration,
  artifact externalization, cache observation, and durable recovery; later
  Phase 4 additions are covered by the evidence below;
- context, cache, and runtime-service Clippy checks and workspace compilation pass.

Remaining Phase 3 follow-up:
- add provider-specific cache metadata adapters and automatic expiry selection;
  the active implementation phase is the multi-agent slice below.

## Phase 4: Multi-agent - coordination projections active

Implemented Phase 4A and 4B: a typed `orynth-ipc` envelope with sender/recipient
identity, run/task scope, causal event references, provenance, and narrow
message variants; bounded FIFO mailboxes with deterministic backpressure;
versioned `AgentMessage` events in both event-store backends; runtime recovery;
fork remapping of embedded message run scope; and an `orynth-assumptions` graph
with normalized deterministic conflict detection, affected-owner reporting,
versioned `AssumptionTransition` events, recovery, and fork remapping. The
current coordination slice adds deterministic budget and health projections,
atomic typed conflict notifications, replayable resource ownership, a compact
recovered manager projection, explicit model selection preserving logical agent
identity, and deterministic supervision pause/resume.
Child-agent spawning now atomically creates a child, charges the parent's child
budget, and records the parent/child relationship.

Acceptance evidence:
- six IPC tests cover envelope round trips, malformed/unsupported versions,
  bounded FIFO behavior, validation before enqueue, material-origin
  preservation, and legacy decoding;
- six assumption tests cover normalized equality/contradiction, trust-origin
  combination, persisted trust-upgrade rejection, legacy decoding, replay,
  and malformed/versioned payloads;
- 47 event-store tests cover filesystem message/assumption persistence and
  typed fork remapping; 32 runtime-service tests cover bounded delivery,
  assumption recovery, budget/health projections, conflict notifications, and
  SQLite reopen, failure-memory recovery, and cache-aware routing; twelve scheduler tests cover budget rejection, health
  thresholds, ownership/child relationships, durable budget transfer, and
  codec bounds;
- workspace check, formatting, and Clippy remain clean.

Next slice:
- continue Phase 6 with unrestricted/implicit activation,
  broader HTTP bidirectional session behavior,
  stronger OS filesystem/network sandboxing, full WASI/broader effectful
  host-import capability integration, preemptive WASM wall-time interruption, and broader platform
  enforcement; the current contract/adaptation
  slice covers bounded manifest discovery, plugin manifests, revalidated
  process/WASM activation, process supervision, progressive MCP discovery, MCP
  trust boundaries, and
  A2A-to-IPC translation;
  durable repair/impact/preview audit transitions, effect-boundary capability
  rechecks, multi-capability checks,
  rooted filesystem effects including find/copy/quarantine-backed remove,
  injected process fixtures, tool provenance policy,
  opaque capability-gated secret handles,
  and context-local trust filtering are covered by focused tests.

## Phase 7: Terminal AI - typed planning and rooted execution

Implemented bounded terminal environment discovery, typed operation planning,
risk/confirmation/disposition classification, path and Git validation, honest
compensation metadata, rooted find/copy/quarantine-backed remove execution, and
the explicit `orynth-cli`/`orynth-shell` boundary. A bounded local phrase
grammar translates recognized requests into typed operations and rejects
unsupported prose. The shell reports host facts and plans, and its confirmed execute path runs the rooted filesystem subset
through preview, execute, verify, and commit; it does not execute ambient shell
commands. Confirmed rooted copy/move/quarantine effects persist bounded
relative compensation records, and `orynth-shell undo` restores the latest
record after conflict checks.

Acceptance evidence:
- twelve terminal-tools tests cover environment discovery, typed planning,
  rooted filesystem effects, traversal rejection, compensation conflict
  handling, injected process policy, and path/Git validation;
- fourteen CLI tests cover configuration parsing, typed plan/execute parsing,
  bounded arguments, confirmation gating, blocked raw-shell preservation,
  verified filesystem execution, conservative local phrase translation, and
  unknown/incomplete flag rejection;
- the shell help, representative plan command, and isolated confirmed copy
  command run successfully, while model-backed natural-language translation,
  process/Git adapters, stronger crash semantics, and production terminal UX remain
  future work.

## Phase 8: TUI/debugger - projection-backed inspector slice

Implemented the first operator-facing slice: `orynth-tui` renders a bounded,
read-only text view over `RecoveredRun`, and `orynth inspect --db <path> --run
<id>` recovers a real SQLite run before rendering it. `orynth replay` recovers
recorded prefixes without providers, `orynth fork` persists a validated branch
and child prefix, and `orynth diff` compares recovered projections. The view exposes run
status, event/task/agent/artifact counts, context invalidations, messages,
assumptions/conflicts, tool and capability records, cache telemetry, manager
agent projections, recent events, and decoded semantic breakpoint hits.
`orynth debug` adds a read-only command-driven session for selecting event
coordinates and breakpoint output, with bounded pane and item navigation owned
by `orynth-tui`.

Acceptance evidence:
- three TUI tests cover projection-backed rendering, event-derived breakpoint
  scanning, and bounded pane/item navigation;
- six operator-app tests cover explicit argument requirements, successful
  recovery/rendering from a real SQLite run, recorded prefix replay, fork
  materialization, projection comparison, and debug-session navigation.

Remaining Phase 8 work:
- full-screen interactive terminal UI panes and keyboard navigation;
- live re-execution, interactive replay/fork/comparison, and semantic breakpoint
  controls;
- live subscription/update behavior with explicit mutation boundaries.

## Phase 9 preparation: budget transfer and failure memory

The first Phase 9 scheduler primitive is implemented: an existing agent can
durably transfer configured budget capacity to another existing agent by
selected dimension. Transfers preserve usage and logical identity, reject
source or recipient limits that would invalidate current usage, preserve
unlimited dimensions, and round-trip through the versioned scheduler codec.
Runtime membership checks and replay-backed projection recovery are covered.

The same runtime now exposes bounded event-sourced failure memory. Agents can
record exact fingerprints, approaches, reasons, and evidence references,
query prior attempts, resolve records without erasing history, and recover the
projection through SQLite. The inspector derives a breakpoint from recorded
failure entries.

Cache-aware routing now decorates candidates with only exact-prefix provider
observations and ranks them against caller-supplied cost/value policy. Warm
evidence can lower estimated cost; absent metadata never becomes a fabricated
hit, tie-breaking is deterministic, and an explicit caller timestamp can
reject stale, future-dated, or unverifiable cache savings.

Context proprioception now exposes a bounded read-only freshness dashboard for
active tokens, lifecycle counts, largest blocks, recent invalidations, pinned
blocks, and explicit policy pressure flags. The inspector reports the context
freshness summary without creating a second authority.

Runtime context inspection also exposes bounded principal- and trust-filtered
search plus direct dependency/source/dependent reports. These operations
recover the authoritative graph and do not append events or mutate access
timestamps.

Context lifecycle control now includes explicit replayable `Archived` and
`Restored` transitions. `ContextGraph` validates legal lifecycle changes, and
runtime service archive/restore operations persist them through SQLite or the
filesystem event boundary; artifact-store variants recover externalized content
before applying the transition. Explicit pin/unpin transitions persist
retention metadata without changing lifecycle or content; automatic
refresh/archive policy remains separate.

Supervision now has an explicit deterministic policy boundary. A caller can
provide a failure threshold, stronger model candidates, and the current
user-pin state; a promotable specialist is promoted through a durable model
selection event, while a blocked agent is paused when no promotion applies.
Candidate ordering and threshold validation are deterministic, and the policy
does not discover providers or infer model quality.

Remaining Phase 9 work includes richer profile-driven specialist selection,
provider-specific cache adapters and automatic expiry selection, automatic
routing policy, semantic failure classification, cross-run memory, automatic
context refresh/archive policy, and deeper active supervision/consultation.

## Phase A: deep-audit remediation - complete

The five S1 findings in `docs/CODE_AUDIT.md` were remediated in the current
worktree before any Phase B feature work. Shared resource matching now
normalizes traversal and all process/WASM callers use it; rooted filesystem
effects reject symlink/reparse components and authorize every concrete path;
quarantine and the persistent undo journal use unique durable transaction and
operation identities with no-overwrite allocation; durable IDs use a
process-unique random prefix; the filesystem event adapter persists
begin/event/commit batches and recovers only committed batches; and the MCP
HTTP transport binds normalized structured endpoint authority to a capability
context for POST, GET, and reconnect operations.

Validation and residual limitations are tracked in
`docs/CODE_AUDIT_REMEDIATION.md`.

## Phase B: deep-audit remediation - complete

ORY-AUDIT-006 through ORY-AUDIT-018 are remediated in the current worktree.
The implementation preserves Phase A and adds focused coverage for snapshot
identity, context freshness and replay, cache/budget evidence, health
resolution, validated runtime mutation boundaries, typed tool effects, CLI
translation, schema-declared syntax fields, and process executable policy.
The Phase B validation matrix and explicit remaining limitations are recorded
in `docs/CODE_AUDIT_REMEDIATION.md`; the architecture choices are recorded in
ADR-0062.

## Phase C: deep-audit remediation - complete

ORY-AUDIT-019 through ORY-AUDIT-025 are remediated in the current worktree.
The process adapter has bounded response and stdin I/O with timeout cleanup
and suspended-before-attach Windows containment; discovery and WASM activation
are bounded before allocation; plugin activation limits are cumulative; MCP
wire versions are negotiated and checked; and ownership is enforced as a
separate scheduler-backed policy across tools, terminal effects, plugin
adapters, and cancellation cleanup. The complete matrix and residual
limitations are in `docs/CODE_AUDIT_REMEDIATION.md`; the architecture record
is ADR-0063.

Phase C validation is complete for all seven scoped findings. Formatting,
workspace check, Clippy, release build, and the focused executable suites
that the host policy allowed all pass. The exact all-features workspace test
gate is ENVIRONMENT-BLOCKED by Windows Application Control (OS error 4551)
for generated test binaries; this is recorded in the remediation matrix.
## Phase D: provider, persistence, and release-hardening remediation - complete

Phase D addressed ORY-AUDIT-026 through ORY-AUDIT-032 without beginning the
benchmark/fuzzing phase. The completed work strengthens the provider-neutral typed
contract and deterministic mock, makes filesystem metadata replacement and
sidecar layout recoverable, keeps fork schema tags truthful, unifies durable
frame limits, aligns CI/status claims with evidence, and bounds obvious inline
payload/output retention.

The benchmark/hardening phase was intentionally deferred during Phase D and
was completed in Phase E. One directly related opaque-fork-tag finding was
discovered during the final adversarial pass, fixed, and recorded as
NEW-AUDIT-D-001.

## Phase E: benchmark and hardening - complete

Implemented `crates/benchmarks` as a dependency-light reproducible harness
with release startup, memory, context, event-store, SQLite, reconstruction,
snapshot, IPC, assumptions, tool, and stress scenarios. Added deterministic
fault smoke, malformed-input smoke, focused fuzz-target scaffolding, and raw
TSV/text evidence under `benchmarks/results/`. A measured conflict-heavy
assumption bottleneck was reduced with a subject index; the before/after
measurement and limitations are recorded in `docs/BENCHMARK_RESULTS.md`.

Updated `docs/HARDENING.md` and the audit remediation with fault, fuzz,
security, resource-boundary, and cross-platform evidence. Full cargo-fuzz is
not installed on the current host, and Windows Application Control blocked
some newly generated executables; these remain explicitly environment-blocked.

## Phase F: full-screen TUI/runtime debugger - complete

Implemented the projection-backed full-screen runtime debugger in
`orynth-tui` and the `orynth tui` command. The client provides dashboard,
agent, event/detail, context, IPC, tools, policy, assumptions, runs, and help
views; keyboard navigation/filtering/refresh; explicit empty/error/small
terminal states; SQLite run selection; bounded older event pages; and RAII
terminal restoration. The deterministic offline demo generates real runtime
events for agents, models, context, IPC, conflicts, health, budgets,
ownership, capability, cache, and tool transitions.

Focused TUI/runtime tests, workspace check, warnings-denied all-targets and
all-features Clippy, workspace all-features tests, formatter check, and
release build passed. Release working-set samples and known limits are in
`docs/TUI_IMPLEMENTATION_REPORT.md` and `docs/BENCHMARK_RESULTS.md`.

Next phase: Deep Audit #2. It has not started.
