# Security

Status: canonical specification derived from the supplied research blueprint.

Model output, generated code, web content, MCP metadata/results, plugin output, remote messages, and artifacts are untrusted. Schema validity is not permission.

The runtime, not the model, grants capabilities. Long-term capability domains are filesystem, process, network, secrets, plugins, and external services. Capabilities are scoped; a generic shell boolean is not the target model. Temporary leases expire and attach to agent/task/policy provenance. Path traversal, silent scope broadening, and unnecessary secret logging are prohibited.

Tool execution will be parse -> normalize -> validate -> deterministic repair -> policy -> impact -> preview -> approval -> execute -> verify -> commit/event, with compensation only where valid. Effects are reversible, compensatable, or irreversible.

`orynth-security` now provides deterministic, event-replayable capability leases scoped by agent,
optional task, domain, resource subtree, and expiry. Authorization is explicit
and denies missing or expired leases. `orynth-tool-runtime` consumes that
policy and rechecks every registered capability immediately before execution or
compensation; it does not perform external effects by itself. An executor
adapter must be supplied by an adapter. The current terminal-tools fixtures enforce rooted
filesystem paths and typed process policy before invoking injected effects, but
they are controlled adapters rather than OS-level sandboxing. `SecretVault`
provides opaque, agent/task-bound in-memory secret handles: issuing and
resolving a handle rechecks the current Secrets lease, and secret bytes are
never part of capability transitions or handle debug output.

The kernel owns the policy-level `TrustOrigin` classification, re-exported by
the security crate. Derived
values combine origins by retaining the least-trusted input; tools, context,
and IPC use this classification while preserving their source-specific labels.

The controlled fixtures perform only explicitly requested filesystem effects or
injected process calls after tool-runtime validation. They are not a sandbox;
full cross-domain trust propagation, stronger filesystem/network isolation, and
platform-specific enforcement remain later work. Tool proposals and IPC messages retain explicit
trust origins and can be denied, filtered, or escalated to approval by policy.
