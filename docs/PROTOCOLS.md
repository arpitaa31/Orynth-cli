# Protocols

Status: canonical specification derived from the supplied research blueprint.

Internal communication is lightweight typed Orynth IPC. External MCP adapts
tools/resources and treats server metadata/results as untrusted; the current
adapter requires host-supplied capability/risk policy and exposes bounded,
caller-driven pages for tool/resource discovery. External A2A adapts
independent agents into bounded typed IPC with `RemoteAgent` provenance. The
current process-plugin contract similarly validates manifests and bounds
before an injected transport is called. Its command host adapter uses a
versioned bounded binary request/response frame and treats child output as
external; the executable path is rechecked against host process capability at
the spawn boundary. Provider protocols are adapted behind provider contracts
while preserving capabilities. MCP now has an explicit legacy/modern session
lifecycle contract and bounded stdio JSON-RPC plus a bounded Streamable HTTP
transport. HTTP POST responses may be JSON or SSE, and an active SSE response
may dispatch server requests only through an explicit host handler. HTTP
remains an adapter concern rather than a kernel responsibility; bounded
caller-driven GET streams and `Last-Event-ID` resumption are supported, and
bounded SSE retry hints trigger automatic reconnects. Broader bidirectional
session behavior remains deferred.

External plugin discovery uses a strict bounded manifest file and returns
validated candidates only. Discovery does not grant capabilities or launch a
plugin. The process host exposes a separate explicit activation function for
validated discovered process candidates; invocation still rechecks policy.

The WASM adapter uses a bounded Wasmi ABI with linear memory, fuel,
input/output, external output provenance, and explicit `orynth.capability_check`
and opt-in `orynth.resource_read` imports. Both imports require manifest
declaration and current lease authorization; resource reads additionally
require a host-supplied bounded provider. Full WASI and broader effectful host
imports remain explicit follow-up adapters rather than implicit permissions.

Adapters translate to runtime events and policy decisions. Network versioning and trust assumptions do not leak into kernel contracts. Phase 1 includes only the provider-independent stream contract.
