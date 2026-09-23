# ADR-0017: Shared trust origins and artifact provenance

Status: accepted.

## Context

Tools, context blocks, IPC messages, and artifacts all need provenance-aware
policy. Separate domain enums make it easy for a conversion to accidentally
upgrade an untrusted value, while putting the shared type in a higher-level
adapter would create dependency cycles around event persistence.

## Decision

`orynth-kernel` owns the policy-level `TrustOrigin` classification. Security
domains retain source-specific labels—for example a URL or MCP server name—
and map them to this shared class. Combining origins keeps the least-trusted
input. Runtime, trusted-project, and user-provided origins are trusted for
policy purposes; generated, remote, external, web, and MCP origins are not.

IPC preserves the expanded source classes and bounded material-input origins in
its versioned envelope; runtime-generated conflict notifications retain the
origins of the claims they report. Artifact
references and `ArtifactCreated` events carry the shared origin. In-memory,
filesystem, and SQLite stores combine provenance for same-content writes and
persist it through blob records, SQLite schema version 2, event reconstruction,
and snapshot version 3. Legacy event/blob/snapshot records default to generated
provenance rather than being treated as trusted.

Context externalization passes the context block origin into the artifact
store. This prevents storage from silently changing the trust classification
of derived context content. Tool proposals likewise carry bounded material
input origins and combine them with direct proposal provenance before policy.

## Alternatives

- Keep independent trust enums in every crate: rejected because mappings can
  silently upgrade provenance and drift over time.
- Put trust metadata only on tools: rejected because context, IPC, and artifacts
  are also policy inputs.
- Treat legacy records as trusted: rejected because missing provenance must not
  widen authority.

## Consequences

The dependency direction remains kernel -> domain contracts -> adapters, and
trust classification is durable and replayable. This is still not full
information-flow security: all derivation paths must explicitly combine origins,
and platform isolation, secret handles, and cross-domain policy orchestration
remain future work.
