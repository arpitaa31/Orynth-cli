# ADR-0059: Bounded Peer Consultation Helpers

Status: accepted.

## Context

Agents need to ask narrowly scoped peer questions without copying complete
contexts or transcripts. Generic IPC envelopes already support typed questions
and answers, but callers should not need to rebuild the same validation and
durability boundary for ordinary consultation.

## Decision

Expose runtime `request_consultation` and `answer_consultation` helpers. Each
constructs a bounded agent-provenance `Question` or `Answer` envelope and
delegates to `send_message`, preserving run membership checks, mailbox
backpressure, event persistence, recovery, and logical agent identity.

## Consequences

- peer consultation remains typed, durable, and observable;
- only the narrow subject/value/evidence fields cross the IPC boundary;
- authorization beyond run membership and richer correlation policy remain
  future coordination layers.
