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
- `orynth-tui`: projection-backed, read-only full-screen runtime inspection over `RecoveredRun`; it owns only view state, selection, filtering, and bounded event paging, and does not invoke providers or mutate state.
- `orynth`: operator command boundary for recovering selected SQLite runs or constructing the deterministic offline demo and passing snapshots to the TUI.
- Later: policy-driven specialist selection, context-wide trust propagation, broader platform enforcement, interactive replay/fork/comparison controls, semantic breakpoints, and terminal services.
- Apps compose contracts; adapters cannot replace kernel authority.

A run contains tasks, and tasks may be assigned to logical agents. An agent owns identity, mission, scope, model assignment, context references, subscriptions, assumptions, artifacts, permissions, budget, health, progress, and history references. Model changes preserve `AgentId`.

Important transitions are immutable events. The current Phase 2 slice assigns monotonic sequences, reconstructs run/task/agent/artifact projections, pins snapshots to a sequence, provides filesystem plus SQLite event/blob adapters, materializes isolated child runs from validated prefixes, and exposes provider-backed continuation through the agent session. A backend-neutral continuation helper validates the full child trace and commits only the continuation with an atomic batch append. The filesystem adapter uses a single-open OS advisory lock and repairs only incomplete final frames; SQLite uses full synchronous `IMMEDIATE` transactions and bounded busy waiting for concurrent writers. Both reject complete checksum corruption and expose validated branch metadata and versioned snapshots. Runtime model selection preserves logical `AgentId`; fault-injection and directory-entry durability remain ahead.

Deterministic behavior includes validation, policy, budgets, cancellation, conflict detection, and health thresholds. Models are untrusted proposal sources.

Phase 1 implements IDs/lifecycle values, an execution trace, provider streaming, usage, cancellation, a mock provider, a basic loop, and configuration. Phase 2 adds in-memory reconstruction, recorded replay, filesystem durability, the initial SQLite backend, branches, and artifacts. Phase 3 now adds an in-memory typed context graph with privacy-filtered projections, deterministic dependency invalidation, subscriptions, prompt prefix hashing, a versioned opaque kernel-event envelope that both durable stores preserve, runtime-service recovery that hydrates the graph from stored events, artifact-backed large context content with hash verification, and evidence-based cache telemetry with durable observations; exact-prefix cache-aware ranking and caller-supplied observation freshness are now implemented, while provider-specific adapters, automatic expiry selection, and broader automatic routing remain ahead. Phase 4A adds typed internal IPC with bounded mailboxes, durable `AgentMessage` events, recovery, and fork-safe run-scope remapping. Phase 4B adds a normalized assumption graph, deterministic conflict transitions, durable recovery, and fork-safe assumption run-scope remapping.
Phase 5 now adds scoped capability leases, typed transactional tool contracts, durable proposal/state/preflight audit transitions, syntax-safe deterministic repair, bounded injected impact previews, multiple independent capability checks, controlled rooted filesystem/injected-process fixtures, shared security trust origins, expanded durable IPC provenance, tool-level provenance policy, context-derived trust closure, assumption provenance, and effect-boundary capability rechecks. Full cross-domain propagation and platform-level isolation remain explicit follow-up boundaries.
Phase 9 preparation adds bounded event-sourced specialist profiles. A profile records role, scoped resource patterns, subscriptions, descriptive capability requirements, and promotability without becoming a prompt or granting authority. The runtime commits the profile with the child identity and parent/child budget relationship, and recovery exposes it through manager projections.
