# Testing

Status: canonical specification derived from the supplied research blueprint.

Every vertical slice has deterministic acceptance tests and an explicit
unsupported-feature list. Core tests require no paid credentials.

Phase 1 acceptance covers IDs and display behavior, ordered mock streaming and
usage, lifecycle trace events, cancellation, configuration parsing, formatting,
Clippy, unit tests, and doc tests.

Phase 2 acceptance covers immutable sequencing, duplicate IDs, successful and
failed reconstruction, recorded replay without provider calls, snapshots,
incremental reads, artifact deduplication, filesystem reopen and torn-tail
repair, SQLite reopen and migration, branch validation, fork materialization,
provider-backed continuation, atomic continuation persistence, locking, and
checksum corruption rejection.

Phase 3 acceptance covers typed context blocks, content addressing, revisions,
dependency invalidation, private-scope projection, subscriptions, bounded
rendering, stable-prefix hashing, replayable context events, artifact
externalization, hash-verified recovery, explicit cache metadata, aggregation,
durable cache observations, runtime recovery, bounded freshness proprioception,
explicit archive/restore and pin/unpin lifecycle transitions, and bounded
search/dependency inspection.

Phase 4 acceptance covers typed IPC envelopes, schema rejection, bounded FIFO
backpressure, filesystem/SQLite message persistence, normalized assumptions,
deterministic conflicts, affected-owner reporting, transition replay, fork
remapping, six budget dimensions, health thresholds, ownership, manager
summaries, model selection preserving logical identity, supervision pause/resume,
atomic child spawning, bounded specialist profiles with role/scope/subscription
metadata, durable profile recovery, configured-capacity transfers, deterministic
profile selection and promotion/pause policy, and SQLite reopen. Profile capability fields are
descriptive and do not bypass capability leases.

Phase 9 preparation acceptance covers bounded specialist profile codecs and
atomic spawn/recovery, deterministic promotion/pause policy, bounded failure records, exact
fingerprint queries, durable resolution, malformed/oversized payload rejection,
SQLite recovery, event-derived inspector breakpoints, and deterministic
cache-aware ranking from explicit provider observations, and deterministic
threshold-based promotion/pause policy. It does not claim semantic similarity,
automatic retry suppression, physical cache hits, provider discovery, or
broader automatic routing/supervision.

Phase 5 acceptance covers scoped capability authorization, expiry/task
enforcement, opaque secret handles, versioned capability recovery, typed tool normalization, provenance
policy, risk and approval gates, injected execution, verification, commit, compensation,
versioned tool transition codecs, audit replay, effect-boundary capability
rechecks, deterministic safe repair,
normalized-field collision rejection, bounded impact previews, rooted
filesystem write/move/copy/quarantine-backed-remove effects, traversal
rejection, compensation conflict retry, typed injected process invocation,
shell-policy rejection, bounded
terminal environment discovery, typed plan risk/confirmation classification,
path/Git validation, and explicit irreversible-effect handling. Tool proposals also preserve material input
origins and legacy audit decoding.

Phase 6 contract acceptance covers plugin manifest/version/limit validation,
bounded process-plugin invocation, real command launch, and deterministic
crash/timeout supervision,
WASM host admission with Wasmi ABI execution, discovered-module activation,
fuel/memory admission, invalid-module rejection, manifest/lease-gated bounded
resource reads through an explicit provider, bounded progressive MCP
discovery, legacy/modern session lifecycle validation, bounded stdio JSON-RPC,
bounded HTTP JSON-RPC/SSE responses with active server-request handling,
caller-driven GET SSE streams with `Last-Event-ID` resumption and bounded
automatic retry/resumption, configured
discovered-MCP host activation,
bounded external manifest discovery, untrusted MCP
metadata/results, host-owned MCP tool policy, bounded A2A messages, and
remote-agent IPC translation. It does not yet cover stronger OS process
filesystem/network sandboxing,
broader bidirectional session behavior, and unrestricted/implicit activation,
full WASI/broader effectful host imports, or preemptive WASM wall-time interruption.

Current evidence includes provider-contract and persistence-hardening tests in
addition to the existing 47 event-store tests, 32 runtime tests, 6 specialist tests, 3 failure-memory
tests, 12 scheduler tests, 11 tool-runtime
tests, 12 terminal-tools tests, 14 CLI tests, 16 context tests, 4 cache
tests, 6 IPC tests, 6 assumption tests, 3 security tests, 4 plugin-api tests,
5 process-plugin library tests plus 4 command-host integration tests, 14 MCP
session/adapter/stdio/HTTP tests, 4 plugin-discovery tests, 10 WASM adapter tests, 4 plugin-host
tests, 2 A2A adapter tests, 8 TUI tests, 8 operator-app tests, and the existing
foundation/agent/provider suites.
Workspace formatting, check, Clippy, release build, and the all-features
workspace test suite are acceptance gates in CI. Local validation may be
`ENVIRONMENT-BLOCKED` when Windows Application Control prevents a generated
test binary from launching; such runs are not reported as passes.

Phase E adds the release benchmark/fault commands documented in
`docs/BENCHMARK_RESULTS.md` and `docs/HARDENING.md`. The local bounded fuzz
smoke passed with 30,000 malformed decoder cases and zero observed panics;
full `cargo fuzz` is unavailable on this host. Benchmark executable launches
that Windows Application Control blocks are recorded as environment limits.

The shared kernel/security trust taxonomy, expanded IPC provenance, context
trust closure, and assumption-origin persistence are now covered. Remaining
Phase 5 work is full cross-domain trust propagation and platform enforcement.
Later phases add provider-specific cache adapters and automatic expiry selection, deeper
automatic scheduling, deeper child-agent policy, interactive TUI/debugger controls, fault
injection, artifact retention, fuzzing, RSS, startup, and cross-platform
coverage.
