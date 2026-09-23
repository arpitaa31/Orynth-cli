# Agents

Status: canonical specification derived from the supplied research blueprint.

A logical agent is a persistent entity with an `AgentId`, mission, scope, task, model assignment, context references, subscriptions, assumptions, artifacts, permissions, budget, health, progress, and history references. It is not a persona prompt or model instance.

Promotion, demotion, provider migration, pause, resume, redirect, fork,
cancellation, and budget transfer preserve identity and emit transitions. The
runtime cancellation operation now persists the cancellation boundary and
rejects repeated cancellation after a terminal state. User model pins override
automatic routing.

Supervision uses compact projections containing role, model, task, state, progress, health, budget, context pressure, failures, blockers, conflicts, owned resources, and recent artifacts. Deterministic signals should trigger before manager-model reasoning. The current policy slice can promote a promotable specialist after an explicit failure threshold or pause a blocked agent, while respecting a caller-supplied user pin. Agents are logical metadata and references, not dedicated OS threads or copied repositories.

Assumptions are runtime claims owned by agents and represented separately from
their model prompts. Deterministic normalized contradictions mark affected
claims conflicted and identify their owners; automatic pause, escalation, and
manager intervention are later policies.

Phase 1 defines one-agent execution and identity. Phase 4A now provides the
first typed internal communication slice with bounded mailboxes and durable
message recovery. Dynamic child creation now has a bounded, durable specialist
profile slice, and recovered profiles can be selected deterministically by
role, scope, capability, and promotability requirements; model routing,
capability grants, and active supervision remain separate policies.
