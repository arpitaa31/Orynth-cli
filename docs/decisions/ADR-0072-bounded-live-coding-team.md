# ADR-0072: Bounded live coding team

## Status

Accepted for the alpha release patch.

## Decision

The live Coordinator receives `spawn_agent` on every normal request. A successful call uses `RuntimeService::spawn_agent`, preserving the authoritative `AgentCreated` event and AI TEAM projection. Explicit multi-part coding team requests require at least two successful implementation worker results and a later read-only review result before the Coordinator may finish. Small follow-ups keep tool choice optional.

Live scheduler configuration sets bounded defaults of 100,000 Coordinator tokens, 50,000 worker tokens, and four children. Existing scheduler enforcement remains authoritative. Runtime budget failures are presented separately from provider errors, tool failures, and cancellation.

The complete conversation remains in the event store. Provider requests retain the two current system layers and a bounded recent projection of conversation and tool results. Workers receive their assignment and a bounded context projection. Coding workers write through the capability-gated rooted tool runtime. Review workers have no write capability or provider write tool and inspect bounded files through the same rooted filesystem fixture.

The live boundary treats a completed tool call as an action even if a provider labels the surrounding finish as `stop`. Workers normalize harmless `./` relative file paths, reject escape paths, and get bounded reminders when required writes are missing. A failed worker releases its file ownership so a later assignment can recover without bypassing the scheduler's child limit or rooted tool checks.

## Rationale

The former one-child limit and one-specialist prompt prevented a small coding team from appearing in live runtime state. Unbounded transcript projection made later provider turns progressively larger. The release patch changes those live limits and projections without adding a second orchestration authority.
