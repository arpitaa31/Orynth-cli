# ADR-0051: Durable specialist profile lifecycle

Status: accepted.

## Context

The runtime already preserves logical child-agent identity and parent/child
budget relationships, but a dynamically created specialist needs more than a
name and mission. The brief calls for lightweight role, scope, subscriptions,
and capability requirements without turning identities into persona prompts or
allowing model output to grant authority.

## Decision

Add `orynth-specialist` as a bounded domain crate. A
`SpecialistProfile` is attached to an existing `AgentId` and records role,
resource-scope patterns, context subscriptions, descriptive capability
requirements, and whether the identity is promotable. The profile is encoded as
a versioned opaque `SpecialistTransition` event. `RuntimeService::spawn_specialist`
commits the child identity, parent budget charge, parent/child relationship, and
profile in one event-store batch. Recovery exposes profiles through the
`RecoveredRun` registry and manager projections.

The profile's capability strings are requirements and observability metadata;
they are not leases and cannot authorize a tool effect. Capability grants remain
owned by `orynth-security`.

## Alternatives

- Put all profile fields into `AgentIdentity`: rejected because it would make
  the kernel own evolving orchestration metadata and expand every identity
  codec.
- Store profiles only in manager memory: rejected because restart and replay
  would lose specialist identity context.
- Let profile capability fields authorize tools: rejected because model- or
  manager-supplied metadata must not bypass security policy.

## Consequences

Specialist creation is replayable, bounded, and atomic across identity,
budget, relationship, and profile state. The profile registry is intentionally
small; profile-driven model selection, capability lease issuance, supervision,
and automatic consultation remain separate policy slices.
