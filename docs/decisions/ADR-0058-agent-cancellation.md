# ADR-0058: Durable Agent Cancellation

Status: accepted.

## Context

Cancellation must stop an agent's logical work without deleting its identity,
history, projections, or evidence. Repeated terminal operations must not create
ambiguous state transitions.

## Decision

`RuntimeService::cancel_agent` validates run membership and rejects completed,
failed, or already-cancelled agents before appending the existing durable
`ModelCancelled` event. Event-store recovery remains authoritative for the
terminal `Cancelled` status and preserves the original `AgentId` and identity.

## Consequences

- cancellation is observable, replayable, and durable across backends;
- no provider call or transcript deletion is implied by the runtime boundary;
- resumption or further model selection requires a distinct future policy and
  cannot silently override the terminal state.
