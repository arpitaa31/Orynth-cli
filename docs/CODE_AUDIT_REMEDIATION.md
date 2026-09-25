# Orynth Audit Remediation

## Phase E measurement evidence

Phase E measured the bounded hot paths rather than claiming the planning
targets from static inspection. Release evidence includes 1/4/10 logical
waiting-agent working-set scenarios, release CLI startup, 1k/10k/50k event
append/reconstruction and snapshot workloads, context through 10k blocks,
SQLite file sizes, bounded IPC through 10k messages, tool validation, and
conflict-heavy assumptions through 10k publications. The exact values and
commands are in `docs/BENCHMARK_RESULTS.md`; raw TSV/text captures are under
`benchmarks/results/`.

The measured assumption hotspot was addressed with a subject index. The
10,000-publication release median fell from 3,486,949 us to 2,565,214 us;
the remaining cost is explicit conflict-record generation. This optimization
does not change the audit status of unrelated storage limitations.

`orynth-bench faults` covered five filesystem suffix truncation points plus
malformed context/IPC/assumption inputs and an unknown tool proposal
(`scenarios=9`, `failures=0`). The bounded smoke runner exercised 30,000
malformed decoder cases with zero observed panics. Focused cargo-fuzz targets
are present, but full cargo-fuzz was unavailable on the Windows host.

ORY-AUDIT-032 remains `PARTIALLY FIXED`: measured hot-memory and persistence
evidence now exists and current inline bounds are enforced, while duplicate
transitive `base64` versions and externalized SQLite blob storage remain
intentional limitations. TUI and cross-platform measurements are deferred to
the next phase and are not represented as passes.

## Phase A

The work below addresses the five S1 findings in `docs/CODE_AUDIT.md`. The
historical audit is unchanged.

### ORY-AUDIT-001

Status: FIXED

Root cause: capability matching compared unnormalized resource strings, while
rooted filesystem effects did not bind authorization to every concrete source
and destination object or reject reparse traversal.

Fix: `orynth-security` now normalizes `.`/`..`, separators, boundaries, and
Windows case behavior. Tool requirements can authorize multiple input paths.
`FilesystemFixture` validates every path component with symlink metadata and
Windows reparse-point attributes before preview, execution, verification, and
compensation. Process and WASM adapters now use the shared matcher.

Regression tests: traversal and boundary capability tests; rooted source and
destination symlink rejection on Unix; concrete source/destination capability
coverage; existing rooted traversal and compensation suites.

Remaining limitations: validation and the following OS operation are not one
handle-relative no-follow transaction. The remaining TOCTOU window is
documented in ADR-0061; unsafe reparse paths fail closed.

Validation: focused security, terminal-tools, plugin-process/WASM compilation,
workspace check, and Clippy pass.

### ORY-AUDIT-002

Status: FIXED

Root cause: quarantine names were process-local basename combinations, Unix
rename could overwrite an existing target, and `.orynth`/quarantine paths were
created through lexical checks.

Fix: quarantine tokens use durable unique IDs; quarantine files are allocated
with exclusive hard links and the original is removed only afterward. Internal
directories and CLI journal, temporary, backup, and record paths reject
symlinks/reparse points and are root-confined. The persistent journal v2 stores
transaction ID, operation ID, original location, and quarantine location while
decoding legacy v1 records.

Regression tests: same-basename removal across separate fixture instances,
reverse undo recovery, persistent CLI undo, rooted internal-path checks, and
symlink/reparse rejection coverage.

Remaining limitations: hard-link allocation requires the quarantine and source
to share a filesystem; terminal effect/journal coupling and directory-entry
fsync are not claimed. Compensation conflicts retain the durable journal
record instead of silently overwriting a destination.

Validation: terminal-tools and CLI focused suites pass; workspace check and
Clippy pass.

### ORY-AUDIT-003

Status: FIXED

Root cause: every kernel durable ID family, plus `ContextBlockId`, restarted a
process-local counter at one.

Fix: all kernel durable ID constructors use a random process prefix plus an
atomic counter, preserving the existing u64 serialization and deterministic
`from_u64` fixtures. Context block IDs use the same allocator. Protocol-local
request IDs and context subscriptions remain intentionally non-durable.

Regression tests: all durable kernel identity families are allocated together
and checked for uniqueness/non-zero values; event-store and context recovery
tests continue to reconstruct references after reopen.

Remaining limitations: the compatibility-preserving u64 scheme has the
collision resistance of its random process prefix rather than a 128-bit UUID;
the entropy-failure fallback is time/process derived. A future on-disk
store-assigned UUID migration would provide a stronger formal guarantee.

Validation: kernel, event-store, workspace check, and Clippy pass.

### ORY-AUDIT-004

