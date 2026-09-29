# ADR-0071: Reusable agent model turns

## Status

Accepted for Phase G.

## Decision

`ModelCompleted`, `ModelFinishedWithoutUsage`, `ModelFailed`, and
`ModelCancelled` retain their existing terminal logical-agent meaning. This
preserves replay of earlier completed agent sessions.

A long-lived Coordinator records each provider request with `ModelRequested`,
then `ModelTurnCompleted` with optional reported usage, `ModelTurnFailed`, or
`ModelTurnCancelled`. These events are persisted in the normal event store and
leave the logical agent running so a later user turn or tool-result continuation
can reuse its identity. Reported usage accumulates in the agent projection;
absent usage remains absent from the turn event.

The new event tags are additive in the filesystem and SQLite codecs and are
valid only while the agent is running. The Workspace presents turn outcomes
without claiming that the logical agent finished.
The bounded site worker also records each finished provider request as a turn.
That preserves reported usage from an earlier request if a later tool step or
model continuation fails. Its final terminal model event retains the aggregate
usage when reported, so the agent projection does not double count it.
The worker's IPC handoff reports only filenames whose tool transactions were
verified. Its free-form final model text is required to finish the turn but is
not promoted into an authoritative claim about completed work.

Closing the live Workspace cancels an in-flight provider token and records a
terminal run outcome synchronously. Completed and failed idle sessions also
receive a terminal run event. A partial request therefore remains visible in
the event log without being mistaken for a completed response after restart.
Shutdown and the worker's verified file transaction share a short-lived lock.
If the transaction starts first, shutdown waits for its event records; if
shutdown starts first, cancellation prevents the transaction from writing.
When replay applies a terminal run event, any agent still Created, Running, or
Paused inherits that terminal outcome. Agents already Completed, Cancelled, or
Failed retain their own outcome. This makes a closed Coordinator appear closed
in reconstructed state while preserving a worker's distinct result.

## Rationale

A provider request and a logical agent have different lifetimes. Marking the
Coordinator completed after its first reply prevents further conversation and
the second provider request needed after worker delegation.
