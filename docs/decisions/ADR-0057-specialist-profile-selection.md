# ADR-0057: Deterministic Specialist Profile Selection

Status: accepted.

## Context

Dynamic specialist creation persists descriptive role, scope, subscription,
capability-requirement, and promotability metadata. A manager needs a bounded
way to find an appropriate existing specialist without treating profile text as
authority or silently selecting a provider/model.

## Decision

`SpecialistRegistry::select` accepts a role, required scope entries, required
capability descriptions, and an optional promotability constraint. It validates
bounded inputs and returns the first matching profile in deterministic
`AgentId` order. Runtime selection rebuilds the registry from authoritative
events and is read-only. Model routing, capability leases, context projection,
and spawning remain explicit separate operations.

## Consequences

- profile-driven discovery is replayable and does not invoke a model/provider;
- descriptive capability fields cannot grant authority by selection;
- future policy can add richer scoring or model-class constraints without
  changing the durable profile event;
- automatic orchestration remains user-overridable.
