# ADR-0027: Windows Process Containment for Command Plugins

Status: accepted

## Context

The command host already bounded protocol payloads and wall time, but a timed
out child could otherwise leave descendants behind and the manifest memory
limit would remain advisory. Windows provides Job Objects as a native process
containment primitive without requiring a heavyweight runtime dependency.

## Decision

When containment is required, `CommandProcessInvoker` creates a Windows Job
Object before invoking the child, applies kill-on-job-close, an active-process
limit of one, and the manifest memory limit, then assigns the child to the
job. Closing the containment handle after reap terminates remaining descendants.
If setup or assignment fails, the child is killed and invocation fails closed.

The portable API does not claim that a Job Object is a complete security
sandbox: filesystem, network, token, and other access restrictions require
additional platform policy. Non-Windows implementations must provide their own
containment adapter or explicitly opt into the trusted-host uncontained mode.

## Consequences

- Timeout and host shutdown cleanup include descendants on Windows.
- Resource limits have an OS-enforced memory boundary where the platform
  supports it.
- The containment handle is deliberately internal to the process host; kernel
  contracts remain platform-independent.
