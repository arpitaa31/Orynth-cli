# Phase 04: Multi-Agent

Status: active.

Phase 4A implements the first vertical slice: stable agent IDs remain distinct
from models; `orynth-ipc` provides typed envelopes with narrow message variants,
causal references, provenance, and bounded FIFO mailboxes; `RuntimeService`
validates run/agent membership, persists messages as versioned events, recovers
message history, and remaps embedded run scope during forks. Envelope validity
is not authorization, and local bounded delivery is not a claim of process
isolation or network transport. Mailbox contents and receive acknowledgements
remain transient until a later delivery-state slice.

Acceptance:
- typed envelope encode/decode rejects unknown versions, malformed bytes, and
  unbounded fields;
- mailbox capacity and FIFO ordering are deterministic;
- filesystem and SQLite event stores preserve agent messages;
- runtime recovery returns typed messages and fork materialization remaps their
  run scope;
- core tests remain offline and deterministic.

Phase 4B adds first-class assumptions with deterministic normalization,
conflict state transitions, affected-owner reporting, bounded versioned codecs,
filesystem/SQLite persistence, runtime recovery, and fork-safe run remapping.
Assumption claims also retain bounded material-input trust origins; publication
combines them without upgrading trust, replay rejects persisted upgrades, and
version-1 payloads remain readable. It intentionally does not auto-pause
agents.

The coordination projection slice adds deterministic per-agent budgets for
tokens, money, wall-clock time, tool calls, child agents, and context, plus
replayable health signals with stable healthy/degraded/blocked thresholds.
Budget overruns are rejected before durable append; both projections recover
from the event stream and survive SQLite reopen.

The coordination slice also emits runtime-originated typed `Conflict` IPC
notifications to every affected owner. Mailbox capacity for every recipient is
checked before the assumption transitions and notifications are appended as one
atomic batch; a full recipient leaves the graph unchanged.

It also persists deterministic resource ownership claims and releases. A
different agent cannot claim an owned resource, and a non-owner cannot release
it; runtime membership is checked before either transition is appended.

Blocked health now has a deterministic supervision action: runtime appends an
`AgentPaused` event once, exposes the paused state through recovery and manager
projections, and allows an explicit `AgentResumed` transition.

Child-agent spawning is atomic across the child `AgentCreated` event, the
parent child-budget usage transition, and the parent/child relationship
transition. An over-budget spawn leaves no partial child or relationship.

Dynamic specialist creation now extends that atomic batch with a bounded
`orynth-specialist` profile. The profile records a role, scoped resource
patterns, context subscriptions, descriptive capability requirements, and a
promotability flag. It is versioned as an opaque kernel event, survives both
event-store backends, is recovered into the runtime manager projection, and
does not grant capabilities or replace security leases.

Next: add profile-driven model/child policy, deeper supervision, and
tool/security boundary integration.
