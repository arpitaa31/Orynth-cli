# Tools

Status: canonical specification derived from the supplied research blueprint.

Tools are capability-controlled transactions: proposal, parse, normalize, schema validation, deterministic repair, policy, impact analysis, preview, approval, execute, verify, commit, and event. Compensation is used only when an inverse is real.

Effects are reversible, compensatable, or irreversible. Tool discovery is progressive through namespaces, search, manifests, and task-relevant exposure. Tool output remains untrusted until verified.

The first security/tools slice implements typed `ToolDefinition` and
`ToolProposal` contracts with deterministic normalization, required-field
validation, capability authorization, risk classification, explicit approval,
execution through an injected executor, verification through an injected
verifier, commit, and compensation for genuinely reversible effects.

Tool proposals and state changes are persisted as versioned `ToolTransition`
events and replayed into a runtime `ToolHistory`. The audit record retains
proposal identity, normalized input, provenance, state, and optional state
detail; it does not claim that an external effect occurred.

The current preflight boundary adds syntax-safe deterministic repair with
normalized-key collision rejection and an injected `ToolPlanner` for bounded
impact previews. `orynth-terminal-tools` supplies bounded terminal planning,
a rooted filesystem fixture
with reversible write/move compensation and an injected process fixture; these
adapters reject traversal and shell-like process requests, but are not a
platform sandbox. The shell app exposes a plan-only report path plus an explicit
confirmed execution path for the rooted filesystem subset; it never falls back
to an ambient shell. Rooted copy/move/quarantine effects can persist bounded
relative compensation records for a later conflict-checked `undo` command.

Tool provenance has explicit origins for generated, user, project, remote-agent,
web-untrusted, and MCP metadata/result sources. The kernel/security boundary
owns the policy-level classification, including non-upgrading combination of
multiple inputs. Proposals retain the origins of material context, artifact, or
message inputs in addition to their direct proposer provenance. A registered
tool can allow all origins, require approval for untrusted origins, or fail
closed unless the combined origin is trusted; origins are retained in the
versioned proposal codec, with legacy version-1 audits still readable.

The crate itself performs no filesystem, process, network, or shell operation.
Capability leases are checked during preflight and rechecked at the effect
boundary before execution and compensation. Schema-rich inputs, context-wide
trust propagation, and platform sandboxing remain follow-up boundaries;
the contracts cannot claim those properties yet.
