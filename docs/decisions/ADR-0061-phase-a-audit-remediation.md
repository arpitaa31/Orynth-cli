# ADR-0061: Phase A audit remediation boundaries

Status: accepted

## Context

The deep audit identified five S1 defects in shared authorization, terminal
compensation, persistent identity, filesystem event recovery, and MCP HTTP
transport. The old implementations checked lexical strings, reused
process-local counters after restart, exposed independent filesystem frames,
and allowed an HTTP transport to outlive the capability context that created
it.

## Decision

- Resource subtree matching is normalized once before comparison. Filesystem
  effect adapters additionally resolve the parent and final object, reject
  symlink/reparse components, and authorize all concrete source and
  destination inputs. A lease for `.` explicitly means the complete rooted
  relative fixture namespace; it is not a wildcard for other capability
  domains.
- Quarantine creates a unique durable object with exclusive hard-link
  allocation and removes the original only after the recoverable object
  exists. The CLI journal records separate durable transaction and operation
  identifiers, original/quarantine paths, and supports legacy v1 records.
- Durable u64 IDs retain wire compatibility but use a cryptographically random
  process prefix and an atomic per-process counter. Context block IDs use the
  same allocator; subscription IDs remain process-local because they are not
  persisted identities.
- New filesystem event files use version 2 begin/event/commit frames. Recovery
  publishes only committed batches, discards an incomplete or corrupted final
  batch, and continues to support version 1 single-event files as legacy
  input. The writer applies the same frame and batch bounds enforced by the
  reader.
- MCP HTTP endpoints are canonicalized into scheme/host/port/path/query
  components. Endpoint capabilities compare those components, redirects are
  disabled, and every POST, GET, server response, and reconnect rechecks the
  bound capability snapshot.

## Consequences

The effect boundary no longer relies on an earlier lexical preflight for the
tested symlink/junction/reparse and URL normalization cases. A filesystem
validation and its subsequent OS operation are still separate calls; a hostile
actor can race a checked path on platforms without handle-relative no-follow
APIs in this slice. MCP transports bind a cloned capability policy because the
existing returned adapter API cannot borrow a mutable policy; later policy
revocation is therefore not observed by an already-created transport.

Directory-entry fsync and atomic coupling between a terminal effect and its
undo journal remain outside this phase. Compensation conflicts remain durable
by retaining the journal record for inspection/retry; the implementation does
not claim universal rollback after an effect or journal failure.
