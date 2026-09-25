# Status

Project: Orynth
Stage: Phase F - full-screen TUI/runtime debugger complete
Blueprint: SUPPLIED IN CURRENT RESEARCH BRIEF
Implementation: Phase 2 runtime core hardened; Phase 3 context slice complete; Phase 4A/4B plus budget/health projections complete; Phase 5 security/tools slice complete; Phase 6 plugin/protocol contracts active; Phase 7 terminal planning and rooted execution active; Phase 8 inspector slice complete; Phase D provider/persistence hardening complete; Phase E benchmark and hardening complete; Phase F full-screen TUI/runtime debugger complete
Current Phase: Phase F - full-screen TUI/runtime debugger complete; next Deep Audit #2

Phase A audit remediation: ORY-AUDIT-001 through ORY-AUDIT-005 have been
addressed in the current worktree with focused regression coverage. The
filesystem boundary now rejects symlink/reparse traversal and authorizes
concrete source/destination paths; terminal undo uses unique quarantine
objects and validated internal state; durable IDs use a process-unique
restart-safe prefix; filesystem event batches use commit markers; and MCP
HTTP authorization uses normalized structured destinations with redirects
disabled. Remaining limitations are documented in
`docs/CODE_AUDIT_REMEDIATION.md`.

Phase B audit remediation: ORY-AUDIT-006 through ORY-AUDIT-018 have been
addressed in the current worktree. Snapshot identity/effective-model
separation, archived-stale context propagation, active health resolution,
cache/budget evidence gates, centralized runtime mutation validation, typed
tool-effect outcomes, schema-declared syntax normalization, CLI preservation,
and deny-by-default process executable policy are covered by focused tests.
Details and limitations are recorded in `docs/CODE_AUDIT_REMEDIATION.md` and
ADR-0062.

Phase C audit remediation: ORY-AUDIT-019 through ORY-AUDIT-025 are fixed in
the current worktree. Process response and stdin I/O are bounded and
deadline-cleaned; Windows contained processes are suspended before Job Object
assignment and containment closes on cancellation; manifest, WASM, and
directory discovery reads are bounded; plugin activation limits are
cumulative; MCP wire negotiation and HTTP ownership admission are validated;
and scheduler-backed ownership is enforced at tool, terminal, plugin, and
cancellation boundaries. Details, tests, and residual limitations are
recorded in `docs/CODE_AUDIT_REMEDIATION.md` and ADR-0063.

Completed:
- repository bootstrap and workspace validation;
- canonical specification and dependency direction;
- phase boundaries and current acceptance gate.
- Phase 1 foundation slice;
- in-memory event-store contract, immutable sequencing, and state reconstruction;
- recorded replay over captured events without provider invocation.
- snapshot metadata and incremental event reads over the in-memory store.
- dependency-free filesystem event/blob adapters with reopen and torn-tail tests.
- SQLite event/artifact adapters with transactional schema v2 migration, reopen,
  duplicate-event atomicity, and checksum-validated reads.
- backend-neutral branch metadata with validated fork sequences and SQLite
  persistence across reopen.
- versioned SQLite snapshot persistence with checksum validation, immutable
  re-save behavior, event-prefix validation, and reopen/load tests.
- dependency-free filesystem snapshot/branch sidecar persistence with metadata
  reopen, torn-tail repair, and corruption rejection tests.
- child-run fork materialization across in-memory, filesystem, and SQLite
  stores, with prefix remapping, isolated run identity, and terminal-boundary
  validation.
- provider-backed child continuation through `AgentSession::run_fork`, with
  logical-agent identity preservation and continuation-event persistence.
- backend-neutral continuation persistence through an atomic `append_batch`
  contract, prefix/state validation, and filesystem/SQLite batch-append tests.
- explicit local persistence locking policy: OS advisory locking for the
  filesystem backend and serialized SQLite writers with busy-timeout and full
  synchronous durability settings.
- initial durable artifact lifecycle: per-hash filesystem locking, synced
  temporary writes, atomic rename, orphan-temp recovery, and SQLite
  transactional validation.
- in-memory typed context graph with versioned blocks, content addressing,
  global/team/private scopes, dependency invalidation, namespace subscriptions,
  privacy-filtered projections, stable-prefix prompt rendering, replayable
  context transitions, and a versioned opaque kernel-event envelope with
  bounded decoding and filesystem/SQLite persistence coverage;
- runtime-service recovery that hydrates `RuntimeState` and `ContextGraph`
  together from the same stored event sequence, with in-memory, filesystem,
  and SQLite reopen coverage and malformed-event rejection.