Status: FIXED

Root cause: filesystem `append_batch` emitted independent event frames with no
commit boundary, so a valid prefix of a logical batch became authoritative.

Fix: event format v2 writes bounded begin/event/commit frames with a digest.
Recovery publishes only committed batches, truncates incomplete or damaged
final batches to the batch start, rejects non-final corruption, and retains a
legacy v1 reader. Legacy v1 stores are explicitly read-only until migrated;
they cannot be silently appended with v2 frames. Writer and reader frame
limits agree.

Regression tests: interrupted batch recovery, truncation at begin/event/commit
cut points, corrupted final-frame handling, torn-tail repair, checksum
rejection, reopen, duplicate IDs, and the complete event-store suite.

Remaining limitations: directory-entry durability and arbitrary OS-level
write/flush/sync fault injection are not simulated; the recovery rule is
deterministic for the tested torn/corrupt tail cases.

Validation: 48 event-store unit tests pass; workspace check and Clippy pass.

### ORY-AUDIT-005

Status: FIXED

Root cause: MCP authorization compared endpoint text while the URL parser and
HTTP client used normalized structured destinations, and GET/reconnect paths
had no capability context.

Fix: MCP HTTP canonicalizes scheme, case-folded host, default/explicit port,
dot-segment-free path, and query. Encoded dot/separator ambiguity and
credentials/fragments are rejected. Manifest and lease checks compare
structured targets; redirects are disabled; POST, GET, server-response, and
reconnect requests reauthorize the bound capability snapshot.

Regression tests: normalized dot traversal, encoded dot/separator rejection,
host case/default port, sibling-boundary, IPv6, GET stream, POST SSE,
reconnect, and server-request transport suites.

Remaining limitations: the current adapter API stores a cloned capability
policy, so revocation of the original mutable policy after connection is not
observed. Redirects are rejected rather than reauthorized.

Validation: MCP library and HTTP integration tests pass; workspace check and
Clippy pass.

## Validation summary

- New/expanded targeted regression tests: 8 (security normalization, kernel
  identity families, terminal symlink/source-destination and quarantine
  collision, MCP structured URL cases, filesystem batch interruption/cut
  points, and CLI undo identity format).
- Related issues found and addressed: duplicate lexical resource matchers in
  process/WASM adapters; redirected internal CLI undo paths; generic
  filesystem tool capability requirements that skipped destinations.
- Residual hardening: handle-relative no-follow filesystem operations, live
  mutable MCP policy bindings, directory fsync, and effect/journal atomic
  coupling remain explicit follow-up work.
- Release validation was attempted; Windows Application Control blocked
  dependency build-script executables with OS error 4551 before compilation
  could complete.

## Phase B

Phase B addresses ORY-AUDIT-006 through ORY-AUDIT-018. Phase A behavior and
the Phase A focused regression suites were preserved and rerun before this
work.

### ORY-AUDIT-006

Status: FIXED

Root cause: snapshot encoding persisted only the current effective model, so
promotion or demotion could overwrite the logical model identity and make
recovery disagree with the event-derived state.

Fix: snapshot format v4 persists both immutable logical identity and current
effective model. Older snapshot formats remain readable and conservatively
interpret their single model field as both values.

Files changed: `crates/event-store/src/sqlite.rs`,
`crates/event-store/src/lib.rs`, `crates/event-store/src/durable.rs`.

Regression tests: in-memory codec/recovery, filesystem reopen, and SQLite
reopen after model promotion and demotion.

Validation: focused event-store suite passes (51 tests).

Remaining limitations: legacy snapshots cannot recover a distinction that was
never encoded; they use the documented compatibility fallback.

### ORY-AUDIT-007

Status: FIXED

Root cause: invalidation transitions updated lifecycle state without recording
an auditable invalidation history entry, and replay could therefore drift from
live state.

Fix: replay records invalidation entries exactly once, preserves dependency
invalidation history, and keeps replay idempotent.

Files changed: `crates/context/src/lib.rs`.

Regression tests: invalidation history is compared after live application and
single/double replay.

Validation: focused context suite passes (19 tests).

Remaining limitations: history is event-derived; callers that mutate a graph
directly without persisting the returned transition do not create durable
history.

### ORY-AUDIT-008

Status: FIXED

Root cause: stale propagation stopped at archived or already-stale nodes, so
deep dependency chains could leave descendants appearing fresh; restore also
could reactivate content whose dependencies had changed.

Fix: propagation traverses archived, stale, superseded, and invalidated
intermediate nodes; archived dependents become `ArchivedStale`; restore checks
dependency/source revisions and returns `Stale` when freshness is not proven.

Files changed: `crates/context/src/lib.rs`.

