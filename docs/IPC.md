# IPC

Status: Phase 4A typed internal transport implemented; authorization and
cross-process protocols remain later boundaries.

Internal communication uses `orynth-ipc` typed messages: `Question`, `Answer`,
`Assumption`, `Decision`, `Conflict`, `Blocked`, `Progress`, `Artifact`,
`ReviewRequest`, `Warning`, `Handoff`, `ContractUpdate`, and
`OwnershipRequest`. An `IpcEnvelope` carries sender/recipient IDs, run/task
scope, a message ID, causal event references, provenance, and one typed
payload. Provenance retains runtime, agent-generated, user, project, external,
remote-agent, web, and MCP source classes; envelopes also retain bounded
material-input origins and expose an effective origin that never upgrades an
untrusted input. `trust_origin()` maps direct provenance to the shared security
taxonomy. Envelope schema version 2 is written now; version-1 payloads decode
with no material-origin list. IPC carries narrow claims and references rather
than copied transcripts.

`BoundedMailbox` provides FIFO in-process delivery with explicit capacity and
backpressure. `RuntimeService::send_message` validates the envelope, checks
run and agent membership plus recipient capacity before appending, persists it
as a versioned `AgentMessage` event, and enqueues it. Recovery decodes message
history from the authoritative event sequence; transient mailbox contents and
delivery acknowledgements are not yet event-sourced. Filesystem and SQLite
persistence, malformed payload rejection, and fork run-scope remapping are
tested.

`RUNTIME_AGENT_ID` is the reserved zero-valued sender for runtime-originated
coordination messages. Assumption conflicts use this sender and emit typed
`Conflict` notifications to every affected owner; those notifications retain
the origins of both claims as material inputs. Notification capacity is
checked together with the assumption transitions, so a full recipient mailbox
cannot leave a partially published conflict.

The runtime also exposes explicit `request_consultation` and
`answer_consultation` helpers for narrow peer questions. They are convenience
boundaries over the same validated, durable `Question`/`Answer` envelopes;
membership and mailbox capacity are still enforced by `send_message`, and no
transcript or full context is copied.

Routing authorization is deliberately not implied by schema validity. Agent
relationships, capability policy, and peer permissions will gate delivery in a
later security/coordination slice. A2A remains an external adapter and must
not impose network overhead on local IPC.