- artifact-backed large context content with opaque artifact digests,
  artifact-event sequencing, and hash-verified recovery.
- artifact provenance is carried through in-memory, filesystem, and SQLite
  stores, event reconstruction, snapshots, and context externalization;
  same-content writes combine origins without upgrading trust.
- bounded context proprioception now reports active-token usage, stale/
  archived/invalidated lifecycle counts, largest-block summaries, recent
  invalidations, pinned-block counts, and explicit freshness pressure flags
  without mutating the graph.
- bounded context search and dependency reports are available through the
  recovered runtime graph with principal visibility and trust filtering;
  queries are read-only and deterministically bounded.
- explicit context archive/restore lifecycle transitions are validated,
  replayable, durable through the runtime service, and usable with external
  artifact resolution. Explicit pin/unpin transitions now persist retention
  metadata without changing lifecycle or content; automatic archival/refresh
  policy remains separate.
- evidence-based cache telemetry keyed by provider, model, and stable-prefix
  hash, with optional provider usage metadata, durable `CacheObserved` events,
  recovery-time runtime aggregation, and deterministic exact-prefix cache-aware
  candidate ranking that never fabricates hits; explicit caller-supplied
  freshness windows reject stale, future-dated, or clock-unverifiable savings.
- typed internal IPC with versioned envelopes, causal references, direct and
  material-input provenance, typed message variants, bounded FIFO mailboxes, durable `AgentMessage` events,
  runtime recovery, and fork-safe run-scope remapping.
- explicit runtime peer-consultation helpers persist bounded question/answer
  messages through the same membership and mailbox checks without copying
  transcripts.
- deterministic normalized assumptions with owner/evidence metadata, conflict
  transitions, affected-owner reporting, durable `AssumptionTransition` events,
  runtime recovery, fork-safe assumption run-scope remapping, and bounded
  trust/material-input provenance with legacy decoding.
- deterministic scheduler projections for per-agent token, money, wall-clock,
  tool-call, child-agent, and context budgets; budget overruns are rejected
  before append, and health signals replay into stable healthy/degraded/blocked
  states through durable `SchedulerTransition` events.
- automatic conflict notifications as runtime-originated typed IPC, with
  all-recipient capacity checks and atomic assumption-plus-notification append.
- deterministic resource ownership claims/releases with conflict rejection,
  runtime membership checks, and replayable `SchedulerTransition` state.
- compact recovered manager projections with agent identity/model/status, usage,
  health, budget, owned resources, assumptions, affected conflicts, per-agent
  failure IDs, and active failure IDs; run-level context freshness/pressure,
  cache-observation, artifact, and active-failure summaries are also exposed.
- manager projections expose the effective trust origin of each owned
  assumption, and runtime conflict notifications retain both claim origins.
- durable runtime model selection that can promote or demote an agent while
  preserving its logical `AgentId`.
- durable agent cancellation preserves logical identity and rejects repeated
  cancellation after a terminal state.
- deterministic supervision pause/resume lifecycle events, including automatic
  pause when recovered health is `Blocked` and explicit resume control.
- atomic child-agent spawning with parent/child relationships and parent
  `child_agents` budget enforcement.
- bounded dynamic specialist profiles with role, scope, subscriptions,
  descriptive capability requirements, promotability, atomic child creation,
  filesystem/SQLite event persistence, recovery, and manager projection
  exposure; profile metadata does not grant capabilities by itself.
- deterministic read-only specialist selection now filters recovered profiles
  by role, required scope/capabilities, and promotability without choosing
  models or granting authority.
- deterministic policy-driven supervision can promote a promotable specialist
  after an explicit failure threshold using caller-supplied stronger model
  candidates, respects user-pinned routing, preserves `AgentId`, and pauses
  blocked agents when no promotion applies.
- durable, replayable budget transfers between existing agents: configured
  capacity moves per selected dimension while usage and logical agent identity
  remain fixed; source/recipient membership and usage-safe bounds are enforced
  before append.
- bounded event-sourced failure memory records attempted approaches, exact
  fingerprints, reasons, and evidence references; runtime membership checks,
  SQLite recovery, resolution, exact queries, and read-only inspector
  breakpoints are covered without copying transcripts.
- first security/tools contracts: scoped capability leases and a typed tool
  transaction pipeline with approval, verification, commit, and compensation.
- durable capability grant/revoke transitions with runtime membership checks and
  filesystem/SQLite recovery.
- opaque in-memory secret handles bound to agent/task scope, with current
  Secrets-lease checks on issue and resolve and redacted debug output.
