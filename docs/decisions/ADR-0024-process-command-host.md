# ADR-0024: Bounded Command Host for Process Plugins

Status: accepted

## Context

The process-plugin lifecycle contract originally used only injected invokers.
That made the boundary testable, but did not provide a host path for launching
an implementation written in another language. Launching arbitrary commands
without a framing protocol, output bound, timeout, or effect-boundary policy
check would weaken the runtime authority invariant.

## Decision

`orynth-plugin-process` provides `CommandProcessInvoker`. It launches one fresh
child per request with piped stdin/stdout, null stderr, an explicitly
configured environment, and a timeout capped by the plugin manifest. Requests
and responses use a versioned bounded binary frame with request IDs and
explicit status values. The host kills and reaps timed-out children, rejects
non-successful exits, and marks successful output external through the existing
plugin response contract.

The concrete command path is an effect capability. It must be declared by the
manifest and authorized by the host policy immediately before spawn. On
Windows, the adapter attaches a Job Object with kill-on-close, an active
process limit, descendant containment, and the manifest memory limit. This is
process containment rather than a complete filesystem/network security
sandbox; stronger platform sandboxing and automatic discovery-to-transport
activation remain separate work.

## Consequences

- Any implementation language can target the stable frame without entering the
  kernel crate.
- Bounded framing prevents a child from expanding host memory through a length
  field or response payload.
- A child cannot use the adapter to self-authorize its executable or continue
  after a timeout, but descendant-process cleanup and stronger OS isolation
  require platform-specific enforcement later.
