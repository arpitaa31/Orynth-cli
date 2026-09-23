# Phase 05: Security and Tools

Status: active, first durable audit slice implemented.

The first slice adds typed tool definitions/proposals and the transactional
pipeline: normalization, required-field validation, capability leases, risk,
explicit approvals, injected execution, verification, commit, compensation,
provenance, and reversible/irreversible classification. Capability checks cover
agent/task scope, resource subtrees, and expiry. The contracts perform no
external side effects by themselves.

Capability grants/revocations are now versioned runtime events and recover
through both event-store backends with agent/task membership checks. Tool
proposals and state changes are also versioned `ToolTransition` events and
replay into a recovered transaction history; the audit path records no external
effect by itself. Deterministic syntax-safe proposal repair rejects normalized
field collisions, and injected planners now validate impact previews. The
The terminal-tools crate provides bounded terminal planning plus a rooted reversible filesystem fixture and an
injected typed process fixture with shell-interpreter/metacharacter rejection.
The security crate also provides opaque in-memory secret handles bound to the
issuing agent/task; issue and resolve operations recheck the current Secrets
lease and never serialize secret bytes.
Tool provenance now distinguishes generated, user, project, remote-agent, web,
and MCP origins; policies can require trusted origins or escalate untrusted
origins to approval, with fail-closed denial and codec coverage.

The shared kernel/security trust taxonomy now feeds tool provenance, material
input origins, context trust levels, expanded IPC source classes, and artifact
metadata. It combines derived origins without upgrading an untrusted input;
context publication now enforces this for dependency/source-derived blocks and
replay rejects persisted trust upgrades. Focused tests cover durable codec
preservation and the context closure invariant.

Next: broaden platform-specific enforcement and propagate trust metadata through
all remaining derived values, with policy decisions connected across domains.
The durable repair/impact/preview audit, multi-capability registration,
tool provenance and material inputs, expanded IPC provenance, artifact trust,
and context-local trust-policy slices are covered by focused replay tests.
