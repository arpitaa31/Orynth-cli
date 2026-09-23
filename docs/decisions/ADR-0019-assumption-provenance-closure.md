# ADR-0019: Assumption Provenance Closure

Status: accepted

## Context

Assumptions are runtime-owned claims, but their claim text and evidence may be
derived from untrusted web, MCP, remote-agent, or external inputs. Without
persisted provenance, a later conflict or policy decision cannot distinguish a
generated claim from one materially based on an untrusted source.

## Decision

`Assumption` carries an effective trust origin and a bounded list of
material-input origins. Publication combines the requested origin with every
input origin using `TrustOrigin::combine` and persists the effective result.
Replay rejects a created assumption whose stored origin is more trusted than
one of its recorded inputs. Version-1 payloads remain readable and default to
generated provenance with no material-input list.

## Consequences

- Deterministic conflict detection retains the provenance of both claims.
- Missing legacy metadata remains conservative rather than widening authority.
- Runtime-originated conflict notifications remain distinguishable from the
  claims that caused them while retaining both claim origins as IPC
  material-input metadata. Manager projections expose the same effective
  origins beside owned assumption IDs.
