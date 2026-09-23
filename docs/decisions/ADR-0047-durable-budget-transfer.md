# ADR-0047: Durable budget transfer between agents

Status: accepted.

## Context

Multi-agent orchestration sometimes needs to rebalance unused capacity without
changing the logical identity of an agent or rewriting prior usage. A runtime
helper that mutates an in-memory budget would not survive recovery and would
make the scheduler projection diverge from the event log.

## Decision

Represent a transfer as a versioned `SchedulerTransition::BudgetTransferred`
event containing an existing source agent, an existing recipient agent, and
per-dimension configured capacity to move. The scheduler applies the transfer
deterministically during normal append and replay.

Only configured finite capacity is transferable. Unlimited dimensions remain
unlimited. Usage stays with its original agent. The transition is rejected when
the source cannot provide the requested capacity, when the resulting source
limit would be below source usage, or when the resulting recipient limit would
be below recipient usage. Runtime membership checks happen before append.

## Consequences

Budget rebalancing is durable, replayable, and observable through the existing
scheduler projection. Agent identity, usage accounting, and health history do
not move with capacity. Automatic policy selection, transfer authorization
policy, and cross-run transfers remain outside this primitive and require later
orchestration work.
