# Orynth Deep Code Audit

Audit date: 2026-09-23  
Commit/revision: unborn repository (no Git commits); audited working tree SHA inventory captured before the report was written  
Auditor: Codex, with independent persistence/context, security/tools, and plugin/protocol passes  
Scope: 173 non-`.git`/non-`target` files: 34 Rust sources, 31 manifests/tooling/CI files, 105 Markdown/design files, and the examples, fixtures, and scripts. All meaningful workspace crates and application sources were read; focused repros were run for the filesystem and plugin findings. No production source was changed.

## Executive Summary

Orynth has an unusually explicit architecture and a substantial deterministic test suite. The strongest parts are the separation of kernel/provider/runtime domains, opaque versioned domain events, bounded codecs, SQLite transaction use, and the deliberate documentation of deferred features. The current implementation is not safe to treat as a security boundary or crash-consistent runtime, however. The most serious problems are capability checks based on lexical paths rather than the final filesystem object, internal quarantine/undo paths that follow junctions, and restart-unsafe identity allocation. Those defects allow writes outside a granted subtree and data loss while the user-facing result reports success.

Several claims in the canonical documents are stronger than the implementation: filesystem event batches are not recoverably atomic, context invalidation replay is incomplete, and plugin/MCP limits do not bound all memory or network paths. The report distinguishes current defects from explicitly documented future work such as real provider adapters, full OS sandboxing, and interactive TUI controls.

## Overall Assessment

The repository is a credible experimental vertical slice, not a release-ready runtime. The runtime authority model is present in types and recovery code, but important effect boundaries still depend on advisory metadata and caller discipline. Persistence is good for ordinary happy-path reopen and SQLite transactions, but the filesystem adapter can lose or partially publish multi-event state under interruption. Context, scheduler, tool, plugin, and protocol projections each have local invariants that are not consistently preserved across replay, restart, or boundary normalization. The absence of measured benchmarks, cross-platform security tests, fuzzing, and real provider integration leaves the end-goal claims unverified.

## Validation Performed

- Read `AGENTS.md`, `BLUEPRINT.md`, all canonical documents listed by the task, `plans/`, and all 60 architecture decisions.
- Enumerated and inspected 173 repository files, including all 34 Rust sources, manifests, CI, examples, fixtures, and scripts.
- `cargo fmt --all -- --check`: passed.
- `cargo check --workspace`: passed.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: passed when run outside the sandbox; the in-sandbox attempt was blocked by Windows Application Control (4551).
- `cargo test --workspace --all-features`: the test binaries that executed passed (including 47 event-store, 32 runtime, 14 CLI, 14 MCP integration, 10 WASM, 4 process-host integration, and the other crate suites), but the complete command could not finish because Application Control (4551) blocked several generated test executables (`orynth-context`, `orynth-failure-memory`, `orynth-plugin-mcp`, and the process fixture). This is an environment validation failure, not a test failure.
- `cargo build --release --workspace`: failed before completion because Application Control (4551) blocked the `libsqlite3-sys` build script. `cargo tree --workspace --duplicates` found two normal `base64` versions (`0.22.1` and `0.23.1`).
- Focused isolated reproductions confirmed the quarantine collision/data loss and Windows junction escapes. Temporary diagnostic files/directories were removed after inspection.

## Severity Summary

| Severity | Count |
|---|---:|
| S0 | 0 |
| S1 | 5 |
| S2 | 19 |
| S3 | 7 |
| S4 | 1 |
| INFO | 0 |

## Highest-Risk Findings

- **ORY-AUDIT-001 (S1):** A lexical capability prefix plus a junction/symlink lets a granted filesystem lease write outside its subtree.
- **ORY-AUDIT-002 (S1):** Quarantine names collide across operations and internal quarantine/undo paths follow junctions, causing confirmed data loss and writes outside the configured root.
- **ORY-AUDIT-003 (S1):** Process-restart ID allocation starts at one again, colliding with persisted event/context/domain IDs and making later appends or recovery unsafe.
- **ORY-AUDIT-004 (S1):** Filesystem append batches have no commit marker; an interrupted “atomic” child/conflict batch can recover as a valid partial state.
- **ORY-AUDIT-005 (S1):** MCP endpoint authorization checks the original text while the HTTP client executes a normalized URL, allowing `..` endpoint traversal; GET streams also have a public path without capability recheck.

## Architecture Compliance

The implementation generally preserves `Agent != Model`, uses typed IPC rather than transcript copying, keeps MCP/A2A outside the kernel, and recovers multiple projections from one event sequence. The following boundaries are weakened in current code:

