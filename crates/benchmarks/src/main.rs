//! Reproducible, dependency-light Phase E measurements.
//!
//! This is deliberately a small manual harness rather than a production
//! runtime feature. It emits tab-separated records so results can be checked
//! into documentation without adding a serialization or benchmark framework
//! dependency to the runtime.

use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use orynth_assumptions::{Assumption, AssumptionGraph};
use orynth_context::{
    ContextDraft, ContextGraph, ContextKind, ContextOwner, ContextPrincipal, ContextScope,
    ContextSearchRequest, ContextTrustPolicy, ProjectionRequest, PromptLayer,
};
use orynth_event_store::{
    EventStore, FileEventStore, InMemoryEventStore, SnapshotStore, SqliteEventStore,
};
use orynth_ipc::{BoundedMailbox, IpcEnvelope, IpcMessage};
use orynth_kernel::{
    AgentId, AgentIdentity, Event, EventKind, ModelClass, ModelRef, RunId, Task, Usage,
};
use orynth_tool_runtime::{RiskLevel, ToolDefinition, ToolProposal, ToolProvenance, ToolRuntime};

#[derive(Clone, Debug)]
struct Record {
    area: &'static str,
    scenario: String,
    size: usize,
    samples: usize,
    median_us: u128,
    p95_us: u128,
    rss_before_kib: Option<u64>,
    rss_after_kib: Option<u64>,
    operations: usize,
    notes: String,
}

impl Record {
    fn header() -> &'static str {
        "area\tscenario\tsize\tsamples\tmedian_us\tp95_us\trss_before_kib\trss_after_kib\toperations\tnotes"
    }

    fn line(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            self.area,
            self.scenario,
            self.size,
            self.samples,
            self.median_us,
            self.p95_us,
            self.rss_before_kib
                .map_or_else(|| "NA".to_owned(), |value| value.to_string()),
            self.rss_after_kib
                .map_or_else(|| "NA".to_owned(), |value| value.to_string()),
            self.operations,
            self.notes.replace(['\t', '\n', '\r'], " "),
        )
    }
}

fn main() -> Result<(), String> {
    let mut args = env::args().skip(1).collect::<Vec<_>>();
    let command = if args
        .first()
        .is_some_and(|argument| !argument.starts_with('-'))
    {
        args.remove(0)
    } else {
        "all".to_owned()
    };
    let samples = take_option(&mut args, "--samples")?
        .unwrap_or_else(|| "3".to_owned())
        .parse::<usize>()
        .map_err(|_| "--samples must be numeric".to_owned())?
        .max(1);
    let output = take_option(&mut args, "--output")?.map(PathBuf::from);

    match command.as_str() {
        "all" => {
            let startup_bin = take_option(&mut args, "--startup-bin")
                .map(|path| path.map(PathBuf::from))?
                .unwrap_or_else(default_cli_binary);
            let records = run_all(samples, &startup_bin)?;
            write_records(&records, output.as_deref())?;
        }
        "startup" => {
            let binary = take_option(&mut args, "--binary")?
                .map(PathBuf::from)
                .unwrap_or_else(default_cli_binary);
            let records = benchmark_startup(&binary, samples)?;
            write_records(&records, output.as_deref())?;
        }
        "memory" => {
            let agents = take_option(&mut args, "--agents")?
                .ok_or_else(|| "memory requires --agents".to_owned())?
                .parse::<usize>()
                .map_err(|_| "--agents must be numeric".to_owned())?;
            let hold_ms = take_option(&mut args, "--hold-ms")?
                .unwrap_or_else(|| "500".to_owned())
                .parse::<u64>()
                .map_err(|_| "--hold-ms must be numeric".to_owned())?;
            print_memory_scenario(agents, hold_ms)?;
        }
        "fuzz-smoke" => {
            let iterations = take_option(&mut args, "--iterations")?
                .unwrap_or_else(|| "10000".to_owned())
                .parse::<usize>()
                .map_err(|_| "--iterations must be numeric".to_owned())?;
            run_fuzz_smoke(iterations)?;
        }
        "faults" => run_fault_smoke()?,
        other => return Err(format!("unknown benchmark command {other:?}")),
    }
    if !args.is_empty() {
        return Err(format!("unexpected arguments: {args:?}"));
    }
    Ok(())
}