- Phase 7 terminal planning boundary: bounded host-environment discovery,
  typed operation plans, deterministic risk/disposition classification,
  confirmation gating, path/Git validation, and honest compensation metadata;
  rooted find/copy/quarantine-backed remove execution is verified; the
  `orynth-shell plan` renders bounded reports, while explicit `execute` commands
  run the rooted filesystem subset through preview/execute/verify/commit. Full
  model-backed natural-language translation, process/Git adapters, and stronger
  crash semantics remain later work; a bounded local phrase grammar rejects
  unsupported prose.
- Phase 8 inspector slice: `orynth-tui` renders a bounded view over a recovered
  runtime projection, and `orynth inspect --db <path> --run <id>` opens a real
  SQLite event store and recovers that run. The inspector is read-only and does
  not call providers or fabricate sample state; its semantic breakpoint scanner
  derives model changes, context invalidation, assumption conflicts, capability
  grants, approval gates, tool repairs, and tool failures from persisted events.
  Recorded prefix replay, persisted fork materialization, and recovered
  projection comparison are also available through `orynth replay`,
  `orynth fork`, and `orynth diff`; live re-execution and interactive controls
  remain later work. `orynth debug` now provides a terminal-independent,
  line-oriented read-only session with `show`, `events`, `event <sequence>`,
  `breakpoints`, pane switching, and bounded item selection/navigation.
- durable, versioned tool proposal/state transitions with runtime `ToolHistory`
  recovery and SQLite reopen coverage.
- tool capabilities are rechecked immediately before execution and
  compensation, so expiry or revocation after preflight cannot authorize an
  effect.
- deterministic safe proposal repair with ambiguity rejection, injected impact
  previews, a rooted reversible filesystem fixture, and an injected typed
  process fixture that rejects shell interpreters and metacharacters.
- durable `Repaired`/`Previewed` tool audit transitions with replay coverage,
  and backward-compatible registration of multiple independent capability
  requirements.
- explicit tool provenance origins with trusted/untrusted policy modes,
  approval escalation, fail-closed denial, and durable codec preservation.
- tool proposals now retain material input origins, combine them without trust
  upgrade, and apply the effective origin to approval and trusted-only policy;
  version-1 tool audits remain decodable.
- context projections and prompt rendering can enforce allow-all,
  exclude-external, or trusted-only trust policies while preserving the
  existing privacy and lifecycle filters.
- derived context publication combines requested trust with dependency/source
  origins, and replay rejects persisted context trust upgrades.
- shared kernel/security trust origins now classify runtime, project, user, generated,
  remote, external, web, and MCP inputs; IPC preserves the expanded origin
  classes through its durable codec without upgrading untrusted messages.
- Phase 6 contract/adaptation slice: a bounded plugin registry and manifests
  declare protocol version, capability requirements, resource limits, and
  untrusted responses; process supervision fails closed across crash, timeout,
  and stop states; the command process adapter launches bounded child
  invocations over a versioned frame with an executable capability recheck;
  the Windows host revalidates discovered manifests and executable files,
  then attaches Job Object containment with descendant cleanup,
  active-process limiting, and manifest memory limits; the WASM adapter
  embeds Wasmi for bounded ABI execution with eager validation, fuel,
  bounded linear memory, discovered-module revalidation, and host
  admission/resource checks; the WASM linker exposes manifest-and-lease
  capability checking plus an opt-in bounded resource-read provider; MCP provides bounded progressive discovery and an
  explicit legacy/modern session lifecycle contract plus bounded stdio JSON-RPC
  and Streamable HTTP transports, including JSON/SSE POST responses and
  explicit active-stream server-request handling; process, MCP, and A2A
  adapters validate boundaries without moving transport concerns into the
  kernel; plugin-discovery scans strict bounded manifests without activating
  or authorizing candidates, and the process host explicitly binds discovered
  process candidates only when an entrypoint is present and declared;
  plugin-host performs atomic, allowlisted startup binding for process/WASM
  candidates and explicitly configured MCP HTTP candidates without granting
  capabilities or launching processes; configured MCP binding requires an
  endpoint and agent-scoped network context.

Not yet implemented:
- crash fault-injection and directory-durability coverage beyond the tested
  torn-tail and transactional recovery paths;
- artifact retention, garbage collection, external blob migration, and
  cryptographic hashing policy;
- provider-specific cache metadata adapters, automatic expiry selection, context
  automatic archival/retention policy, and broader runtime-service orchestration;
- provider-specific policy triggers, richer profile-driven specialist selection,
  broader promotion/demotion policy, and deeper multi-agent supervision;