- Runtime state is not fully authoritative at restart because ID allocation is process-local and several runtime methods append events without first validating run membership.
- Capability policy is authoritative only for lexical strings; the effect adapter does not authorize the canonical final object and does not enforce scheduler ownership.
- Event-store `append_batch` is atomic for SQLite/in-memory but only a sequence of independent frames for the filesystem adapter.
- Context graph replay does not rebuild all live invalidation observations, and archived nodes stop transitive invalidation.
- Provider abstraction is a synchronous iterator/mock contract, not the documented streaming/tool/usage/provider integration boundary.

## Detailed Findings

### ORY-AUDIT-001 — Lexical filesystem capability checks are bypassed by junctions/symlinks

Severity: S1  
Confidence: CONFIRMED  
Area: security, tools, resource ownership

Affected:
- `crates/security/src/lib.rs:594-603`, `resource_matches`
- `crates/tool-runtime/src/lib.rs:1057-1065`
- `crates/terminal-tools/src/lib.rs:690-712`, `checked_path`
- rooted filesystem operation handlers around lines 745-1102

Problem: authorization matches a requested string beneath a granted resource, then the adapter canonicalizes/opens a path under the broad fixture root. It does not resolve the final path and re-authorize it beneath the lease resource, and it does not use no-follow handles.

Evidence and failure scenario: an isolated Windows repro granted `allowed` and requested `allowed\link\restricted.txt`, where `link` was a junction to `restricted`. The operation was accepted and wrote `restricted\restricted.txt` outside the lease. This requires no race. The same lexical prefix matcher is used by process/WASM/MCP capability checks.

Impact: a model/agent with a narrow lease can mutate another agent’s resource or a protected filesystem target. This contradicts the documented least-privilege guarantee.

Recommended fix: authorize canonical final components at the effect boundary, reject reparse points/junctions unless explicitly allowed, and use platform-specific no-follow/open-relative primitives. Carry access mode and canonical resource identity through tool definitions. Add Windows junction, Unix symlink, case-folding, UNC, and TOCTOU tests.

### ORY-AUDIT-002 — Quarantine and undo paths permit outside-root writes and overwrite prior data

Severity: S1  
Confidence: CONFIRMED  
Area: terminal effects, compensation, data loss

Affected:
- `crates/terminal-tools/src/lib.rs:591`, `:824-841`
- `crates/terminal-tools/src/lib.rs:828-836`, quarantine naming
- `crates/cli/src/lib.rs:525-624`, persistent undo journal

Problem: quarantine names are generated from a per-fixture counter and basename, so each new process starts at `fs-fixture-1-...`; a second same-basename removal can overwrite the first quarantined file. `.orynth-quarantine` and `.orynth` are internal paths checked lexically and are not canonicalized before creation/use.

Evidence and failure scenario: removing two `same.txt` files in separate directories reported committed effects, but the second operation overwrote the first quarantine object. The first original was permanently lost; undo restored only the second. Separate Windows junction repros redirected `.orynth-quarantine` and `terminal-undo.log` outside the configured root; the operation reported success, while undo later refused the escaped path.

Impact: irreversible data loss and writes outside the configured root in the advertised reversible terminal workflow.

Recommended fix: use collision-resistant, durable transaction IDs and exclusive create; never overwrite an existing quarantine object. Create internal directories only after no-follow/canonical-root validation, store journal paths as validated handles or root-relative records, and fsync directory metadata where claimed. Add duplicate-basename, crash, junction, and cross-process undo tests.

### ORY-AUDIT-003 — Process-local ID allocation is not restart-safe

Severity: S1  
Confidence: HIGH  
Area: kernel identity, persistence, event sourcing

Affected:
- `crates/kernel/src/lib.rs:10-21`, global `NEXT_ID`
- `crates/context/src/lib.rs:24-47`, `ContextBlockId`
- all `EventId`, `RunId`, `TaskId`, `AgentId`, and domain ID constructors

Problem: IDs are generated by atomics initialized to one in each process. Recovery decodes existing IDs but does not advance allocators to the maximum persisted value.

Failure scenario: reopen a database after a prior process created events, then create a new event or context block. The new process can reuse an existing ID. Event append may reject it as a duplicate; context publication can create an indistinguishable duplicate block ID and later replay cannot deterministically represent both versions. The same design affects assumptions, messages, branches, failures, tools, plugins, and transactions.

Impact: restart can fail ordinary writes or corrupt references; this violates persistent logical identity.

Recommended fix: use durable store-assigned sequences/UUIDs, or initialize generators from recovered maxima under a writer lock and make domain IDs collision-checked. Add reopen-and-append tests for every ID family and concurrent generator tests.

### ORY-AUDIT-004 — Filesystem `append_batch` is not crash-atomic

Severity: S1  
Confidence: CONFIRMED  
Area: event store, crash recovery

Affected:
- `crates/event-store/src/durable.rs:190-212`, `FileEventStore::append_batch`
- `crates/event-store/src/durable.rs:18`, `MAX_FRAME_BYTES`
- child spawn/conflict notification callers in `crates/runtime/src/lib.rs:910-932`, `:1122-1139`, `:1201-1228`