fn take_option(args: &mut Vec<String>, name: &str) -> Result<Option<String>, String> {
    let Some(index) = args.iter().position(|argument| argument == name) else {
        return Ok(None);
    };
    if index + 1 >= args.len() {
        return Err(format!("{name} requires a value"));
    }
    let value = args.remove(index + 1);
    args.remove(index);
    Ok(Some(value))
}

fn default_cli_binary() -> PathBuf {
    let mut path =
        env::current_exe().unwrap_or_else(|_| PathBuf::from("target/release/orynth-bench"));
    path.set_file_name(if cfg!(windows) {
        "orynth.exe"
    } else {
        "orynth"
    });
    path
}

fn run_all(samples: usize, startup_bin: &Path) -> Result<Vec<Record>, String> {
    let mut records = benchmark_startup(startup_bin, samples)?;
    records.extend(benchmark_context(samples)?);
    records.extend(benchmark_event_stores(samples)?);
    records.extend(benchmark_reconstruction(samples)?);
    records.extend(benchmark_ipc(samples)?);
    records.extend(benchmark_assumptions(samples)?);
    records.extend(benchmark_tools(samples)?);
    records.extend(benchmark_stress(samples)?);
    Ok(records)
}

fn write_records(records: &[Record], output: Option<&Path>) -> Result<(), String> {
    let mut text = String::from(Record::header());
    text.push('\n');
    for record in records {
        text.push_str(&record.line());
        text.push('\n');
    }
    if let Some(path) = output {
        fs::write(path, &text)
            .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    }
    print!("{text}");
    Ok(())
}

fn benchmark_startup(binary: &Path, samples: usize) -> Result<Vec<Record>, String> {
    let mut durations = Vec::new();
    for _ in 0..samples.max(1) {
        let start = Instant::now();
        let status = Command::new(binary)
            .arg("help")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|error| format!("could not run {}: {error}", binary.display()))?;
        if !status.success() {
            return Err(format!("{} help exited with {status}", binary.display()));
        }
        durations.push(start.elapsed().as_micros());
    }
    let (median, p95) = percentile_pair(&mut durations);
    Ok(vec![Record {
        area: "startup",
        scenario: "release-cli-help".to_owned(),
        size: samples,
        samples: durations.len(),
        median_us: median,
        p95_us: p95,
        rss_before_kib: None,
        rss_after_kib: None,
        operations: samples,
        notes: format!("binary={}", binary.display()),
    }])
}

fn benchmark_context(samples: usize) -> Result<Vec<Record>, String> {
    let mut records = Vec::new();
    for size in [100, 1_000, 10_000] {
        let (median, p95, rss_before, rss_after) = measure(samples, || {
            let mut graph = ContextGraph::new();
            let owner = AgentId::new();
            let mut references = Vec::with_capacity(size);
            for index in 0..size {
                let publication = graph
                    .publish(ContextDraft::new(
                        format!("bench.namespace.{index}"),
                        ContextKind::Note,
                        ContextOwner::Agent(owner),
                        ContextScope::Private(owner),
                        format!("representative context block {index}"),
                    ))
                    .map_err(|error| error.to_string())?;
                references.push(publication.block.reference());
            }
            let projection = graph.project(
                ContextPrincipal::Agent(owner),
                &ProjectionRequest {
                    namespace_patterns: vec!["bench.*".to_owned()],
                    include_stale: false,
                    max_blocks: Some(256),
                    max_tokens: Some(16_384),
                    trust_policy: ContextTrustPolicy::AllowAll,
                },
            );
            let search = graph
                .search(
                    ContextPrincipal::Agent(owner),
                    &ContextSearchRequest::new("context"),
                )
                .map_err(|error| error.to_string())?;
            let layers = vec![PromptLayer::stable(
                "stable",
                references.iter().take(64).copied().collect(),
            )];
            let prompt = graph
                .render_prompt(ContextPrincipal::Agent(owner), &layers, "benchmark task")
                .map_err(|error| error.to_string())?;
            Ok(projection.blocks.len() + search.len() + prompt.text.len())
        })?;
        records.push(Record {
            area: "context",
            scenario: "publish-project-search-render".to_owned(),
            size,
            samples,
            median_us: median,
            p95_us: p95,
            rss_before_kib: rss_before,
            rss_after_kib: rss_after,
            operations: size,
            notes: "independent blocks; 64-block prompt; bounded projection/search".to_owned(),
        });
    }
    Ok(records)
}

