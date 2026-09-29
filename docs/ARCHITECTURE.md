# Architecture

Status: canonical specification derived from the supplied research blueprint.

The dependency direction is: applications and UI -> runtime services -> domain crates -> kernel primitives. The kernel does not depend on providers, networks, databases, plugins, or UI.

- `orynth-kernel`: IDs, lifecycle values, events, budgets, cancellation, and provider-independent primitives.
- `orynth-provider`: model request/stream/usage/error contracts and deterministic mocks.
- `orynth-agent`: logical-agent execution and model-loop coordination.
- `orynth-event-store`: immutable event persistence, reconstruction, replay, snapshots, branches, and artifacts.
- `orynth-context`: typed context blocks, projections, dependency invalidation, subscriptions, prompt rendering, and bounded freshness proprioception.
- `orynth-ipc`: typed internal-agent envelopes, schema codecs, and bounded FIFO mailboxes.
- `orynth-assumptions`: normalized assumption graph, deterministic conflicts, and replayable transitions.
- `orynth-failure-memory`: bounded attempted-approach records and replayable resolution transitions.
- `orynth-specialist`: bounded role/scope/subscription/capability metadata for dynamic specialist identities.
- `orynth-scheduler`: deterministic budget limits/usage, health projections, and resource ownership.
- `orynth-security`: scoped capability leases and deterministic authorization.
- `orynth-tool-runtime`: typed, capability-gated transactional tool pipeline.
- `orynth-runtime`: runtime-service composition that hydrates ordinary, context, scheduler, failure-memory, and compact manager projections—including bounded context/cache/artifact/failure observability—from one authoritative event stream.
- `orynth-terminal-tools`: bounded terminal planning plus controlled rooted filesystem effects and injected typed process fixtures built on the tool contracts.
- `orynth-cli` and `orynth-shell`: explicit terminal request parsing, host-aware plan rendering, and confirmed rooted-filesystem execution through the tool runtime; they do not execute ambient shell commands.
- `orynth-tui`: chat-first Workspace and Advanced Debugger over the same `RecoveredRun` source; the TUI owns presentation state and forwards live input through an app-supplied data source. Complete Coordinator turns are durable; the Phase G app source streams provider text and uses the runtime for effects.
- `orynth`: operator command boundary for recovering selected SQLite runs, constructing the deterministic offline demo, or running the OpenRouter-backed Coordinator and rooted website worker.
- Later: policy-driven specialist selection, context-wide trust propagation, broader platform enforcement, interactive replay/fork/comparison controls, semantic breakpoints, and terminal services.
- Apps compose contracts; adapters cannot replace kernel authority.

The runtime owns a versioned complete-turn codec for user/Coordinator
conversation. Validated turns are appended to the same run event log as agent
and tool transitions; the Workspace decodes them from recovered or paged
events. The offline input does not submit free-form turns without a provider.
The OpenRouter adapter remains behind `ModelProvider`; model response metadata
is persisted as separate observations without changing logical agent identity.

## Run, agent task, and model turn

Phase G.5 keeps three lifecycle levels distinct. A **run** is the bounded
user-requested job and owns the aggregate provider, tool, wall-clock, and
concurrency safety budgets. An **agent task** is the durable assignment owned
by one `AgentId`; its identity, model assignment, permissions, context,
children, IPC history, and budget survive provider turns. A **model turn** is
one bounded provider request. `stop`, `tool_calls`, `length`, cancellation,
timeout, and provider failure finish a model turn; they do not by themselves
complete or fail the agent task or run.

The live turn state machine is explicit: `READY` -> `REQUESTING` ->
`STREAMING` -> `TURN_FINISHED`, then `TEXT_RESPONSE`, `TOOL_CALLS`,
`OUTPUT_LIMIT`, `CANCELLED`, or `FAILED`. Timeout and provider failures are
turn outcomes handled by bounded retry policy; they are not implicit run
terminal states.

The live Coordinator and rooted website worker preserve their request parts and
runtime results across bounded turns. Tool calls are validated and executed
before the next request, while a length finish continues from the current
runtime state. Empty length turns use a small consecutive retry budget and then
surface a bounded unsuitable-model error without replacing the logical
`AgentId`. Run-level request and scheduler budgets still stop runaway loops.
Completed effects are recorded before continuation, so replay and restart use
the event log rather than reissuing an in-flight provider request.

A run contains tasks, and tasks may be assigned to logical agents. An agent owns identity, mission, scope, model assignment, context references, subscriptions, assumptions, artifacts, permissions, budget, health, progress, and history references. Model changes preserve `AgentId`.

Important transitions are immutable events. The current Phase 2 slice assigns monotonic sequences, reconstructs run/task/agent/artifact projections, pins snapshots to a sequence, provides filesystem plus SQLite event/blob adapters, materializes isolated child runs from validated prefixes, and exposes provider-backed continuation through the agent session. A backend-neutral continuation helper validates the full child trace and commits only the continuation with an atomic batch append. The filesystem adapter uses a single-open OS advisory lock and repairs only incomplete final frames; SQLite uses full synchronous `IMMEDIATE` transactions and bounded busy waiting for concurrent writers. Both reject complete checksum corruption and expose validated branch metadata and versioned snapshots. Runtime model selection preserves logical `AgentId`; fault-injection and directory-entry durability remain ahead.

Deterministic behavior includes validation, policy, budgets, cancellation, conflict detection, and health thresholds. Models are untrusted proposal sources.

Phase 1 implements IDs/lifecycle values, an execution trace, provider streaming, usage, cancellation, a mock provider, a basic loop, and configuration. Phase 2 adds in-memory reconstruction, recorded replay, filesystem durability, the initial SQLite backend, branches, and artifacts. Phase 3 now adds an in-memory typed context graph with privacy-filtered projections, deterministic dependency invalidation, subscriptions, prompt prefix hashing, a versioned opaque kernel-event envelope that both durable stores preserve, runtime-service recovery that hydrates the graph from stored events, artifact-backed large context content with hash verification, and evidence-based cache telemetry with durable observations; exact-prefix cache-aware ranking and caller-supplied observation freshness are now implemented, while provider-specific adapters, automatic expiry selection, and broader automatic routing remain ahead. Phase 4A adds typed internal IPC with bounded mailboxes, durable `AgentMessage` events, recovery, and fork-safe run-scope remapping. Phase 4B adds a normalized assumption graph, deterministic conflict transitions, durable recovery, and fork-safe assumption run-scope remapping.
Phase 5 now adds scoped capability leases, typed transactional tool contracts, durable proposal/state/preflight audit transitions, syntax-safe deterministic repair, bounded injected impact previews, multiple independent capability checks, controlled rooted filesystem/injected-process fixtures, shared security trust origins, expanded durable IPC provenance, tool-level provenance policy, context-derived trust closure, assumption provenance, and effect-boundary capability rechecks. Full cross-domain propagation and platform-level isolation remain explicit follow-up boundaries.
Phase 9 preparation adds bounded event-sourced specialist profiles. A profile records role, scoped resource patterns, subscriptions, descriptive capability requirements, and promotability without becoming a prompt or granting authority. The runtime commits the profile with the child identity and parent/child budget relationship, and recovery exposes it through manager projections.