Problem: events are encoded and written as independent frames. There is no batch begin/commit marker or recovery rule that discards an incomplete logical batch. A crash after frame N leaves a valid prefix even though the API and ADRs describe the operation as atomic.

Failure scenario: interrupt a child spawn or assumption-plus-notification write between frames. Reopen accepts the prefix and exposes a child without its budget/relationship/profile, or an assumption without all notifications.

Impact: authoritative projections diverge after a crash; safety checks and manager state can disagree.

Recommended fix: frame batches with a commit record and recover only committed batches, or use a single durable transaction log. Add fault-injection at every write/flush/sync boundary. Also align frame-size validation so writer and opener accept the same maximum.

### ORY-AUDIT-005 — MCP authorization and URL execution use different endpoint representations

Severity: S1  
Confidence: HIGH  
Area: MCP/network capability boundary

Affected:
- `crates/plugin-mcp/src/lib.rs:657`, URL parsing
- `crates/plugin-mcp/src/lib.rs:799-815`, endpoint authorization
- `crates/plugin-mcp/src/lib.rs:852` and `:1428`, request/resource matching

Problem: the lease/manifest check uses the original endpoint text and a lexical prefix matcher, while `Url::parse`/the HTTP client normalizes the URL before sending it. A grant for `/allowed` can accept `/allowed/../private` and send `/private`.

Evidence: the focused local HTTP repro showed the server receiving `POST /private` while the capability was granted only for the `/allowed` endpoint. `open_event_stream` is also public and performs a GET without a capability context or connect-time recheck.

Impact: a configured network capability can be used to reach an unauthorized path; an unconnected caller can open an event stream outside policy.

Recommended fix: normalize and validate the final URL before authorization, compare structured origin/path components, bind endpoint and lease at construction, and require an explicit capability context for every POST/GET/reconnect. Add redirects, dot-segment, encoded-slash, host-case, port, IPv6, and reconnect tests.

### ORY-AUDIT-006 — Durable snapshot codec loses the current model assignment

Severity: S2  
Confidence: HIGH  
Area: snapshots, replay

Affected: `crates/event-store/src/sqlite.rs:883-937` and equivalent filesystem snapshot codec; `RuntimeState` model handling in `crates/event-store/src/lib.rs:1060-1080`.

Problem: snapshot encoding stores `agent.identity.model`, while model selection changes `agent.model` only. A snapshot taken after a model switch serializes the old model; load/prefix validation compares it to the switched projection and rejects the snapshot or recovers stale routing.

Recommended fix: encode both immutable identity metadata and effective model assignment, validate them independently, and add switched-model filesystem/SQLite snapshot tests.

### ORY-AUDIT-007 — Replayed context invalidations are silently dropped from the graph

Severity: S2  
Confidence: CONFIRMED  
Area: context replay/observability

Affected: `crates/context/src/lib.rs:1378-1405`, invalidation transition replay; `:365`, `:615`, `:1020-1021`.

Problem: live invalidation appends to `self.invalidations`, but replay of `Invalidated` updates lifecycle without rebuilding that vector.

Failure scenario: live `invalidation_count`, recent invalidations, and TUI breakpoint context show an invalidation; after restart/recovery the block is invalidated but counts and recent evidence are zero.

Recommended fix: append validated invalidation records during replay, with bounded retention semantics documented and tested.

### ORY-AUDIT-008 — Archived dependencies stop transitive invalidation; restore ignores dependency freshness

Severity: S2  
Confidence: HIGH  
Area: context graph

Affected: `crates/context/src/lib.rs:1244-1275`, `mark_dependents_stale`; archive/restore around `:1082`.

Problem: traversal follows only `Active`/`Stale` blocks. An archived intermediate dependency cuts the graph, so downstream dependents remain active. Restore makes archived content active without checking whether dependencies changed while it was archived.

Recommended fix: traverse dependency edges independently of lifecycle, mark affected downstream blocks stale/invalidated, and require a dependency revision check before restore. Add A→B→C tests with B archived during A supersession.

### ORY-AUDIT-009 — Prompt rendering can include non-active context references

Severity: S2  
Confidence: HIGH  
Area: context projection/privacy

Affected: `crates/context/src/lib.rs:1199-1228`, `render_prompt_with_trust_policy`.

Problem: projection checks principal scope and trust but does not reject archived, invalidated, or superseded references.

Failure scenario: a caller retains an old `ContextRef`; rendering still emits its content after archival/invalidation, defeating lifecycle semantics and potentially exposing stale private material.

Recommended fix: require `Active` (or an explicit caller opt-in for archived inspection), resolve only the current namespace revision, and test lifecycle transitions for private and trust-filtered projections.

