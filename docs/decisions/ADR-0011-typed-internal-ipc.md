# ADR-0011: Typed, bounded internal IPC

Status: accepted

## Context

Logical agents need narrow communication without copying complete transcripts
into one another's context. Communication must remain local and lightweight,
but important messages still need to be observable, replayable, and recoverable
with the run that produced them. An unbounded queue would violate the runtime's
memory discipline, while a network protocol would impose unnecessary overhead
on local agents.

## Decision

Define `orynth-ipc` as a kernel-adjacent domain crate with a versioned
`IpcEnvelope`, typed message variants, explicit provenance, causal event
references, and bounded FIFO mailboxes. `RuntimeService` validates capacity
before appending a message as a versioned opaque `AgentMessage` event, then
enqueues it. Recovery decodes the authoritative event sequence. Fork
materialization remaps the embedded run scope in valid IPC payloads so child
messages cannot retain the parent run identity.

The current mailbox is a transient delivery projection. Message history is
durable, but receive acknowledgements and pending-queue state are not yet
separate events; that boundary is explicit so recovery does not overclaim
exactly-once delivery.

Envelope validity is separate from authorization. Relationship checks,
capabilities, and policy will gate delivery in a later coordination/security
slice.

## Alternatives

- Copy arbitrary chat transcripts: rejected because it duplicates context and
  hides the narrow contract being communicated.
- Use an unbounded channel: rejected because backpressure and memory bounds are
  runtime invariants.
- Put a network protocol in the kernel: rejected because local IPC should stay
  typed and cheap; A2A remains an external adapter.
- Store messages only in a transient queue: rejected because important runtime
  transitions must survive restart and replay.

## Consequences

IPC schemas require explicit versioning and bounded decoding. A full recipient
mailbox rejects new messages before they become durable, so callers receive a
deterministic backpressure error. The current slice does not claim
authorization, cross-process transport, or cross-agent conflict resolution.
