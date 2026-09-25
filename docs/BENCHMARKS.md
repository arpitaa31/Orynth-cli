# Benchmarks

Status: Phase E and Phase F measured evidence is recorded in
`docs/BENCHMARK_RESULTS.md`.

The desirable planning targets remain under 25 MB CLI idle, under 35 MB TUI
idle, under 50 MB for one remote agent, under 75 MB for four agents, and under
100 MB for ten mostly waiting agents. They are not treated as acceptance
claims until the production CLI/TUI processes are measured independently of
the benchmark harness and any local model/plugin processes.

The reproducible harness is `crates/benchmarks`, built as `orynth-bench`. It
uses a deterministic mock event/context/tool workload and emits TSV records.
It requires no API keys. Focused fuzz targets are under `fuzz/`; the bounded
fallback smoke runner is `orynth-bench fuzz-smoke`.

Phase E covers release startup, bounded logical-agent memory scenarios,
context projection/search/rendering, memory and SQLite append,
reconstruction, snapshots, IPC mailbox round trips, assumptions, tool
proposal validation, long event runs, fault smoke, and malformed-input smoke.
Phase F adds release TUI working-set measurements for empty, four-agent, and
ten-agent local scenarios. Real provider/plugin process memory, Linux/macOS
execution, sustained multi-hour stress, and full cargo-fuzz execution remain
explicitly limited or deferred as recorded in `docs/BENCHMARK_RESULTS.md` and
`docs/HARDENING.md`.