### ORY-AUDIT-010 — Budget transfer can mint finite capacity from an unlimited source

Severity: S2  
Confidence: HIGH  
Area: scheduler accounting

Affected: `crates/scheduler/src/lib.rs:817-830`, transfer dimension logic.

Problem: a transfer with `source=None` (unlimited) and `target=Some(x)` is accepted, leaving the source unlimited while creating recipient capacity. ADR-0047 says only configured finite capacity is transferable.

Recommended fix: reject any transfer from an unlimited dimension and test all `None`/`Some` combinations, including replay.

### ORY-AUDIT-011 — Health pressure counters never recover

Severity: S2  
Confidence: HIGH  
Area: scheduler health/supervision

Affected: `crates/scheduler/src/lib.rs:391-460`.

Problem: dependency invalidation, assumption conflict, and verification-failure counters accumulate with no reset/acknowledgement or resolution event. Health derivation can remain `Blocked` after progress, repair, resume, or conflict resolution.

Impact: deterministic supervision can repeatedly pause or promote agents based on obsolete failures.

Recommended fix: model signal instances and explicit resolution/decay transitions, or document monotonic health intentionally; add recovery-after-resolution tests.

### ORY-AUDIT-012 — Cache opt-out/freshness policy still affects tie ordering

Severity: S3  
Confidence: HIGH  
Area: cache-aware routing

Affected: `crates/scheduler/src/lib.rs:127-132` and ranking functions.

Problem: raw cached-token counts remain a sort tie-breaker even when `prefer_warm_cache=false` or evidence is stale. The effective-cost policy ignores savings, but the tie-break can still select a warm/stale candidate unexpectedly.

Recommended fix: make all cache-derived ordering conditional on the policy and freshness result; add equal-cost opt-out and stale-evidence tests.

### ORY-AUDIT-013 — Several runtime event methods append to an unvalidated run

Severity: S2  
Confidence: HIGH  
Area: runtime authority

Affected: `crates/runtime/src/lib.rs:1460-1495`, `record_cache_usage`; similar event-only operations such as model selection/pause/resume/cancellation.

Problem: `record_cache_usage` does not reconstruct/validate that `run_id` exists before appending. The SQLite and in-memory append boundaries accept an event with that run ID, so a phantom run can acquire projection events and fail only on later reconstruction. Event-only lifecycle methods also do not check current model/agent status until the store projection is later rebuilt.

Recommended fix: centralize `require_run` and membership/transition validation before every append, then append and update projections atomically. Add unknown-run and wrong-status tests for every public mutation.

### ORY-AUDIT-014 — Tool input normalization trims user data, and failed verification blocks valid compensation

Severity: S2  
Confidence: CONFIRMED  
Area: transactional tools

Affected:
- `crates/tool-runtime/src/lib.rs:190-207`, proposal normalization
- `crates/tool-runtime/src/lib.rs:966-1017`, execution/verification/compensation

Problem: normalization applies `trim()` to every input value, including content fields where whitespace is data. A verification failure changes the transaction to `Failed`, while compensation accepts only the compensatable execution state and refuses the failed transaction.

Evidence: focused tests showed `"  indented\n"` became `"indented"`; a reversible effect whose verifier failed could not be compensated.

Recommended fix: normalize keys and syntax-only fields; preserve opaque/content values. Track “effect may have occurred” separately from verification state and allow compensation from any state after an attempted reversible effect, with durable compensation failure.

### ORY-AUDIT-015 — Empty executor output leaves a transaction in `Executing`

Severity: S2  
Confidence: HIGH  
Area: tools/reliability

Affected: `crates/tool-runtime/src/lib.rs:930-1000`.

Problem: an injected executor can return empty output without an error; the transaction path does not establish a committed/verified result or a failed terminal state. Recovery then presents an in-flight transaction indefinitely.

Recommended fix: require a typed execution result with explicit effect/verification evidence, reject empty/ambiguous output, and persist a terminal failure/unknown-effect state with compensation guidance.

### ORY-AUDIT-016 — Bare CLI `plan`/`execute` arguments panic instead of returning an error

Severity: S2  
Confidence: CONFIRMED  
Area: CLI robustness

Affected: `crates/cli/src/lib.rs:77`, operation argument slicing before the missing-operation guard around `:98`.

Failure scenario: invoking the command with only `plan` or `execute` indexes `args[2..]` and panics. This violates the documented “unknown/incomplete flag rejection” behavior and gives a non-deterministic crash/exit code.

Recommended fix: validate length before slicing and add process-level exit-code tests for every incomplete command form.

### ORY-AUDIT-017 — Local phrase translation lowercases case-sensitive paths

Severity: S3  
Confidence: CONFIRMED  
Area: terminal CLI

Affected: `crates/cli/src/lib.rs:122-130`, bounded local translation.