Regression tests: archived intermediate dependency chains, stale restore, and
replay of the resulting transitions.

Validation: focused context suite passes (19 tests).

Remaining limitations: automatic archival and refresh policy remain outside
this phase; lifecycle changes remain explicit transitions.

### ORY-AUDIT-009

Status: FIXED

Root cause: prompt rendering could accept references that were visible but not
active, allowing archived, stale, superseded, or invalidated blocks to enter a
prompt.

Fix: prompt rendering now requires every referenced block to be `Active` after
visibility checks, while preserving private-scope isolation and trust policy
enforcement.

Files changed: `crates/context/src/lib.rs`.

Regression tests: private visibility remains denied to another agent and an
archived reference is rejected for its owner.

Validation: focused context suite passes (19 tests).

Remaining limitations: callers must choose replacement references explicitly;
the renderer does not silently substitute newer blocks.

### ORY-AUDIT-010

Status: FIXED

Root cause: disabled or stale cache observations could still influence ranking
tie-breaks, and unlimited budget sources could mint finite recipient capacity.

Fix: cache observations affect ranking only when enabled and fresh. Positive
budget transfer from an unlimited source is rejected; zero transfer remains a
no-op, and transfers to an unlimited target preserve the existing unlimited
semantics while decrementing a finite source.

Files changed: `crates/scheduler/src/lib.rs`.

Regression tests: disabled/stale cache tie behavior, finite/unlimited transfer
edges, and existing usage-safe transfer rejection.

Validation: focused scheduler suite passes (14 tests).

Remaining limitations: cache freshness remains caller-supplied and does not
claim provider-side physical cache state.

### ORY-AUDIT-011

Status: FIXED

Root cause: health projections retained historical counters but used those
counters as if they represented currently active pressure, so resolved issues
could leave agents blocked.

Fix: each health dimension now has an active counter and explicit replayable
resolution signals. Historical evidence remains available while status derives
from active pressure.

Files changed: `crates/scheduler/src/lib.rs`.

Regression tests: blocked health recovers after progress and resolved conflict
signals while historical counts remain intact; new signal tags round-trip.

Validation: focused scheduler suite passes (14 tests).

Remaining limitations: resolution is explicit and deterministic; no automatic
inference of resolution is introduced.

### ORY-AUDIT-012

Status: FIXED

Root cause: cache-aware routing treated stale evidence as usable and could
fabricate warm-cache preference from incomplete observations.

Fix: routing now requires exact provider/model/prefix observations with valid
caller-supplied freshness metadata, and ranking ignores disabled or stale
observations.

Files changed: `crates/scheduler/src/lib.rs`.

Regression tests: exact observation ranking plus absent, disabled, stale, and
unverifiable evidence cases.

Validation: focused scheduler suite passes (14 tests), with runtime cache
recovery tests also passing.

Remaining limitations: provider-specific TTL adapters and automatic expiry
selection remain future work.

### ORY-AUDIT-013

Status: FIXED

Root cause: some runtime mutation methods appended events directly without
reconstructing the authoritative run state and validating membership,
ordering, lifecycle, and terminal constraints first.

Fix: all public runtime mutation paths use centralized validated single-event
or batch append helpers. Candidate state is reconstructed and advanced before
the event reaches the store.

Files changed: `crates/event-store/src/lib.rs`, `crates/runtime/src/lib.rs`.

Regression tests: unknown runs, cross-run agent membership, terminal-agent
mutation, and invalid pause/resume/model transitions are rejected without an
append.

Validation: focused runtime suite passes (33 tests).

Remaining limitations: the intentionally exposed low-level `event_store_mut`
adapter accessor remains an escape hatch for store-level fixtures and tooling;
service mutation APIs do not use it to bypass validation.

### ORY-AUDIT-014

Status: FIXED

Root cause: tool execution represented output as an unconditional string and
did not distinguish an unattempted effect from an effect that may have
occurred, making failed verification and uncertain execution ambiguous.

Fix: typed execution output is optional and carries `NotAttempted`,
`MayHaveOccurred`, or `Confirmed` effect status. Verification failure retains
eligible compensation metadata; absent confirmed output is valid, while an
unattempted execution is terminally failed.

Files changed: `crates/tool-runtime/src/lib.rs`,
`crates/terminal-tools/src/lib.rs`, `crates/plugin-mcp/src/lib.rs`.

Regression tests: opaque-value preservation, failed-verification compensation,
confirmed no-output execution, and existing transactional effect-boundary
coverage.

Validation: focused tool-runtime and terminal-tools suites pass (14 tests each).

Remaining limitations: an executor that returns only an error cannot provide
compensation metadata; the runtime records the uncertainty but cannot invent a
safe compensator.

