# ADR-0064: Phase D provider and filesystem hardening

Status: Accepted

Date: 2026-09-25

## Context

Phase D found two groups of boundary defects: the provider contract collapsed
model interaction into text chunks, and the filesystem backend treated
metadata replacement, sidecar naming, legacy fork payloads, and durable frame
limits as separate implementation details. The runtime must retain authority
over identity, budgets, events, and tool execution while adapters expose richer
provider semantics without requiring real network providers.

## Decisions

1. Providers expose bounded request parts, tool definitions/results, structured
   output, reasoning options, extension metadata, capability discovery, typed
   stream events, usage/cost/cache metadata, finish reasons, cancellation, and
   classified errors. Streams remain pull-based for backpressure; the runtime
   records authoritative lifecycle events and does not delegate state to the
   provider.
2. Agent executions retain bounded text deltas rather than requiring a single
   response `String`. Deterministic mock providers exercise text, tools,
   malformed streams, usage, cancellation, errors, finish reasons, empty
   responses, and capability mismatches.
3. Filesystem metadata uses an explicit sibling sidecar name. The established
   lock-file identity remains unchanged so old and new processes cannot bypass
   one another's advisory lock. Legacy sidecars migrate only when the legacy
   path is distinct from the event file.
4. Metadata replacement writes and syncs a same-directory temporary file,
   moves the previous generation to a recoverable backup, installs the new
   generation, syncs the parent where supported, and removes the backup only
   after the new generation is present. Open recovers a valid backup or
   legacy sidecar deterministically.
5. Fork remapping re-encodes decoded legacy IPC, assumption, and tool payloads
   using the current schema constants and records those constants as the
   persisted tags. If decoding fails, the opaque payload and its original tag
   are preserved. Parent events remain unchanged.
6. Event and metadata frame writers share the reader's maximum frame limit.
   Inline artifact payloads, retained provider output, output chunk count, and
   tool-call count also have explicit limits; external blob storage and
   empirical performance targets remain future work.

## Consequences

The provider crate remains offline and provider-neutral; real adapters are not
part of Phase D. A provider can reject unsupported request capabilities before
stream creation. Filesystem recovery can preserve an old or new complete
metadata generation, but directory-entry durability and arbitrary power-loss
semantics remain platform-dependent and are not claimed as universal.