Evidence: translating a request containing `README.md` produced `readme.md`. This fails on case-sensitive filesystems and can target a different file.

Recommended fix: use a lowercase copy only for command recognition; preserve captured arguments byte-for-byte except for explicitly typed normalization.

### ORY-AUDIT-018 — Process denylist is incomplete and permits alternate shell interpreters

Severity: S2  
Confidence: HIGH  
Area: terminal process policy

Affected: terminal process operation policy around `crates/terminal-tools/src/lib.rs:1052-1105`.

Problem: blocking a small set of shell names/flags does not deny `powershell.exe`, `pwsh.exe`, `bash.exe`, or `zsh` command interpreters and does not establish argv-level command policy.

Recommended fix: represent process operations as an allowlist of executable identity plus structured arguments; explicitly block interpreter modes and verify the executable at spawn time.

### ORY-AUDIT-019 — Process plugin response pump uses an unbounded queue

Severity: S2  
Confidence: HIGH  
Area: plugin memory bounds

Affected: `crates/plugin-process/src/lib.rs:378-395`.

Problem: the stdout reader sends every line through `std::sync::mpsc::channel()` with no capacity. A child can flood stdout while the caller is idle, growing host memory without bound despite bounded individual frames.

Recommended fix: use a bounded queue with backpressure/child termination, or a single reader that enforces total in-flight bytes and response count. Add a flood test.

### ORY-AUDIT-020 — Process plugin writes can block forever before the request timeout

Severity: S2  
Confidence: HIGH  
Area: plugin cancellation/timeouts

Affected: `crates/plugin-process/src/lib.rs:407-421`, `ProcessSession::write_line`.

Problem: synchronous `write_all`/`flush` happens before the receive timeout is applied. A child that does not drain stdin can make the host block in the write indefinitely.

Recommended fix: bound writes with a dedicated I/O task and cancellation, or use nonblocking/pollable pipes; include a child-not-reading timeout test.

### ORY-AUDIT-021 — Windows process containment is attached after the child starts

Severity: S2  
Confidence: HIGH  
Area: plugin/process security

Affected: `crates/plugin-process/src/lib.rs:356-368`, `:574-588`.

Problem: the process is spawned and only then assigned to a Job Object. A child can create descendants in the gap before assignment.

Recommended fix: use a suspended-create/assign/resume sequence or a platform primitive that atomically starts within the Job; test a fast-spawning descendant.

### ORY-AUDIT-022 — Plugin discovery and WASM activation read unbounded files before checking limits

Severity: S2  
Confidence: HIGH  
Area: plugin resource admission

Affected: `crates/plugin-discovery/src/lib.rs:89`; `crates/plugin-wasm/src/lib.rs:154-156`.

Problem: `fs::read` loads a candidate manifest/module fully before the declared 64 KiB/16 MiB limit is enforced. A hostile file can exhaust memory or disk bandwidth before admission rejects it.

Recommended fix: inspect metadata, stream with a hard byte cap, reject growth/races, and make module parsing consume a bounded reader.

### ORY-AUDIT-023 — Plugin host `max_plugins` is enforced per activation call, not cumulatively

Severity: S3  
Confidence: HIGH  
Area: plugin lifecycle/memory

Affected: `crates/plugin-host/src/lib.rs:212-246`.

Problem: the policy checks `selected.len()` for the current batch, then appends to an already active host. Repeated calls can exceed the configured maximum and retain unbounded adapters.

Recommended fix: enforce `existing + selected <= max_plugins`, define replacement/duplicate semantics, and test repeated activation.

### ORY-AUDIT-024 — Legacy MCP handshake accepts an unsupported server protocol version

Severity: S3  
Confidence: HIGH  
Area: protocol negotiation

Affected: `crates/plugin-mcp/src/lib.rs:528-569`, `:1340-1384`.

Problem: the returned `protocolVersion` is parsed into metadata but the session records the requested mode/version rather than validating negotiated compatibility. A server can claim an unsupported version and still be marked connected.

Recommended fix: validate the server version against an explicit compatibility table and fail closed; preserve negotiated version separately from requested mode.

### ORY-AUDIT-025 — A2A/IPC and ownership metadata do not enforce resource ownership across all effect paths

Severity: S2  
Confidence: HIGH  
Area: architecture/security

Affected:
- `crates/scheduler/src/lib.rs` ownership projection
- terminal tool definitions around `crates/terminal-tools/src/lib.rs:472-534`
- all native/process/plugin/MCP effect adapters

Problem: scheduler ownership is descriptive coordination state. Terminal tool definitions authorize the generic resource `filesystem` rather than concrete source/destination paths, and no effect adapter checks the scheduler owner. An agent that does not own `src/auth/**` can still request a native/terminal path if it holds any applicable lease.

Recommended fix: make ownership a mandatory policy input at every effect boundary, derive concrete resources from typed operations, and test native, plugin, MCP, and shell paths with conflicting owners.