### ORY-AUDIT-015

Status: FIXED

Root cause: tool transaction state permitted ambiguous execution/output paths
and compensation handling did not consistently preserve failure state.

Fix: execution state transitions classify absent output and effect certainty,
verification receives an explicit empty output for no-output tools, and
compensation is available after failed verification when the effect is known or
may have occurred and the definition is reversible.

Files changed: `crates/tool-runtime/src/lib.rs` and tool adapter call sites.

Regression tests: failed verification with retained compensation, confirmed
no-output terminal execution, and durable tool transaction recovery.

Validation: focused tool-runtime, terminal-tools, and runtime suites pass.

Remaining limitations: crash recovery still exposes an in-flight `Executing`
record for operator policy; it does not pretend that an interrupted external
effect was confirmed.

### ORY-AUDIT-016

Status: FIXED

Root cause: CLI parsing sliced `args[2..]` before checking that an operation
actually had an operation argument, and local translation lowercased user
values while recognizing phrases.

Fix: incomplete `plan`/`execute` forms now return typed errors. Recognition
uses a lowercased copy, while captured paths, values, and Unicode text retain
their original spelling except for explicitly defined outer quote/whitespace
syntax.

Files changed: `crates/cli/src/lib.rs`.

Regression tests: incomplete-command no-panic cases and case-sensitive path /
Unicode translation.

Validation: focused CLI suite passes (17 tests).

Remaining limitations: unsupported natural language remains rejected rather
than guessed.

### ORY-AUDIT-017

Status: FIXED

Root cause: generic proposal repair trimmed all input values, corrupting
opaque content fields and conflating syntax normalization with content
normalization.

Fix: `ToolDefinition` declares syntax fields explicitly. Only those fields are
normalized; opaque values are bounded and NUL-checked without changing bytes.

Files changed: `crates/tool-runtime/src/lib.rs` and tool definition call sites.

Regression tests: path syntax trimming alongside byte-preserving multiline
content, plus deterministic repair collision checks.

Validation: focused tool-runtime suite passes (14 tests).

Remaining limitations: each adapter must declare its syntax fields accurately;
the generic runtime does not infer field semantics.

### ORY-AUDIT-018

Status: FIXED

Root cause: process policy relied on a partial shell-name blacklist rather than
an allowlist enforced at the actual injected invocation boundary.

Fix: `ProcessPolicy` is deny-by-default and permits only explicitly approved
executable identities. The policy is checked immediately before injected
preview/execute invocation, with Windows case folding and structural
metacharacter rejection retained.

Files changed: `crates/terminal-tools/src/lib.rs`.

Regression tests: explicit approval of `cargo` and denial of cmd, PowerShell,
pwsh, bash, sh, zsh, and unexpected executable identities.

Validation: focused terminal-tools suite passes (14 tests).

Remaining limitations: the current fixture policy is executable-identity based;
argv-level command profiles and production OS sandboxing remain future work.

## Phase B validation summary

- Focused Phase A regression suites were rerun before Phase B validation and
  remained green.
- Focused Phase B suites pass: context (19), scheduler (14), tool-runtime
  (14), terminal-tools (14), CLI (17), event-store (51), and runtime (33).
- Phase B added or expanded 16 targeted regression cases across the affected
  crates, including snapshot reopen, archived dependency propagation,
  invalidation replay, cache/budget edges, health resolution, typed tool
  effects, runtime mutation validation, CLI preservation, and process policy.
- Cross-cutting search covered normalization, cache ranking, unlimited budget
  transfer, health signals, lifecycle handling, direct runtime appends,
  executing tool state, and shell interpreter policy.
- Focused workspace validation and release build pass after the documentation
  changes; the current exact all-features workspace test gate is documented as
  host-policy blocked in the Phase C and Phase D summaries.
- No Phase C work is included or authorized by this remediation.

## Phase C

Phase C addresses ORY-AUDIT-019 through ORY-AUDIT-025. The historical audit
file remains unchanged. Phase A and Phase B behavior and focused suites were
preserved and rerun.

### ORY-AUDIT-019 — Process plugin response pump uses an unbounded queue

Status: FIXED

Root cause: the process-session response reader used an unbounded channel, so
a child that emitted faster than its consumer could grow host memory without a
protocol-level limit.

Fix: process sessions now use a bounded frame-and-byte response queue. The
reader applies backpressure when either bound is full; queue close wakes both
producers and consumers during termination. Discovery also retains only a
bounded manifest-path list instead of materializing an entire directory
listing.

