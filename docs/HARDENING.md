# Phase E hardening

## Fault injection

`orynth-bench faults` is a reusable deterministic smoke harness. It writes a
two-event filesystem log, truncates the second append at five suffix points,
and verifies recovery of either one or two committed events. It also feeds
malformed inputs to context, IPC, and assumption decoders and submits an
unknown tool proposal to the validation boundary. The output is
`FAULT_SMOKE ... scenarios=9 failures=0`.

Existing focused suites provide deeper coverage: durable event-store
begin/frame/flush/sync/commit and metadata-generation recovery; tool executor
failure, verification, compensation, and capability recheck; process-plugin
crash/timeout/cancellation; MCP malformed/truncated/oversized wire handling;
context replay and archive/restore; and runtime reconstruction.
Plugin-discovery and MCP-stdio launches remain ENVIRONMENT-BLOCKED by Windows
Application Control (OS error 4551), so they are not reported as passes.

## Fuzz resistance

Focused `cargo-fuzz` targets are checked in for assumption transitions,
context transitions, and IPC envelopes. They are outside the main workspace
and require the external `cargo-fuzz` tool. Because that tool is unavailable
on this host, the bounded smoke runner mutates deterministic byte patterns
across all three decoders for 10,000 iterations (30,000 cases), with 30,000
clean rejections and zero observed panics. Full coverage-guided fuzzing is
still required on a host with cargo-fuzz/libFuzzer available.

## Stress and resource boundaries

Release measurements cover 1k/10k/50k event append/reconstruction runs,
1k/10k/50k snapshot operations, context scales through 10k blocks, bounded
IPC queues through 10k round trips, and conflict-heavy assumptions through
10k publications. The benchmark holds 1/4/10 logical waiting-agent state and
records working-set deltas. Context-churn and agent-churn paths are present
in the harness and were exercised through the debug fallback after the host
blocked the newly rebuilt release binary.

Hard limits remain active at the boundaries: provider/agent output, tool-call
cardinality, event/metadata frames, artifact payloads, context projections,
IPC mailboxes, plugin discovery/activation, process-plugin I/O, and MCP
responses are bounded by their respective crates. The assumption subject
index removes a measured full-graph scan; conflict creation remains explicit
and proportional to the conflicts it records.

## Cross-platform and security regression status

CI is configured for Ubuntu, Windows, and macOS with formatting, all-features
check/Clippy/tests, and release builds. This local run validates only Windows;
hosted matrix results were not available. Existing security regression
coverage was rerun through focused suites for rooted filesystem effects,
capability scoping, ownership/cancellation, secret handles, tool provenance,
plugin admission, MCP trust/protocol bounds, and artifact limits. No new
security regression was found in Phase E.

## Remaining defects and follow-up

No new runtime defect remains open from the Phase E measurement pass. The
following evidence gaps remain intentional: production CLI/TUI RSS isolation,
cross-platform execution, full cargo-fuzz campaigns, real plugin/provider
subprocess memory, sustained long-run churn, and externalized SQLite blobs.
These are limitations or future work, not silently converted into passes.