### ORY-AUDIT-026 — Provider implementation does not meet the documented provider boundary

Severity: S3  
Confidence: HIGH  
Area: provider/runtime architecture

Affected: `crates/provider/src/lib.rs:1-220`, `crates/agent/src/lib.rs`.

Problem: the only implementation is a synchronous `Iterator` mock. The contract has no typed tool-call/parallel-call/structured-output/vision/reasoning/provider-extension representation, no async backpressure contract, no retry/rate-limit/timeout policy, and no network adapter. The agent loop owns a full output `String` and does not integrate runtime budgets/events with provider cancellation.

Impact: the current code cannot support the provider capabilities the canonical docs describe and can encourage a later lowest-common-denominator retrofit.

Recommended fix: classify this as an explicit end-goal gap, then design typed request parts, stream events, tool calls, usage/cost, capability negotiation, cancellation, and provider-specific extensions before adding adapters.

### ORY-AUDIT-027 — Filesystem metadata replacement can lose all branch/snapshot metadata

Severity: S2  
Confidence: HIGH  
Area: persistence crash recovery

Affected: `crates/event-store/src/durable.rs:411-438`, `persist_metadata`.

Problem: the committed metadata file is removed before the temporary file is renamed. A crash or rename failure in that window leaves no metadata; `load_metadata` treats a missing sidecar as empty.

Recommended fix: atomic replace without deleting the old file first, retain and validate a backup/generation, and test failures at remove/rename/fsync boundaries.

### ORY-AUDIT-028 — Filesystem sidecar naming collides with event files for some valid paths

Severity: S2  
Confidence: HIGH  
Area: persistence layout

Affected: `crates/event-store/src/durable.rs:304-310`.

Problem: `event_path.with_extension("meta")` is the same path as the event file when the configured event file already has a `.meta` extension. Opening or writing metadata can overwrite the event stream.

Recommended fix: use sibling names with an unambiguous suffix (for example `events.meta` beside `events.log`) and reject/ migrate colliding configurations.

### ORY-AUDIT-029 — Fork remapping can preserve a legacy schema tag with a current payload

Severity: S3  
Confidence: HIGH  
Area: replay/fork compatibility

Affected: `crates/event-store/src/durable.rs:567-615`.

Problem: fork remapping decodes legacy v1 IPC/assumption/tool payloads and re-encodes them with current fields while retaining the original event version tag. The child payload can then be undecodable by the v1 decoder.

Recommended fix: emit the actual encoded schema version after remapping and add a fork test with every supported legacy payload.

### ORY-AUDIT-030 — Event frame size limits differ between writer and opener

Severity: S3  
Confidence: HIGH  
Area: persistence bounds

Affected: `crates/event-store/src/durable.rs:18`, frame writer around `:166`, frame reader around `:87`.

Problem: the writer accepts a `u32`-sized encoded frame while the opener rejects frames above 64 MiB. A valid append can therefore create a store that cannot reopen.

Recommended fix: apply the same maximum before writing, preferably with a much smaller domain-specific payload cap, and test the boundary.

### ORY-AUDIT-031 — Documentation and release gates overstate verified readiness

Severity: S3  
Confidence: CONFIRMED  
Area: docs/CI/testing

Affected: `docs/STATUS.md`, `docs/TESTING.md`, `docs/PERSISTENCE.md`, `.github/workflows/ci.yml`, examples/benches/scripts/test README files.

Problem: status/testing text says the full workspace suite and release-quality evidence pass, while CI runs neither `--all-features` nor release builds, has no Windows/macOS matrix, fuzzing, RSS/startup benchmark, or fault-injection job, and the examples/benchmarks/fixtures/scripts are placeholders. Current validation also cannot execute the full suite under the host’s application-control policy.

Recommended fix: make status claims conditional on recorded CI artifacts, expand CI matrices and feature/release gates, add real examples and benchmark fixtures, and keep environment-blocked checks visibly distinct from passes.

### ORY-AUDIT-032 — Dependency and payload choices undermine the lightweight target

Severity: S4  
Confidence: HIGH  
Area: performance/supply chain

Affected: workspace dependency graph and `crates/event-store/src/sqlite.rs` artifact storage.

Problem: normal dependency resolution contains duplicate `base64` versions; SQLite artifacts store payload bytes inline, and the provider/agent path retains complete response strings. No benchmark measures the stated 25/35/50/100 MB RSS targets.

Recommended fix: remove duplicate versions when compatible, separate large blobs behind the documented content-addressed locator, bound retained outputs, and add repeatable startup/RSS/reconstruction benchmarks before making lightweight claims.

## Security Findings