The response queue holds at most 8 frames and at most
`min(8 * max_frame_bytes, 8 MiB)` queued payload bytes. The manifest limit
caps `max_frame_bytes` at 1 MiB, so queued payload storage is at most 8 MiB;
the reader may hold one additional in-progress frame of at most 1 MiB (plus
the fixed `BufReader` buffer), for a total response-payload bound of 9 MiB.
Overflow blocks the reader until the consumer pops data or the queue closes;
an oversized frame is rejected before enqueueing.

Files changed: `crates/plugin-process/src/lib.rs`,
`crates/plugin-discovery/src/lib.rs`.

Regression tests: queue backpressure keeps frame and byte metrics within the
configured limits; closing a full queue unblocks the reader; discovery covers
bounded file admission and deterministic candidate handling.

Remaining limitations: the fixed queue is an in-process backpressure boundary,
not a child-process CPU or output-rate quota. The child can still consume its
own resources until the session timeout or OS containment terminates it.

### ORY-AUDIT-020 — Process plugin writes can block forever before the request timeout

Status: FIXED

Root cause: synchronous process-session stdin writes occurred on the caller
thread before the response timeout was entered, allowing a full child pipe to
block indefinitely.

Fix: stdin is owned by a dedicated writer thread behind a one-item synchronous
request channel. Write completion is awaited with the session timeout; a full
handoff, write failure, or timeout terminates the child before joining the
writer. Persistent request send and response receive share one deadline,
capped by the manifest wall-time limit. One-shot process invocation also uses
a bounded result channel and joins its I/O worker after child termination.

Files changed: `crates/plugin-process/src/lib.rs`.

Regression tests: real process command-host launch, crash, and timeout suites;
bounded writer/response queue unit coverage.

Remaining limitations: portable Rust cannot forcibly interrupt every kernel
write operation independently of closing the child and its pipes; termination
is therefore the cancellation boundary. A second in-flight write fails fast
rather than creating another queued request.

### ORY-AUDIT-021 — Windows process containment is attached after the child starts

Status: FIXED

Root cause: Windows process jobs were assigned after `spawn`, leaving a window
where the child could execute before containment was attached.

Fix: contained Windows commands are created suspended. The host creates and
configures the Job Object, assigns the suspended process, then resumes its
primary thread. Assignment or resume failure kills and waits for the child.
The non-Windows adapter remains explicitly fail-closed when containment is
required.

Files changed: `crates/plugin-process/src/lib.rs`.

Regression tests: process command-host launch and timeout coverage; the
Windows-specific path is implemented behind the platform adapter and is
covered by release/Clippy compilation on the supported host.

Remaining limitations: this is Job Object containment, not a complete Windows
AppContainer/token/network/filesystem sandbox. Thread enumeration is kept in a
small Win32 adapter and should receive platform-specific fault injection in a
future hardening cycle.

### ORY-AUDIT-022 — Plugin discovery and WASM activation read unbounded files before checking limits

Status: FIXED

Root cause: discovery revalidation and WASM activation used `fs::read`, so
manifest/module size policy was checked only after the entire file had already
been allocated.

Fix: `read_bounded_file` reads at most `max_bytes + 1`, rejects the extra byte,
and is used for manifest revalidation, directory discovery, and WASM module
activation. Directory discovery retains only bounded manifest paths. Existing
metadata prechecks remain only as an early optimization, not the safety
boundary.

Files changed: `crates/plugin-discovery/src/lib.rs`,
`crates/plugin-wasm/src/lib.rs`.

Regression tests: exact-limit, below-limit, limit-plus-one, and zero-limit
bounded-file cases; policy-preserving revalidation; normal WASM activation;
and oversized-module activation rejection before module parsing.

Remaining limitations: the bounded read still depends on the host filesystem
and does not claim a race-free identity binding between metadata and later
activation.

### ORY-AUDIT-023 — Plugin host `max_plugins` is enforced per activation call, not cumulatively

Status: FIXED

Root cause: plugin activation applied `max_plugins` only to the current
discovery batch, so repeated activation could exceed the host-wide limit.

Fix: activation computes the cumulative existing-plus-incoming count before
staging adapters. The check is atomic with staged activation; failed batches do
not consume slots, and explicit deactivation frees one.

Files changed: `crates/plugin-host/src/lib.rs`.

Regression tests: the host activates one plugin under a one-slot limit, rejects
the repeated batch without changing state, deactivates, and successfully
reuses the slot.

Remaining limitations: activation is process-local and does not coordinate a
limit across multiple host processes.

### ORY-AUDIT-024 — Legacy MCP handshake accepts an unsupported server protocol version

Status: FIXED

Root cause: legacy MCP initialize responses were converted into server metadata
without requiring or validating the server-returned `protocolVersion` against
the selected wire mode.

Fix: stdio and HTTP initialize parsing require a string `protocolVersion`,
accept only the supported legacy/modern versions, and require exact agreement
with the selected legacy mode. Session state records the negotiated version;
modern sessions retain their explicit per-request mode.