- broader platform enforcement, full cross-domain trust propagation beyond
  context, IPC, artifacts, tools, assumptions, and manager projections;
  stronger OS filesystem/network sandboxing, unrestricted/implicit plugin
  activation, and broader HTTP bidirectional session behavior,
  full WASI/broader effectful host-import adapters and
  preemptive WASM wall-time interruption,
  natural-language terminal assistant; the Phase 8 text inspector and pure
  semantic breakpoint scanner are now implemented, while interactive
  TUI/debugger controls remain.

Evidence:
- foundation types, provider streaming, usage, cancellation, configuration, and agent-loop tests pass;
- forty-seven event-store tests plus five agent tests pass for in-memory, filesystem, and SQLite sequencing, reconstruction, replay, snapshots, incremental reads, artifact deduplication and provenance, reopen, interrupted multi-frame batch recovery, torn-tail repair, metadata corruption rejection, concurrent same-hash artifact writes, orphan-temp recovery, duplicate IDs, invalid ordering, terminal-agent transition rejection, branch validation/persistence, snapshot codec/load validation, fork materialization, provider-backed continuation, atomic batch append, filesystem lock release, concurrent SQLite writers, migration idempotence, opaque context-event round trips, cached-usage event/snapshot preservation, durable cache-observation, failure-memory, and specialist-profile event round trips, typed IPC and assumption persistence, fork remapping, and unknown runs;
- sixteen context tests pass for content deduplication, private-scope projection, dependency invalidation, targeted subscriptions, projection limits, stable prompt-prefix hashing, context trust-policy enforcement, derived trust propagation, replay rejection of persisted trust upgrades, context-transition replay, versioned event encoding, malformed-payload rejection, artifact-backed hash verification, bounded freshness proprioception, explicit archive/restore lifecycle control, replayable pin/unpin retention metadata, and bounded search/dependency inspection;
- thirty-two runtime-service tests pass for graph hydration from in-memory events,
  filesystem reopen, SQLite reopen, unknown runs, malformed context events,
  artifact-backed content recovery, durable cache-observation recovery, bounded
  IPC delivery, SQLite IPC recovery, SQLite assumption recovery, durable
  budget/health projections, conflict notifications, atomic notification
  backpressure, ownership, manager projections, model selection, supervision
  pause/resume, cancellation, child spawning, specialist profile recovery, policy-driven
  promotion, durable tool transaction recovery, and failure
  memory recovery/resolution, and context archive/restore recovery;
- three failure-memory tests pass for bounded transition replay, exact attempt
  queries, durable resolution, and malformed/oversized payload rejection;
- twelve scheduler tests pass for budget-overrun rejection, deterministic health
  thresholds, ownership/child relationships, durable budget transfer and its
  usage-safe rejection, evidence-based cache-aware ranking, deterministic
  supervision policy, cache-evidence freshness, and transition codec rejection;
- three security tests pass for scoped capability subtrees, task/expiry
  enforcement, and opaque secret handles; eleven tool-runtime tests pass for capability/risk/approval,
  transactional verification/compensation, effect-boundary capability
  rechecks, versioned audit transition replay,
  deterministic repair, validated previews, provenance policy, and codec
  preservation; thirteen terminal-tool tests pass for rooted filesystem effects,
  traversal rejection, compensation conflict handling, injected process policy,
  bounded environment discovery, and terminal-plan risk/confirmation/path/Git
  validation; fifteen CLI tests pass for bounded runtime configuration, typed
  plan/execute parsing, confirmation gating, verified rooted filesystem
  execution, conservative local phrase translation, and cross-process undo;
- four cache tests pass for explicit metadata recording, absent metadata,
  inconsistent provider values, and aggregation by provider/model/prefix;
- six IPC tests pass for envelope codecs, schema rejection, bounded FIFO
  delivery, validation before enqueue, expanded provenance preservation, and
  legacy material-origin decoding;
- six assumption tests pass for normalized conflict detection, equal-value
  handling, trust-origin combination, persisted trust-upgrade rejection,
  legacy decoding, transition replay, and malformed/versioned payload rejection;
- four plugin-api tests, five process-plugin library tests, fourteen MCP
  session/adapter/stdio/HTTP tests, four plugin-discovery tests, ten WASM
  adapter tests, four plugin-host tests, and two A2A adapter tests were
  covered by prior focused validation. In the current host, plugin-discovery
  and MCP stdio test binaries are ENVIRONMENT-BLOCKED by Windows Application
  Control (OS error 4551); successful focused suites cover bounded contracts,
  protocol validation, progressive and manifest discovery, resource
  admission, untrusted output provenance, host-owned tool policy, remote IPC
  mapping, and real command-host launch/crash/timeout behavior;