The S1 findings ORY-AUDIT-001, 002, and 005 are the security release blockers. ORY-AUDIT-018, 019, 020, 021, 022, and 025 are additional capability, process, and resource-boundary weaknesses. Repository files, plugin output, MCP metadata/results, and external messages are typed/bounded in many places, but provenance is not a substitute for canonical path/object authorization or OS isolation.

## Correctness Findings

ORY-AUDIT-003, 006, 007, 008, 009, 013, 014, 015, 016, 017, 027, 028, 029, and 030 affect restart, replay, state transitions, or user-visible command behavior. The strongest tests cover ordinary happy paths; they do not cover process restart allocator state, partial logical batches, malformed lifecycle combinations, or failure after an external effect.

## Concurrency Findings

SQLite writer serialization is a strong baseline. The filesystem adapter is deliberately single-open, but its logical batches are still interruptible. Process plugin stdout and stdin handling are the main unbounded/blocking concurrency hazards (ORY-AUDIT-019 and 020). Multi-agent operations rebuild projections and append events in separate API calls; a future concurrent runtime needs a single compare-and-append/revision boundary to prevent stale projection decisions.

## Event Store / Persistence Findings

The SQLite backend uses `IMMEDIATE` transactions and full synchronous mode, but this does not repair filesystem batch atomicity or metadata replacement. Snapshots need a schema round-trip test after model migration. Event IDs, branch IDs, and sidecar metadata need durable collision and crash tests.

## Context / Cache Findings

Private-scope visibility tests exist and should be retained. The missing replay invalidation log, archived dependency traversal, lifecycle-insensitive prompt rendering, and cache tie-break behavior are the current projection risks. Physical provider KV-cache behavior remains correctly treated as unknown; that is a design strength.

## Agent / Orchestration Findings

Logical agent/model separation and bounded specialist metadata are sound in the current slice. Persistent identity generation, monotonic health pressure, ownership enforcement, and provider-independent model selection remain incomplete. Child creation is batch-validated in memory/SQLite but exposed to filesystem partial-batch recovery.

## Tool Runtime Findings

The typed pipeline and effect-boundary capability rechecks are good foundations. Content trimming, verification/compensation state handling, empty executor results, generic filesystem capability resources, and terminal denylist behavior mean the pipeline cannot yet claim safe reversible effects.

## Provider Findings

Only a deterministic mock provider exists. The documented capability surface is materially ahead of the trait, and there is no measured timeout, retry, malformed-stream, rate-limit, cost, or provider cancellation behavior to audit. This is classified as an end-goal gap (ORY-AUDIT-026), not a fabricated implementation defect.

## Performance / Memory Findings

The process plugin unbounded response queue is the clearest live memory bug. Large pre-admission file reads, full response strings, inline SQLite artifacts, and absent RSS benchmarks prevent the lightweight target from being demonstrated.

## CLI / TUI Findings

The projection-backed inspector is read-only and correctly avoids provider calls. The terminal CLI has a confirmed panic on incomplete operations and case-damaging phrase translation. Full-screen TUI/live controls are explicitly future work and are not counted as bugs.

## Testing Findings

The suite is broad but heavily unit/fixture-oriented. High-value missing tests are: restart ID allocation; fault injection at every filesystem append/metadata rename boundary; canonical path and reparse-point enforcement; quarantine collision and recovery; stale/archived context dependency graphs; scheduler resolution/transfer matrix; process stdout flood and blocked stdin; HTTP URL normalization/redirect/reconnect capability checks; malformed provider streams; and Linux/macOS/Windows CI.

## Documentation vs Implementation

The canonical docs are unusually candid about deferred provider adapters, full OS sandboxing, full WASI, stronger crash semantics, live re-execution, and interactive TUI controls. Those should remain “future” rather than being reported as bugs. The contradictions are the stronger current claims of atomic filesystem batches, successful full-suite/release validation, and complete capability-gated rooted effects, which are contradicted by ORY-AUDIT-001/002/004/027 and the validation restrictions above.

## Incomplete / Placeholder Features

The examples, benchmark README, scripts README, and integration/fixture READMEs explicitly reserve content for future work. Missing MVP/end-goal capabilities include real provider adapters, typed tool-call/structured-output/vision support, live re-execution, full interactive TUI controls, automatic routing/supervision, artifact retention/GC, fuzzing, fault injection, measured RSS/startup, and stronger cross-platform isolation. These are gaps, not current defects, unless a status document claims them complete.

## Cross-Platform Findings

Windows junction/reparse behavior is a confirmed security issue. Process containment timing is Windows-specific and currently has a pre-Job race. Unix symlink/no-follow behavior, macOS process containment, case sensitivity, UNC paths, and signal/terminal restoration have no equivalent acceptance coverage.

## Dependency Findings

The dependency graph is mostly deliberate and the kernel remains lightweight. Duplicate `base64` versions are minor supply-chain/size debt. `reqwest`/TLS/HTTP and Wasmi are substantial dependencies relative to the stated harness target; keep them adapter-isolated and measure their actual contribution before optimizing.