Files changed: `crates/plugin-mcp/src/lib.rs` and MCP integration fixtures.

Regression tests: missing, old, future, and wrong-mode legacy responses are
rejected; legacy and modern stdio/HTTP round trips remain green.

Remaining limitations: only the two protocol revisions currently modeled by
Orynth are accepted; adding another revision requires an explicit mode and
wire-contract update.

### ORY-AUDIT-025 — A2A/IPC and ownership metadata do not enforce resource ownership across all effect paths

Status: FIXED

Root cause: scheduler ownership was a descriptive projection, while tool,
terminal, plugin, and cancellation effect paths checked capability leases but
did not enforce exclusive ownership of concrete resources.

Fix: `orynth-security` now defines a separate read/write ownership policy
contract and deterministic resource-overlap matching. Scheduler claims reject
cross-agent parent/child overlaps and implement the policy; tool definitions
declare ownership fields and recheck them at validation, execution, and
compensation boundaries; implicit tool helpers deny write effects unless an
explicit policy is supplied; CLI terminal effects use an explicit workspace
owner; plugin transport invocation carries the authoritative policy through
native, WASM, MCP, and host adapters; HTTP MCP connection/event-stream paths
require connection-bound ownership admission; cancellation emits durable
ownership releases; scheduler claims use the same canonical resource identity.
Read access remains shareable, while writes require a requester-owned claim and
foreign overlapping claims are conflicts. Capability grants remain a separate
permission layer. Process executable and MCP endpoint adapters additionally
authorize their concrete effect resource at the spawn/connect boundary.

Files changed: `crates/security/src/lib.rs`, `crates/scheduler/src/lib.rs`,
`crates/tool-runtime/src/lib.rs`, `crates/terminal-tools/src/lib.rs`,
`crates/cli/src/lib.rs`, `crates/plugin-api/src/lib.rs`,
`crates/plugin-process/src/lib.rs`, `crates/plugin-wasm/src/lib.rs`,
`crates/plugin-mcp/src/lib.rs`, `crates/plugin-host/src/lib.rs`, and
`crates/runtime/src/lib.rs`.

Regression tests: scheduler read/write and overlap cases, tool effect-boundary
rechecks, runtime cancellation cleanup, plugin integration calls with an
explicit policy, and existing terminal rooted source/destination tests.

Remaining limitations: process/plugin manifests do not yet carry a separate
read-vs-write access declaration, so manifest-declared effects are conservatively
treated as writes. Filesystem final-object validation still has the Phase A
TOCTOU limitation. HTTP capability and ownership inputs are cloned at
connection time, so live revocation requires disconnect/reconnect; the
connection itself and every adapter invocation still fail closed without the
required authorization.

## Phase C validation summary

- All seven scoped findings ORY-AUDIT-019 through ORY-AUDIT-025 are fixed: 7/7.
- Focused Phase C suites pass for process, MCP HTTP, host, WASM, scheduler,
  tool-runtime, terminal-tools, and runtime, including real process and HTTP
  integration. The discovery suite is compiled but its generated test binary
  is blocked by Windows Application Control (OS error 4551) in this
  environment; the MCP stdio integration binary is blocked the same way.
- 119 directly exercised Phase C tests passed in the final validation runs;
  the blocked discovery and stdio binaries account for the unexecuted final
  reruns, not assertion failures.
- Phase A/B focused regression suites remain green where executed. The exact
  all-features workspace test command is environment-blocked when launching
  generated test binaries (OS error 4551), not failed by an assertion. The
  release workspace build, workspace check, formatting, and Clippy pass.
- Architecture decisions are recorded in ADR-0063.
- Phase D was explicitly out of scope for the Phase C cycle.

## New findings from Phase C

### NEW-AUDIT-C-001

Severity: S2
Confidence: HIGH
Status: FIXED

Root cause: discovery materialized every directory entry before enforcing its
manifest admission bound.

Fix: discovery now retains only candidate manifest paths and rejects
over-limit candidate counts while scanning.

Tests: bounded discovery admission and deterministic candidate tests.

### NEW-AUDIT-C-002

Severity: S2
Confidence: HIGH
Status: FIXED

Root cause: activation revalidation could have used the process-wide manifest
maximum instead of the lower policy that produced a retained candidate.

Fix: the candidate now carries its admission limit and revalidation enforces
it.

Tests: `revalidation_preserves_the_original_manifest_limit` covers file growth
past the retained boundary.

### NEW-AUDIT-C-003

Severity: S2
Confidence: HIGH
Status: FIXED

Root cause: persistent process-session termination killed the direct child but
left the Windows Job Object open until the session was dropped.

