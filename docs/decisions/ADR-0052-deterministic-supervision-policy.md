# ADR-0052: Deterministic supervision policy boundary

Status: accepted.

## Context

Health projections already count consecutive failures and distinguish degraded
from blocked agents. Explicit model selection also preserves logical identity,
but leaving every escalation decision to a manager model would spend reasoning
tokens on an observable runtime condition and could accidentally override a user
pin.

## Decision

`orynth-scheduler` exposes a pure `choose_supervision_action` function and
`SupervisionPolicy`. The policy may specify a non-zero consecutive-failure
threshold and whether blocked agents should pause. The runtime supplies the
current health, current model, specialist promotability, user-pin state, and an
explicit candidate list. Only candidates with a strictly stronger model class
are eligible; ties are resolved by provider, model, and class. The runtime
persists a promotion as the existing `ModelRequested` event or persists a pause
as `AgentPaused`.

The policy has no provider or clock access, never infers quality or cache hits,
and never promotes a user-pinned agent. Specialist profiles opt into promotion;
their descriptive capability fields do not authorize tools.

## Alternatives

- Let a manager model decide every escalation: rejected for avoidable token
  cost and nondeterministic detection of threshold conditions.
- Infer a stronger model from provider names or model strings: rejected because
  names are not a reliable capability contract.
- Persist a new promotion event: deferred because the existing durable model
  selection transition already represents identity-preserving model changes.

## Consequences

Automatic escalation is now observable, replayable, and user-overridable at the
policy boundary. Provider discovery, cost/capability negotiation, durable model
pin configuration, and broader supervision reactions remain separate future
slices.