## Missing End-Goal Capabilities

Classification: planned future capability or architectural extension, not a defect in the current slice.

- Real provider adapters with async streams, tool calls, parallel calls, structured output, reasoning/vision, usage/cost, retries, and cancellation.
- A durable, compare-and-append runtime coordinator for concurrent agents and delivery acknowledgements.
- Canonical handle-based filesystem/process/network enforcement and OS sandboxing.
- Full trust/taint propagation across every derived artifact, tool, protocol, and provider input.
- Automatic specialist routing, active supervision, failure classification, context archival policy, and cache expiry adapters.
- Live re-execution/fork/branch controls and a full-screen debugger.
- Artifact retention/GC, cryptographic integrity policy, fault injection, fuzzing, cross-platform release CI, and measured performance evidence.

## Open Questions / Needs Reproduction

- Which operating systems and filesystem APIs are the supported no-follow baseline for rooted effects?
- Should health pressure be monotonic historical evidence or recoverable current health? ADR-0013 and the implementation currently imply both.
- What exact negotiated MCP versions and modern server-request semantics are intended? Current tests exercise behavior that differs from the modern transport contract.
- What is the durable ID/sequence authority across multiple runtime processes?
- What is the intended delivery model for recovered IPC messages when transient mailbox acknowledgements are absent?

## Recommended Fix Order

### Immediate

1. Fix canonical path/object authorization, reparse-point handling, internal quarantine/undo rooting, and unique quarantine records (ORY-AUDIT-001/002/025).
2. Replace process-local IDs with durable collision-safe allocation (ORY-AUDIT-003).
3. Make filesystem batches and metadata replacement crash-atomic; add fault injection (ORY-AUDIT-004/027/028/030).
4. Fix MCP normalized endpoint authorization and capability checks on GET/reconnect (ORY-AUDIT-005).

### Next

1. Correct snapshot model encoding, context replay/invalidation/lifecycle semantics, scheduler transfer/health recovery, and runtime run-membership validation (ORY-AUDIT-006–013).
2. Repair tool state/compensation and CLI panic/process policy (ORY-AUDIT-014–018).
3. Bound plugin queues/writes/admission and close the Windows containment gap (ORY-AUDIT-019–024).

### Then

1. Enforce ownership through every effect adapter.
2. Define provider capability contracts before adding network adapters.
3. Add cross-platform and adversarial tests, then re-run the full suite under a clean CI host.

### Cleanup

Resolve path/sidecar compatibility, duplicate dependency versions, case-preserving parsing, and stale documentation claims (ORY-AUDIT-017, 028, 029, 031, 032).

### Future architecture

Implement the missing end-goal capabilities only after the authority, persistence, and effect-boundary fixes are complete.

## Suggested Remediation Phases

**Phase A — data safety and authority:** canonical object authorization, quarantine/undo redesign, durable IDs, crash-atomic filesystem log and metadata.

**Phase B — replayable runtime correctness:** snapshot schema, context lifecycle/invalidation, scheduler resolution, run membership, compare-and-append.

**Phase C — tool and process boundaries:** effect uncertainty/compensation, typed process allowlists, plugin queue/write bounds, Windows/Unix isolation.

**Phase D — protocol/provider reliability:** MCP negotiation/URL policy, provider capability model, retries, malformed stream and cancellation behavior.

**Phase E — performance and release evidence:** bounded blob/output retention, dependency review, RSS/startup/reconstruction benchmarks, fault injection, fuzzing, CI matrices.

**Phase F — end-goal extensions:** automatic orchestration, live re-execution, interactive debugger, richer adapters, and artifact lifecycle policy.

## Final Validation Checklist

- [ ] Re-run all S1/S2 focused tests after each remediation.
- [ ] Add restart tests for every generated ID family.
- [ ] Add filesystem fault injection for every frame, metadata, artifact, and directory-sync boundary.
- [ ] Test rooted effects against junctions, symlinks, dot segments, encoded separators, case changes, UNC paths, and TOCTOU races.
- [ ] Test quarantine duplicate basenames, concurrent removals, crashes, and undo conflicts.
- [ ] Test context A→B→C invalidation with archived intermediates and recovery.
- [ ] Test scheduler unlimited/finite transfer combinations and health resolution.
- [ ] Test plugin stdout floods, blocked stdin, cumulative activation, bounded file reads, and Windows descendants.
- [ ] Test MCP negotiated versions, normalized URLs, redirects, GET streams, and reconnect capability checks.
- [ ] Add provider malformed-stream, timeout, retry, cost, cancellation, tool-call, and usage tests.
- [ ] Run fmt, check, Clippy, all-features tests, release build, fuzzing, cross-platform CI, and measured RSS/startup benchmarks in a clean host.