Fix: termination now closes platform containment before joining I/O workers,
activating descendant cleanup.

Tests: response-queue close/unblock coverage and real process crash/timeout
termination integration tests.

### NEW-AUDIT-C-004

Severity: S2
Confidence: HIGH
Status: FIXED

Root cause: legacy `ToolRuntime` convenience methods implicitly supplied an
allow-all ownership policy.

Fix: convenience methods now deny write effects unless the caller supplies an
explicit ownership policy; terminal adapters pass an explicit policy.

Tests: implicit-write-denial and explicit terminal ownership-boundary tests.

### NEW-AUDIT-C-005

Severity: S2
Confidence: HIGH
Status: FIXED

Root cause: process executable and HTTP MCP endpoint effects were checked only
through declared manifest resources.

Fix: the concrete executable/endpoint is now checked against ownership at
spawn/connect.

Tests: process effect admission and HTTP ownership-denial regression proving
rejection before network I/O.

## Phase D

Phase D addresses ORY-AUDIT-026 through ORY-AUDIT-032. The historical audit
file remains unchanged. The benchmark/fuzzing phase is explicitly deferred.

### ORY-AUDIT-026

Status: FIXED

Root cause: the provider boundary represented only a prompt string, a text
iterator, and a final usage-bearing chunk, so tool calls, structured output,
reasoning, modalities, finish reasons, cost/cache metadata, and classified
provider failures could not be represented without provider-specific leakage.

Architecture decision: ADR-0064 defines a provider-neutral typed request/event
contract while keeping runtime state authoritative.

Files changed: `crates/provider/src/lib.rs`, `crates/agent/src/lib.rs`.

Fix: requests now carry bounded parts, tools, structured-output and reasoning
options, and extension data. Providers advertise capabilities and validate
negotiation before streaming typed text/reasoning/tool-call/usage/finish
events. Errors classify cancellation, timeout, rate limiting, capability
mismatch, malformed streams, and ordinary failure. Agent executions retain
bounded text deltas rather than requiring one giant response string.

Tests: provider tests cover normal text streaming, usage, finish reasons,
tool calls, malformed tool streams, cancellation, provider failure, empty
responses, and capability mismatch; agent tests cover cancellation, provider
failure, fork continuation, and typed-stream execution.

Validation: provider and agent focused suites pass.

Remaining limitations: no real network provider adapters, provider-specific
retry policy, or empirical stream/RSS measurements are claimed in Phase D.

### ORY-AUDIT-027

Status: FIXED

Root cause: metadata persistence removed the committed sidecar before the new
temporary file was installed, so a crash or rename failure could erase the
only valid generation.

Architecture decision: ADR-0064 uses same-directory synced temporary files and
recoverable backup generations.

Files changed: `crates/event-store/src/durable.rs`.

Fix: metadata writes sync a unique temporary file, move the old generation to
`.bak`, install the new generation, sync the parent directory where supported,
and remove the backup only after installation. Open recovers a valid backup or
deterministically migrates an unambiguous legacy sidecar.

Tests: backup recovery, valid-primary precedence over stale generations,
legacy migration, metadata corruption/torn-tail recovery, branch/snapshot
reopen, orphan/partial temporary files, and existing filesystem crash-point
tests.

Validation: event-store focused suite passes.

Remaining limitations: directory fsync and power-loss behavior remain
platform-dependent; no impossible universal durability guarantee is claimed.

### ORY-AUDIT-028

Status: FIXED

Root cause: `with_extension("meta")` could map an event filename ending in
`.meta` back onto the event file itself.

Architecture decision: ADR-0064 defines explicit sibling suffixes.

Files changed: `crates/event-store/src/durable.rs`, `docs/PERSISTENCE.md`.

Fix: metadata paths append a suffix to the complete event filename,
preserving deterministic identity for extensionless, dotted, and `.meta`
filenames. The established lock path is retained for compatibility, and
legacy sidecars migrate only when their path is distinct from the event file.

Tests: `.meta` event filename collision, legacy sidecar migration, normal
metadata reopen, branch persistence, and snapshot persistence.

Validation: event-store focused suite passes.

Remaining limitations: legacy stores with a physically overwritten event file
cannot be reconstructed; they are rejected rather than interpreted as empty.

### ORY-AUDIT-029

Status: FIXED

Root cause: fork remapping decoded legacy IPC, assumption, and tool payloads,
re-encoded current fields, and retained the legacy schema tag.

Architecture decision: ADR-0064 requires the persisted tag to describe the
bytes actually emitted.

Files changed: `crates/event-store/src/lib.rs`.

Fix: successful legacy remaps now write the current IPC, assumption, or tool
schema constant; failed decodes preserve the original opaque payload/tag
instead of claiming a transformed representation.

