# ADR-0016: Tool provenance is an explicit policy input

Status: accepted.

## Context

Tool schema validity and capability authorization do not establish whether the
proposal came from a trustworthy source. The blueprint requires generated,
remote, web, and MCP material to remain potentially untrusted, while direct
user/project sources need distinct handling. Provenance must survive replay so
policy decisions are explainable.

## Decision

`orynth-security` owns the shared policy-level `TrustOrigin` classification,
and `orynth-tool-runtime` retains source-specific proposal provenance while
mapping it to that shared type. The explicit origins are:

- generated agent/manager output;
- direct user input;
- trusted project input;
- external, remote-agent, web-untrusted, MCP metadata, and MCP result input.

Each registered tool may use one of three policies: allow all origins, require
approval for untrusted origins, or require a trusted origin and fail closed.
Untrusted approval escalation produces the existing `AwaitingApproval` state;
trusted-only denial returns a typed error before execution. The origin is
encoded in the version-2 durable proposal transition. Proposals also retain a
bounded list of material input origins; policy evaluates the least-trusted
combination of direct and material origins. Version-1 proposal transitions
decode with an empty material-origin list.

This is shared provenance classification plus domain policy, not a complete
information-flow system. Context blocks support local allow-all,
exclude-external, and trusted-only projection/rendering policies, and IPC
preserves expanded source classes. Artifacts and derived inputs still need
consistently propagated trust metadata before the runtime can make whole-data-
flow claims.

## Alternatives

- Treat every external proposal as equivalent to user input: rejected because it
  would erase important trust distinctions.
- Infer trust from schema validity: rejected because valid data can still be
  untrusted.
- Require approval for every proposal: rejected because it removes useful
  deterministic policy and unnecessarily burdens trusted flows.

## Consequences

Policy decisions are deterministic, inspectable, and replayable. Existing
provenance variants remain compatible, while new source classes can be added to
the versioned codecs. Full cross-domain propagation and platform enforcement
remain future boundaries.