- six specialist tests pass for bounded profile codecs, duplicate/invalid
  profile rejection, and replay; eight TUI tests pass for projection rendering, model-change, failure-memory, and terminal safety
  breakpoint scanning, and bounded pane/item navigation;
- cargo fmt, cargo check, and cargo clippy pass. The exact all-features
  workspace test suite is ENVIRONMENT-BLOCKED when Windows Application Control
  launches generated test binaries; it is not reported as a full-suite pass.
- `cargo build --release --workspace` was previously blocked by Windows
  Application Control with OS error 4551 during an earlier Phase A attempt;
  the final Phase B release validation now passes.
- the supplied research brief is preserved in BLUEPRINT.md;
- canonical docs and phase plans are present.

Next gate:
- add broader platform enforcement and full cross-domain trust propagation
  beyond context, IPC, artifacts, and tool inputs; deeper
  model policy, provider-specific cache adapters, scheduling,
  child-agent policy, fault-injection, directory durability, context archival,
  and artifact retention remain later lifecycle work.

Audit gate:
- Phase A and Phase B remediation are complete for the scoped findings.
- Phase B implementation gates pass locally; the all-features workspace test
  command remains host-policy dependent as recorded below.
- Phase C remediation is complete: 7/7 scoped findings pass the Phase C gate.
- `cargo fmt --all -- --check`, workspace check, Clippy with warnings denied,
  and release workspace build pass. The exact all-features workspace test
  command is ENVIRONMENT-BLOCKED when Windows Application Control launches
  generated test binaries (OS error 4551); focused executable suites that
  launch successfully pass.
- Phase D remediation is complete for ORY-AUDIT-026 through ORY-AUDIT-032.
- Phase E benchmark/hardening is complete with measured evidence and explicit
  limitations in `docs/BENCHMARK_RESULTS.md` and `docs/HARDENING.md`.

Phase D includes a typed provider contract with capability negotiation,
recoverable explicit filesystem metadata generations, safe sidecar naming,
truthful fork schema tags, shared frame limits, and bounded inline
artifact/provider-output retention. Phase E measured the local deterministic
startup/RSS/throughput paths and exercised bounded fault and malformed-input
smoke. Phase F adds the full-screen projection-backed TUI, deterministic
offline runtime demo, SQLite run selection and event paging, terminal cleanup,
and release working-set measurements. Real network providers, full
cargo-fuzz, and hosted cross-platform execution remain unmeasured.
The final adversarial pass found and fixed NEW-AUDIT-D-001, an opaque fork
fallback that could have retained a legacy payload under a current tag.

## Phase F: full-screen TUI/runtime debugger - complete

`orynth tui --demo` now launches the deterministic offline control-room demo;
`orynth tui --db <path> [--run <id>]` inspects persisted SQLite runs. Views
cover dashboard/agent tree, events and bounded older-page navigation, context,
IPC, tools, permissions/ownership/budgets, assumptions/conflicts, persisted
runs, and help. The client uses `RecoveredRun` and visibility-scoped context
projection data, has explicit empty/error/small-terminal states, and restores
terminal state through an RAII guard. It exposes no live mutation controls.

Focused and workspace all-features tests, formatting, workspace check,
warnings-denied Clippy, and release build passed. Release working-set samples
were 6,920 KiB empty, 6,944 KiB for the four-agent demo, and 7,064 KiB for
the ten-agent demo. Details, commands, and limitations are in
`docs/TUI_IMPLEMENTATION_REPORT.md`.

The next planned activity is Deep Audit #2; it has not started in Phase F.

## Phase E: benchmark and hardening - complete

The reproducible `orynth-bench` harness now records release startup, bounded
logical-agent working-set scenarios, context, event-store append, SQLite
growth samples, reconstruction, snapshots, IPC, assumptions, tools, stress,
fault smoke, and malformed-input smoke. The measured assumption hotspot was
optimized with a subject index; its 10k conflict-heavy median fell 26.4%.
Exact measurements and limitations are in `docs/BENCHMARK_RESULTS.md`; fault,
fuzz, stress, security, and cross-platform status is in `docs/HARDENING.md`.

Phase E does not claim TUI idle RSS, real provider/plugin process memory,
Linux/macOS execution, full cargo-fuzz campaigns, or sustained multi-hour
stress. Windows Application Control OS error 4551 blocked the freshly rebuilt
benchmark executable and some generated test binaries; those cases remain
ENVIRONMENT-BLOCKED. The next phase is the full TUI/runtime debugger. No TUI
implementation was started in Phase E.