Tests: legacy IPC, assumption, and tool payloads are forked, re-encoded,
decoded after remapping, and checked after persistence while the parent event
versions remain unchanged. Opaque decode failures also preserve their legacy
payload and tag.

Validation: event-store focused suite passes.

Remaining limitations: future schema families must add an explicit remapper
before becoming fork-remappable.

### ORY-AUDIT-030

Status: FIXED

Root cause: metadata frame writing used only a u32 conversion while recovery
enforced the domain frame limit, allowing a writer path to create data that
recovery would reject.

Architecture decision: ADR-0064 makes the frame limit a shared writer/reader
boundary.

Files changed: `crates/event-store/src/durable.rs`.

Fix: event and metadata writers reject payloads over the same `MAX_FRAME_BYTES`
used by event and metadata readers before any file write. The existing event
batch writer already applies that limit to control and event frames.

Tests: bounded metadata/event persistence, exact `max-1`/`max`/`max+1`
event and metadata boundaries, corruption and truncation recovery, and reopen
tests.

Validation: event-store focused suite passes.

Remaining limitations: the current 64 MiB domain cap is intentionally
conservative for compatibility; payload-specific codecs retain their smaller
limits.

### ORY-AUDIT-031

Status: FIXED

Root cause: CI and canonical status/testing documents described a narrower
test command as a full release gate and did not distinguish environment
blocks, placeholder examples, or unmeasured benchmark targets.

Architecture decision: no feature architecture change; evidence labels are
now explicit and the release gate is represented in CI.

Files changed: `.github/workflows/ci.yml`, `README.md`, `docs/STATUS.md`,
`docs/TESTING.md`, `docs/PERSISTENCE.md`, `plans/CURRENT.md`.

Fix: CI now runs formatting, all-features check/Clippy/test, and release build
on Ubuntu, Windows, and macOS. Docs distinguish implemented, tested,
environment-blocked, planned, and unmeasured behavior; placeholder directories
remain explicitly future work rather than being presented as finished demos.

Tests: workflow/document consistency inspection and all focused suites used by
the current validation matrix.

Validation: local formatting/check/Clippy and focused tests pass; any local
Windows Application Control error 4551 remains ENVIRONMENT-BLOCKED, never a
reported pass.

Remaining limitations: hosted CI results are not available from this local
workspace, and cross-platform claims remain conditional on matrix execution.

### ORY-AUDIT-032

Status: PARTIALLY FIXED

Root cause: duplicate upstream `base64` versions remain in the reqwest graph,
SQLite stores artifacts inline, and provider/agent output retention had no
explicit hot-memory thresholds or measurements.

Architecture decision: ADR-0064 bounds current hot paths without forcing
fragile dependency patches or prematurely building a new blob subsystem.

Files changed: `crates/agent/src/lib.rs`, `crates/event-store/src/lib.rs`,
`crates/event-store/src/durable.rs`, `crates/event-store/src/sqlite.rs`,
`docs/PERSISTENCE.md`, `docs/BENCHMARKS.md`.

Fix: provider streams pull typed deltas, agent output is retained as bounded
chunks with a 4 MiB/65,536-chunk ceiling, active and completed tool calls are
cardinality-bounded, and inline artifact stores reject payloads over 16 MiB
consistently across in-memory, filesystem, and SQLite adapters.
Duplicate base64 versions were inspected and left as transitive reqwest
dependencies because unification would require a fragile upstream patch.

Tests: provider/agent stream and output-bound tests, artifact reopen and
deduplication tests, and dependency-tree inspection.

Validation: focused provider, agent, and event-store suites pass; `cargo tree
--workspace --duplicates` reports only the two transitive base64 versions.

Remaining limitations: RSS, startup, reconstruction, append throughput, and
context/rendering benchmarks are intentionally deferred to the dedicated
benchmark/hardening phase. External content-addressed SQLite blob storage is
also future work.

## New findings from Phase D

### NEW-AUDIT-D-001

Severity: S2

Confidence: HIGH

Area: fork remapping and opaque versioned payload fallback

Root cause: the initial Phase D remapper assigned the current schema tag even
when a legacy payload could not be decoded and was retained unchanged.

Impact: an opaque child event could claim a schema version that did not
describe its bytes, recreating the exact replay ambiguity addressed by
ORY-AUDIT-029.

Fix: failed IPC, assumption, and tool remaps now preserve both the original
payload and its original version tag; only successful re-encodes receive the
current tag.

Tests: `failed_fork_remaps_preserve_opaque_payload_schema_tags` plus the
successful legacy IPC/assumption/tool fork and reopen coverage.

Status: FIXED
