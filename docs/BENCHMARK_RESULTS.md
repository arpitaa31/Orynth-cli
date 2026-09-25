# Phase E/F benchmark results

## Method

Measured on 2026-09-25 in the Windows workspace host, release profile, with
the deterministic mock workload and no API keys. The host is Windows; the
machine also hosted the Phase F TUI measurement. Timings are wall-clock
microseconds, median and p95 over three samples unless noted. RSS is the
Windows process working set sampled by `tasklist`; it includes the benchmark
harness and excludes any model/plugin subprocess because none is launched.
The 10-agent process was held for 100 ms so the retained state was observable.

Commands:

```text
cargo build --release --workspace
target/release/orynth-bench startup --binary target/release/orynth.exe --samples 5
target/release/orynth-bench memory --agents 1 --hold-ms 100
target/release/orynth-bench memory --agents 4 --hold-ms 100
target/release/orynth-bench memory --agents 10 --hold-ms 100
target/release/orynth-bench all --samples 3 --startup-bin target/release/orynth.exe
target/release/orynth-bench faults
target/release/orynth-bench fuzz-smoke --iterations 10000
```

Raw records are retained under `benchmarks/results/`. The full release TSV is
`benchmarks/results/phase-e-all.tsv`; standalone startup and memory captures
are `phase-e-startup.tsv` and `phase-e-memory.txt`.

## Startup and memory

| Scenario | Observed result | Target comparison |
|---|---:|---|
| Release CLI `help`, 5 samples | median 8.597 ms; p95 8.794 ms | measured; no startup target was specified |
| 1 logical waiting agent | 4,932 -> 5,300 KiB | below 50 MB planning target, but harness RSS |
| 4 logical waiting agents | 4,928 -> 5,332 KiB | below 75 MB planning target, but harness RSS |
| 10 logical waiting agents | 4,940 -> 5,408 KiB | below 100 MB planning target, but harness RSS |

These memory values are not production CLI idle claims: the scenario is a
single benchmark process holding event, mailbox, identity, and context state.
The Phase F release TUI working-set samples are recorded below. They are
process observations on this Windows host, not a cross-platform RSS claim.

| TUI scenario | Working set |
|---|---:|
| Empty SQLite database | 6,920 KiB |
| Offline demo, 4 agents | 6,944 KiB |
| Offline demo, 10 agents | 7,064 KiB |

The first frame appeared in the first PTY capture. The manual process harness
has one-second polling resolution, so no more precise first-render timing is
claimed.

## Release workload measurements

| Area / scenario | Size | Median | p95 | Notes |
|---|---:|---:|---:|---|
| Context publish/project/search/render | 100 | 255 us | 255 us | bounded projection and 64-block prompt |
| Context publish/project/search/render | 1,000 | 2,060 us | 2,060 us | same workload |
| Context publish/project/search/render | 10,000 | 21,691 us | 21,691 us | same workload |
| In-memory event append | 1,000 | 338 us | 338 us | 2,048-event batches |
| In-memory event append | 10,000 | 7,534 us | 7,534 us | 2,048-event batches |
| SQLite event append | 1,000 | 26,880 us | 26,880 us | synchronous; 266,240 bytes |
| SQLite event append | 10,000 | 218,993 us | 218,993 us | synchronous; 2,256,896 bytes |
| In-memory reconstruction | 1,000 | 228 us | 228 us | event count checked |
| In-memory reconstruction | 10,000 | 2,823 us | 2,823 us | event count checked |
| In-memory reconstruction | 50,000 | 8,319 us | 8,319 us | event count checked |
| Snapshot save/load | 1,000 | 293 us | 293 us | sequence checked |
| Snapshot save/load | 10,000 | 4,656 us | 4,656 us | sequence checked |
| Snapshot save/load | 50,000 | 22,920 us | 22,920 us | sequence checked |
| Bounded IPC mailbox round trip | 100 | 30 us | 30 us | enqueue/dequeue |
| Bounded IPC mailbox round trip | 1,000 | 212 us | 212 us | enqueue/dequeue |
| Bounded IPC mailbox round trip | 10,000 | 766 us | 766 us | enqueue/dequeue |
| Assumption publication, conflict-heavy | 100 | 456 us | 456 us | 32 normalized subjects |
| Assumption publication, conflict-heavy | 1,000 | 26,836 us | 26,836 us | conflict records retained |
| Assumption publication, conflict-heavy | 10,000 | 2,565,214 us | 2,565,214 us | conflict records dominate |
| Tool proposal repair/validation | 1 | 14 us | 14 us | no external effects |
| Long event run append/reconstruct | 1,000 | 569 us | 569 us | correctness checked |
| Long event run append/reconstruct | 10,000 | 11,519 us | 11,519 us | correctness checked |
| Long event run append/reconstruct | 50,000 | 131,763 us | 131,763 us | correctness checked |

The first assumption run measured 3,486,949 us at 10,000 publications. The
measured remediation added a subject index and reduced that same release
workload to 2,565,214 us (26.4% lower). The remaining cost is the deliberately
conflict-heavy output: 10,000 publications over 32 subjects create a large
number of durable conflict records. No claim is made that this workload is a
normal user distribution.

## Stress and coverage notes

The harness also contains context-churn and agent-churn scenarios at 1,000,
10,000, and 50,000 operations. They were exercised through the debug fallback
after Windows Application Control blocked the freshly rebuilt benchmark
executable; those values are retained in `phase-e-debug-all.tsv` and are not
mixed into the release timing table above.

New defects found while measuring were fixed in the harness: IPC progress
values were constrained to the protocol's 0..=1000 bound, and Windows RSS CSV
parsing now preserves comma-separated working-set values. The assumption
subject index is the only runtime optimization made from the measurements.

## Limitations

- The host is Windows only for this run; Linux and macOS measurements require
  hosted CI or another execution host.
- Windows Application Control (OS error 4551) blocked newly generated release
  benchmark binaries after rebuild and also blocks some generated test
  binaries. This is an environment limitation, not a benchmark pass.
- `cargo fuzz` is not installed on the host. Focused fuzz targets are checked
  into `fuzz/`; the bounded deterministic smoke exercised 30,000 malformed
  decode cases with zero panics.
- Remote provider process RSS, plugin process RSS, sustained multi-hour churn,
  hosted Linux/macOS TUI RSS, and real network-provider latency are not
  measured. TUI working set is measured above for empty, four-agent, and
  ten-agent local scenarios.
- The SQLite file-size readings are workload samples, not a storage-growth
  forecast or a claim about long-term retention.
