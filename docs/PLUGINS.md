# Plugins

Status: canonical specification derived from the supplied research blueprint.

Plugin tiers are built-in Rust, trusted native extension, isolated process, MCP adapter, and WASM/WASI. Plugins use kernel contracts for identity, capabilities, events, and policy and cannot bypass them. Dynamic Rust ABI loading is not the primary extension mechanism.

Process plugins provide language independence and are intended to provide
crash isolation. The current `orynth-plugin-api` contract supplies typed
manifests, protocol-version negotiation, explicit capability declarations,
bounded memory/fuel/wall-time/message limits, and untrusted responses.
`orynth-plugin-process` supports injected invokers and a
`CommandProcessInvoker` that launches one bounded child per request over a
versioned binary protocol, closes child stderr, applies a minimal environment
by default, and admits calls only when the host capability policy authorizes
every declared and effect-bound executable capability. Its supervisor tracks
crash/timeout/stop states, fails closed after failure, and restarts only with
a newly supplied adapter. Discovered process candidates can be explicitly
bound to this command host after the host re-reads and validates the manifest,
requires an absolute regular-file entrypoint, and rechecks that executable at
the spawn boundary. On Windows, the command host additionally attaches a Job Object with
kill-on-close, descendant containment, active-process limiting, and the
manifest memory cap. This is process containment and lifecycle control, not a
complete filesystem/network security sandbox. The current WASM adapter provides host admission and checks
executor-reported memory, fuel, wall-time, message, capability, and provenance
constraints. It embeds Wasmi for a small bounded ABI (`memory` plus
`orynth_run(i32, i32) -> i64`) and explicit `capability_check` and
`resource_read` imports, with eager module validation, strict instance/
memory/table limits, fuel metering, bounded input/output, and external output
provenance. `resource_read` is disabled unless the host supplies a provider;
the import rechecks the manifest and current lease and bounds each read to
64 KiB. Full WASI, broader effectful host adapters, and preemptive wall-time
interruption are not provided. Plugin metadata/results are untrusted. Bounded external manifest
discovery is available as a metadata-only candidate scan; it does not
automatically activate a plugin or authorize its command. Stronger OS
sandboxing, unrestricted/implicit activation, implicit MCP endpoint/network
binding, newer bidirectional session
semantics, full WASI/broader effectful host capability
integration, and preemptive WASM interruption remain deferred.

The MCP adapter exposes an explicit lifecycle contract for legacy handshake and
modern sessionless modes plus bounded stdio JSON-RPC and Streamable HTTP
transports. HTTP POST responses may be JSON or bounded SSE; server requests in
an active SSE response require an explicit host handler and are answered with
a policy-bound nested POST. A bounded caller-driven GET SSE stream supports
event IDs and `Last-Event-ID` resumption, with server-advertised retry hints
triggering at most two bounded automatic reconnects. Broader bidirectional
session behavior remains separate follow-up work.
Secret authority is represented separately by opaque, in-memory capability
handles; plugin metadata cannot provide secret bytes or grant Secrets access.
`orynth-plugin-host` provides host-owned startup activation for process and
WASM candidates selected by an explicit ID/kind allowlist, plus a separate
configured MCP activation path requiring an endpoint, client/mode metadata,
and an agent-scoped capability context. Activation is staged atomically, does
not grant capabilities, and routes later calls through the bound adapter
policy checks. Default activation leaves MCP candidates unbound; no manifest
or server output can implicitly grant network authority.