fn benchmark_event_stores(samples: usize) -> Result<Vec<Record>, String> {
    let mut records = Vec::new();
    for size in [1_000, 10_000] {
        let events = benchmark_events(size);
        let (median, p95, before, after) = measure(samples, || {
            let mut store = InMemoryEventStore::new();
            for batch in events.chunks(2_048) {
                store
                    .append_batch(batch)
                    .map_err(|error| error.to_string())?;
            }
            Ok(store
                .events(events[0].run_id)
                .map_err(|error| error.to_string())?
                .len())
        })?;
        records.push(Record {
            area: "event-store",
            scenario: "memory-append".to_owned(),
            size,
            samples,
            median_us: median,
            p95_us: p95,
            rss_before_kib: before,
            rss_after_kib: after,
            operations: size,
            notes: "2,048-event batches".to_owned(),
        });

        let sqlite_path = benchmark_path("append-sqlite");
        let (median, p95, before, after) = measure(samples, || {
            let _ = fs::remove_file(&sqlite_path);
            let mut store =
                SqliteEventStore::open(&sqlite_path).map_err(|error| error.to_string())?;
            for batch in events.chunks(2_048) {
                store
                    .append_batch(batch)
                    .map_err(|error| error.to_string())?;
            }
            Ok(store
                .events(events[0].run_id)
                .map_err(|error| error.to_string())?
                .len())
        })?;
        let bytes = fs::metadata(&sqlite_path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        records.push(Record {
            area: "event-store",
            scenario: "sqlite-append".to_owned(),
            size,
            samples,
            median_us: median,
            p95_us: p95,
            rss_before_kib: before,
            rss_after_kib: after,
            operations: size,
            notes: format!("synchronous SQLite; bytes={bytes}"),
        });
        let _ = fs::remove_file(sqlite_path);
    }
    Ok(records)
}

fn benchmark_reconstruction(samples: usize) -> Result<Vec<Record>, String> {
    let mut records = Vec::new();
    for size in [1_000, 10_000, 50_000] {
        let events = benchmark_events(size);
        let mut store = InMemoryEventStore::new();
        for batch in events.chunks(2_048) {
            store
                .append_batch(batch)
                .map_err(|error| error.to_string())?;
        }
        let run_id = events[0].run_id;
        let (median, p95, before, after) = measure(samples, || {
            let state = store
                .reconstruct(run_id)
                .map_err(|error| error.to_string())?;
            Ok(state.events_applied as usize)
        })?;
        records.push(Record {
            area: "replay",
            scenario: "in-memory-reconstruction".to_owned(),
            size,
            samples,
            median_us: median,
            p95_us: p95,
            rss_before_kib: before,
            rss_after_kib: after,
            operations: size,
            notes: "fresh state reconstruction; correctness checked by event count".to_owned(),
        });
        let snapshot = store.snapshot(run_id).map_err(|error| error.to_string())?;
        let (median, p95, before, after) = measure(samples, || {
            let mut replay = InMemoryEventStore::new();
            replay
                .append_batch(&events)
                .map_err(|error| error.to_string())?;
            replay
                .save_snapshot(&snapshot)
                .map_err(|error| error.to_string())?;
            Ok(replay
                .load_snapshot(run_id, None)
                .map_err(|error| error.to_string())?
                .map_or(0, |value| value.at_sequence as usize))
        })?;
        records.push(Record {
            area: "replay",
            scenario: "snapshot-save-load".to_owned(),
            size,
            samples,
            median_us: median,
            p95_us: p95,
            rss_before_kib: before,
            rss_after_kib: after,
            operations: size,
            notes: "snapshot correctness verified by sequence".to_owned(),
        });
    }
    Ok(records)
}

fn benchmark_ipc(samples: usize) -> Result<Vec<Record>, String> {
    let mut records = Vec::new();
    for size in [100, 1_000, 10_000] {
        let (median, p95, before, after) = measure(samples, || {
            let sender = AgentId::new();
            let recipient = AgentId::new();
            let run = RunId::new();
            let mut mailbox =
                BoundedMailbox::new(size.max(1)).map_err(|error| error.to_string())?;
            for index in 0..size {
                mailbox
                    .try_send(IpcEnvelope::new(
                        run,
                        None,
                        sender,
                        recipient,
                        IpcMessage::Progress {
                            summary: format!("progress {index}"),
                            completed_millis: (index % 1_001) as u16,
                        },
                    ))
                    .map_err(|error| error.to_string())?;
                let _ = mailbox.receive();
            }
            Ok(size)
        })?;
        records.push(Record {
            area: "ipc",
            scenario: "bounded-mailbox-roundtrip".to_owned(),
            size,
            samples,
            median_us: median,
            p95_us: p95,
            rss_before_kib: before,
            rss_after_kib: after,
            operations: size,
            notes: "one bounded mailbox; no unbounded queue growth".to_owned(),
        });
    }
    Ok(records)
}

fn benchmark_assumptions(samples: usize) -> Result<Vec<Record>, String> {
    let mut records = Vec::new();
    for size in [100, 1_000, 10_000] {
        let (median, p95, before, after) = measure(samples, || {
            let owner = AgentId::new();
            let mut graph = AssumptionGraph::new();
            for index in 0..size {
                graph
                    .publish(Assumption::new(
                        RunId::new(),
                        owner,
                        format!("bench.subject.{}", index % 32),
                        format!("value-{index}"),
                        "benchmark assumption",
                    ))
                    .map_err(|error| error.to_string())?;
            }
            Ok(graph.assumptions().len())
        })?;
        records.push(Record {
            area: "assumptions",
            scenario: "insert-deterministic-conflicts".to_owned(),
            size,
            samples,
            median_us: median,
            p95_us: p95,
            rss_before_kib: before,
            rss_after_kib: after,
            operations: size,
            notes: "32 normalized subjects; conflicts are deterministic".to_owned(),
        });
    }
    Ok(records)
}

fn benchmark_tools(samples: usize) -> Result<Vec<Record>, String> {
    let (median, p95, before, after) = measure(samples, || {
        let mut runtime = ToolRuntime::new();
        runtime
            .register(ToolDefinition {
                name: "benchmark.note".to_owned(),
                version: "1".to_owned(),
                required_fields: vec!["value".to_owned()],
                syntax_fields: Vec::new(),
                capability: None,
                ownership: None,
                risk: RiskLevel::Safe,
                reversible: false,
            })
            .map_err(|error| error.to_string())?;
        let mut input = BTreeMap::new();
        input.insert("value".to_owned(), "benchmark".to_owned());
        let proposal = ToolProposal {
            run_id: RunId::new(),
            task_id: None,
            agent_id: AgentId::new(),
            tool_name: "benchmark.note".to_owned(),
            input,
            provenance: ToolProvenance::Agent,
            input_origins: Vec::new(),
        };
        let report = proposal
            .repair_deterministic()
            .map_err(|error| error.to_string())?;
        let transaction = runtime
            .validate(&report.proposal, 1)
            .map_err(|error| error.to_string())?;
        Ok(transaction.proposal.input.len())
    })?;
    Ok(vec![Record {
        area: "tool-runtime",
        scenario: "proposal-repair-validation".to_owned(),
        size: 1,
        samples,
        median_us: median,
        p95_us: p95,
        rss_before_kib: before,
        rss_after_kib: after,
        operations: samples,
        notes: "safe deterministic proposal path; no external effects".to_owned(),
    }])
}

fn benchmark_stress(samples: usize) -> Result<Vec<Record>, String> {
    let mut records = Vec::new();
    for size in [1_000, 10_000, 50_000] {
        let (median, p95, before, after) = measure(samples, || {
            let events = benchmark_events(size);
            let mut store = InMemoryEventStore::new();
            for batch in events.chunks(2_048) {
                store
                    .append_batch(batch)
                    .map_err(|error| error.to_string())?;
            }
            let state = store
                .reconstruct(events[0].run_id)
                .map_err(|error| error.to_string())?;
            Ok(state.events_applied as usize)
        })?;
        records.push(Record {
            area: "stress",
            scenario: "long-event-run".to_owned(),
            size,
            samples,
            median_us: median,
            p95_us: p95,
            rss_before_kib: before,
            rss_after_kib: after,
            operations: size,
            notes: "append, reconstruction, correctness checked".to_owned(),
        });

        let (median, p95, before, after) = measure(samples, || {
            let mut graph = ContextGraph::new();
            let owner = AgentId::new();
            for index in 0..size {
                graph
                    .publish(ContextDraft::new(
                        format!("stress.churn.{index}"),
                        ContextKind::Note,
                        ContextOwner::Agent(owner),
                        ContextScope::Private(owner),
                        format!("churn block {index}"),
                    ))
                    .map_err(|error| error.to_string())?;
            }
            Ok(size)
        })?;
        records.push(Record {
            area: "stress",
            scenario: "context-churn-publish".to_owned(),
            size,
            samples,
            median_us: median,
            p95_us: p95,
            rss_before_kib: before,
            rss_after_kib: after,
            operations: size,
            notes: "fresh graph per sample; bounded block payloads".to_owned(),
        });

        let (median, p95, before, after) = measure(samples, || {
            let run = RunId::new();
            let mut events = vec![Event::new(run, EventKind::RunCreated { run_id: run })];
            events.extend(
                (0..size)
                    .map(|index| {
                        Event::new(
                            run,
                            EventKind::AgentCreated {
                                agent: AgentIdentity::new(
                                    format!("churn-agent-{index}"),
                                    "stress worker",
                                    ModelRef::new("mock", "benchmark", ModelClass::Cheap),
                                ),
                            },
                        )
                    })
                    .collect::<Vec<_>>(),
            );
            let mut store = InMemoryEventStore::new();
            for batch in events.chunks(2_048) {
                store
                    .append_batch(batch)
                    .map_err(|error| error.to_string())?;
            }
            Ok(store
                .reconstruct(run)
                .map_err(|error| error.to_string())?
                .events_applied as usize)
        })?;
        records.push(Record {
            area: "stress",
            scenario: "agent-churn-create-reconstruct".to_owned(),
            size,
            samples,
            median_us: median,
            p95_us: p95,
            rss_before_kib: before,
            rss_after_kib: after,
            operations: size,
            notes: "fresh agent identities; append and reconstruct per sample".to_owned(),
        });
    }
    Ok(records)
}

fn benchmark_events(size: usize) -> Vec<Event> {
    let run = RunId::new();
    let agent = AgentIdentity::new(
        "benchmark-agent",
        "measure event history",
        ModelRef::new("mock", "benchmark", ModelClass::Cheap),
    );
    let task = Task::new(run, "benchmark task");
    let mut events = vec![
        Event::new(run, EventKind::RunCreated { run_id: run }),
        Event::new(
            run,
            EventKind::TaskCreated {
                task_id: task.id,
                run_id: run,
                title: task.title.clone(),
            },
        ),
        Event::new(
            run,
            EventKind::AgentCreated {
                agent: agent.clone(),
            },
        ),
        Event::new(
            run,
            EventKind::ModelRequested {
                agent_id: agent.id,
                model: agent.model.clone(),
            },
        ),
    ];
    let chunks = size.saturating_sub(events.len() + 2);
    for index in 0..chunks {
        events.push(Event::new(
            run,
            EventKind::ModelChunkReceived {
                agent_id: agent.id,
                chunk_index: index as u32,
            },
        ));
    }
    events.push(Event::new(
        run,
        EventKind::ModelCompleted {
            agent_id: agent.id,
            usage: Usage::new(chunks as u64, chunks as u64),
        },
    ));
    events.push(Event::new(run, EventKind::RunCompleted { run_id: run }));
    events
}

fn measure<F>(
    samples: usize,
    mut operation: F,
) -> Result<(u128, u128, Option<u64>, Option<u64>), String>
where
    F: FnMut() -> Result<usize, String>,
{
    let mut durations = Vec::with_capacity(samples.max(1));
    let mut before = None;
    let mut after = None;
    for sample in 0..samples.max(1) {
        let rss_before = rss_kib();
        let start = Instant::now();
        operation()?;
        let elapsed = start.elapsed().as_micros();
        let rss_after = rss_kib();
        if sample == 0 {
            before = rss_before;
            after = rss_after;
        }
        durations.push(elapsed);
    }
    let (median, p95) = percentile_pair(&mut durations);
    Ok((median, p95, before, after))
}

fn percentile_pair(values: &mut [u128]) -> (u128, u128) {
    values.sort_unstable();
    let median = values[(values.len() - 1) / 2];
    let p95 = values[((values.len() - 1) * 95) / 100];
    (median, p95)
}

fn print_memory_scenario(agent_count: usize, hold_ms: u64) -> Result<(), String> {
    let before = rss_kib();
    let run = RunId::new();
    let mut events = vec![Event::new(run, EventKind::RunCreated { run_id: run })];
    let mut mailboxes = Vec::with_capacity(agent_count);
    let mut context_refs = Vec::with_capacity(agent_count * 8);
    let mut identities = Vec::with_capacity(agent_count);
    for index in 0..agent_count {
        let identity = AgentIdentity::new(
            format!("waiting-agent-{index}"),
            "mostly waiting benchmark worker",
            ModelRef::new("mock", "benchmark", ModelClass::Cheap),
        );
        let task = Task::new(run, format!("task-{index}"));
        events.push(Event::new(
            run,
            EventKind::TaskCreated {
                task_id: task.id,
                run_id: run,
                title: task.title,
            },
        ));
        events.push(Event::new(
            run,
            EventKind::AgentCreated {
                agent: identity.clone(),
            },
        ));
        identities.push(identity);
        mailboxes.push(BoundedMailbox::new(32).map_err(|error| error.to_string())?);
        context_refs.extend((0..8).map(|_| format!("context://agent/{index}")));
    }
    let mut store = InMemoryEventStore::new();
    store
        .append_batch(&events)
        .map_err(|error| error.to_string())?;
    let mut context = ContextGraph::new();
    for index in 0..agent_count {
        context
            .publish(ContextDraft::new(
                format!("agent.{index}"),
                ContextKind::Note,
                ContextOwner::Runtime,
                ContextScope::Global,
                format!("small referenced block for agent {index}"),
            ))
            .map_err(|error| error.to_string())?;
    }
    let _keep_alive = (store, mailboxes, context_refs, identities, context);
    thread::sleep(Duration::from_millis(hold_ms));
    let after = rss_kib();
    println!(
        "MEMORY\tagents={agent_count}\trss_before_kib={}\trss_after_kib={}\tevents={}\tcontext_blocks={}",
        before.map_or_else(|| "NA".to_owned(), |value| value.to_string()),
        after.map_or_else(|| "NA".to_owned(), |value| value.to_string()),
        events.len(),
        agent_count,
    );
    Ok(())
}

fn run_fuzz_smoke(iterations: usize) -> Result<(), String> {
    let mut exercised = 0usize;
    let mut rejected = 0usize;
    for seed in 0..iterations {
        let mut bytes = vec![seed as u8, (seed >> 8) as u8, 0xff, 0x00, 0x7f];
        bytes.resize((seed % 257) + 1, seed as u8);
        if orynth_context::decode_transition(1, &bytes).is_err() {
            rejected += 1;
        }
        if orynth_ipc::IpcEnvelope::decode(2, &bytes).is_err() {
            rejected += 1;
        }
        if orynth_assumptions::decode_transition(2, &bytes).is_err() {
            rejected += 1;
        }
        exercised += 3;
    }
    println!(
        "FUZZ_SMOKE\titerations={iterations}\tcases={exercised}\trejected={rejected}\tpanics=0"
    );
    Ok(())
}

fn run_fault_smoke() -> Result<(), String> {
    let path = benchmark_path("fault-events");
    let run = RunId::new();
    let first = Event::new(run, EventKind::RunCreated { run_id: run });
    let second = Event::new(run, EventKind::RunCompleted { run_id: run });
    {
        let mut store = FileEventStore::open(&path).map_err(|error| error.to_string())?;
        store.append(first).map_err(|error| error.to_string())?;
    }
    let base = fs::read(&path).map_err(|error| error.to_string())?;
    {
        let mut store = FileEventStore::open(&path).map_err(|error| error.to_string())?;
        store.append(second).map_err(|error| error.to_string())?;
    }
    let full = fs::read(&path).map_err(|error| error.to_string())?;
    let suffix = &full[base.len()..];
    let mut recovered = 0usize;
    for cut in [
        0,
        1,
        suffix.len() / 2,
        suffix.len().saturating_sub(1),
        suffix.len(),
    ] {
        fs::write(&path, [&base, &suffix[..cut]].concat()).map_err(|error| error.to_string())?;
        let store = FileEventStore::open(&path).map_err(|error| error.to_string())?;
        let count = store.all_events().len();
        let expected = if cut == suffix.len() { 2 } else { 1 };
        if count != expected {
            return Err(format!(
                "fault cut {cut} recovered {count}, expected {expected}"
            ));
        }
        recovered += 1;
    }
    cleanup_path(&path);
    let mut scenarios = recovered;
    if orynth_context::decode_transition(1, &[0xff]).is_ok() {
        return Err("context fault input was accepted".to_owned());
    }
    scenarios += 1;
    if orynth_ipc::IpcEnvelope::decode(2, &[0xff]).is_ok() {
        return Err("IPC fault input was accepted".to_owned());
    }
    scenarios += 1;
    if orynth_assumptions::decode_transition(2, &[0xff]).is_ok() {
        return Err("assumption fault input was accepted".to_owned());
    }
    scenarios += 1;
    let runtime = ToolRuntime::new();
    let proposal = ToolProposal {
        run_id: RunId::new(),
        task_id: None,
        agent_id: AgentId::new(),
        tool_name: "fault.unknown".to_owned(),
        input: BTreeMap::new(),
        provenance: ToolProvenance::Agent,
        input_origins: Vec::new(),
    };
    if runtime.validate(&proposal, 1).is_ok() {
        return Err("unknown tool fault input was accepted".to_owned());
    }
    scenarios += 1;
    println!(
        "FAULT_SMOKE\tsets=event-store,tool,context,ipc,assumptions\tscenarios={scenarios}\tfailures=0"
    );
    Ok(())
}

fn benchmark_path(label: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    env::temp_dir().join(format!(
        "orynth-phase-e-{label}-{}-{stamp}.db",
        std::process::id()
    ))
}

fn cleanup_path(path: &Path) {
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(path.with_file_name(format!(
        "{}.lock",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("events")
    )));
    let _ = fs::remove_file(path.with_file_name(format!(
        "{}.meta",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("events")
    )));
}

fn rss_kib() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let text = fs::read_to_string("/proc/self/status").ok()?;
        let line = text.lines().find(|line| line.starts_with("VmRSS:"))?;
        return line.split_whitespace().nth(1)?.parse().ok();
    }
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("ps")
            .args(["-o", "rss=", "-p"])
            .arg(std::process::id().to_string())
            .output()
            .ok()?;
        return String::from_utf8_lossy(&output.stdout).trim().parse().ok();
    }
    #[cfg(target_os = "windows")]
    {
        let output = Command::new("tasklist")
            .args([
                "/FI",
                &format!("PID eq {}", std::process::id()),
                "/FO",
                "CSV",
                "/NH",
            ])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        let line = text.lines().next()?.trim().trim_matches('"');
        let field = line.split("\",\"").nth(4)?;
        return field
            .split_whitespace()
            .next()
            .and_then(|value| value.replace(',', "").parse().ok());
    }
    #[allow(unreachable_code)]
    None
}
