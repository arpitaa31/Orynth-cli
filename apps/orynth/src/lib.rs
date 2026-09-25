//! Small command boundary for the Orynth operator app.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, BufRead, Write},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use orynth_assumptions::Assumption;
use orynth_context::{
    ContextDraft, ContextEventLog, ContextGraph, ContextKind, ContextOwner, ContextScope,
};
use orynth_event_store::{
    BranchStore, EventStore, ForkStore, InMemoryEventStore, ReplayMode, SqliteArtifactStore,
    SqliteEventStore, StoredEvent,
};
use orynth_ipc::{IpcEnvelope, IpcMessage, IpcProvenance};
use orynth_kernel::{
    AgentId, AgentIdentity, Event, EventKind, ModelClass, ModelRef, RunId, Task, TaskId,
    ToolTransactionId, TrustOrigin,
};
use orynth_runtime::RuntimeService;
use orynth_scheduler::{BudgetLimits, HealthSignal};
use orynth_security::{CapabilityDomain, CapabilityLease};
use orynth_specialist::SpecialistProfile;
use orynth_tool_runtime::{ToolProvenance, ToolState, ToolTransition};
use orynth_tui::{
    InspectorAction, InspectorPane, InspectorState, RunSummary, TuiDataSource, TuiSnapshot,
    inspector_pane_item_count, render_inspector_pane, render_runtime_inspector, run_fullscreen,
    scan_semantic_breakpoints,
};

const HELP: &str = "Usage:\n  orynth tui [--db <path>] [--run <run-id>]\n  orynth tui --demo\n  orynth inspect --db <path> --run <run-id>\n  orynth replay --db <path> --run <run-id> [--at <sequence>]\n  orynth fork --db <path> --run <run-id> --at <sequence> --child <run-id> [--mode recorded|reexecute|live]\n  orynth diff --db <path> --left <run-id> --right <run-id>\n  orynth debug --db <path> --run <run-id>\n  orynth help";
const MAX_TUI_EVENT_WINDOW: usize = 512;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    Help,
    Inspect {
        database: PathBuf,
        run: u64,
    },
    Replay {
        database: PathBuf,
        run: u64,
        at: Option<u64>,
    },
    Fork {
        database: PathBuf,
        run: u64,
        at: u64,
        child: u64,
        mode: ReplayMode,
    },
    Diff {
        database: PathBuf,
        left: u64,
        right: u64,
    },
    Debug {
        database: PathBuf,
        run: u64,
    },
    Tui {
        database: PathBuf,
        run: Option<u64>,
        demo: bool,
    },
}

pub fn parse_args<I>(args: I) -> Result<Command, String>
where
    I: IntoIterator,
    I::Item: Into<String>,
{
    let mut args = args.into_iter().map(Into::into);
    let Some(command) = args.next() else {
        return Ok(Command::Help);
    };
    if command == "help" || command == "--help" || command == "-h" {
        return Ok(Command::Help);
    }
    if command != "inspect"
        && command != "replay"
        && command != "fork"
        && command != "diff"
        && command != "debug"
        && command != "tui"
    {
        return Err(format!("unknown command {command:?}\n\n{HELP}"));
    }

    let mut database = None;
    let mut run = None;
    let mut at = None;
    let mut child = None;
    let mut mode = ReplayMode::Recorded;
    let mut left = None;
    let mut right = None;
    let mut demo = false;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--db" => {
                database = Some(
                    args.next()
                        .ok_or_else(|| "--db requires a path".to_string())?
                        .into(),
                );
            }
            "--run" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--run requires a numeric run id".to_string())?;
                run = Some(
                    value
                        .parse::<u64>()
                        .map_err(|_| format!("invalid run id {value:?}"))?,
                );
            }
            "--at" if command == "replay" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--at requires a numeric event sequence".to_string())?;
                at = Some(
                    value
                        .parse::<u64>()
                        .map_err(|_| format!("invalid event sequence {value:?}"))?,
                );
            }
            "--at" if command == "fork" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--at requires a numeric fork sequence".to_string())?;
                at = Some(
                    value
                        .parse::<u64>()
                        .map_err(|_| format!("invalid fork sequence {value:?}"))?,
                );
            }
            "--child" if command == "fork" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--child requires a numeric run id".to_string())?;
                child = Some(
                    value
                        .parse::<u64>()
                        .map_err(|_| format!("invalid child run id {value:?}"))?,
                );
            }
            "--mode" if command == "fork" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--mode requires recorded, reexecute, or live".to_string())?;
                mode = parse_replay_mode(&value)?;
            }
            "--left" if command == "diff" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--left requires a numeric run id".to_string())?;
                left = Some(
                    value
                        .parse::<u64>()
                        .map_err(|_| format!("invalid left run id {value:?}"))?,
                );
            }
            "--right" if command == "diff" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--right requires a numeric run id".to_string())?;
                right = Some(
                    value
                        .parse::<u64>()
                        .map_err(|_| format!("invalid right run id {value:?}"))?,
                );
            }
            "--demo" if command == "tui" => demo = true,
            other => return Err(format!("unknown {command} argument {other:?}\n\n{HELP}")),
        }
    }
    if command == "tui" {
        if demo && run.is_some() {
            return Err("tui --demo cannot be combined with --run".to_owned());
        }
        return Ok(Command::Tui {
            database: database.unwrap_or_else(|| PathBuf::from(".orynth/runtime.db")),
            run,
            demo,
        });
    }
    let database = database.ok_or_else(|| format!("{command} requires --db\n\n{HELP}"))?;
    if command == "diff" {
        return Ok(Command::Diff {
            database,
            left: left.ok_or_else(|| format!("diff requires --left\n\n{HELP}"))?,
            right: right.ok_or_else(|| format!("diff requires --right\n\n{HELP}"))?,
        });
    }
    if command == "debug" {
        return Ok(Command::Debug {
            database,
            run: run.ok_or_else(|| format!("debug requires --run\n\n{HELP}"))?,
        });
    }
    let run = run.ok_or_else(|| format!("{command} requires --run\n\n{HELP}"))?;
    if command == "inspect" && at.is_some() {
        return Err("--at is only valid with replay".to_string());
    }
    if command == "inspect" {
        Ok(Command::Inspect { database, run })
    } else if command == "replay" {
        Ok(Command::Replay { database, run, at })
    } else {
        Ok(Command::Fork {
            database,
            run,
            at: at.ok_or_else(|| format!("fork requires --at\n\n{HELP}"))?,
            child: child.ok_or_else(|| format!("fork requires --child\n\n{HELP}"))?,
            mode,
        })
    }
}

fn parse_replay_mode(value: &str) -> Result<ReplayMode, String> {
    match value {
        "recorded" => Ok(ReplayMode::Recorded),
        "reexecute" => Ok(ReplayMode::ReexecuteLive),
        "live" => Ok(ReplayMode::ForkLive),
        other => Err(format!(
            "invalid replay mode {other:?}; expected recorded, reexecute, or live"
        )),
    }
}

pub fn run<I>(args: I) -> Result<Option<String>, String>
where
    I: IntoIterator,
    I::Item: Into<String>,
{
    match parse_args(args)? {
        Command::Help => Ok(Some(HELP.to_string())),
        Command::Inspect { database, run } => inspect(database, run).map(Some),
        Command::Replay { database, run, at } => replay(database, run, at).map(Some),
        Command::Fork {
            database,
            run,
            at,
            child,
            mode,
        } => fork(database, run, at, child, mode).map(Some),
        Command::Diff {
            database,
            left,
            right,
        } => diff(database, left, right).map(Some),
        Command::Debug { database, run } => {
            debug_terminal(database, run)?;
            Ok(None)
        }
        Command::Tui {
            database,
            run,
            demo,
        } => {
            run_tui(database, run, demo)?;
            Ok(None)
        }
    }
}

fn recover_recorded_prefix(
    database: &PathBuf,
    run: u64,
    at: Option<u64>,
) -> Result<orynth_runtime::RecoveredRun, String> {
    let store = SqliteEventStore::open(database)
        .map_err(|error| format!("could not open {}: {error}", database.display()))?;
    let run_id = RunId::from_u64(run);
    let all_events = store
        .events(run_id)
        .map_err(|error| format!("could not read run {run}: {error}"))?;
    let prefix = all_events
        .iter()
        .filter(|stored| at.is_none_or(|sequence| stored.sequence <= sequence))
        .cloned()
        .collect::<Vec<_>>();
    if prefix.is_empty() {
        return Err(match at {
            Some(sequence) => {
                format!("run {run} has no recorded event at or before sequence {sequence}")
            }
            None => format!("run {run} is not present"),
        });
    }
    let events = prefix
        .iter()
        .map(|stored| stored.event.clone())
        .collect::<Vec<_>>();
    let mut prefix_store = InMemoryEventStore::new();
    prefix_store
        .append_batch(&events)
        .map_err(|error| format!("could not stage recorded prefix for run {run}: {error}"))?;
    drop(store);
    let artifacts = SqliteArtifactStore::open(database).map_err(|error| {
        format!(
            "could not open artifacts in {}: {error}",
            database.display()
        )
    })?;
    let mut recovered = RuntimeService::new(prefix_store)
        .recover_with_artifact_store(run_id, &artifacts)
        .map_err(|error| format!("could not recover recorded run {run}: {error}"))?;
    recovered.events = prefix;
    Ok(recovered)
}

struct DbTuiSource {
    database: PathBuf,
    selected: Option<RunId>,
}

impl DbTuiSource {
    fn new(database: PathBuf, selected: Option<u64>) -> Self {
        Self {
            database,
            selected: selected.map(RunId::from_u64),
        }
    }
}

impl TuiDataSource for DbTuiSource {
    fn snapshot(&mut self) -> Result<TuiSnapshot, String> {
        let store = SqliteEventStore::open(&self.database)
            .map_err(|error| format!("could not open {}: {error}", self.database.display()))?;
        let run_ids = store
            .run_ids()
            .map_err(|error| format!("could not list persisted runs: {error}"))?;
        let mut runs = Vec::with_capacity(run_ids.len());
        for run_id in run_ids {
            let events = store
                .events(run_id)
                .map_err(|error| format!("could not read run {run_id}: {error}"))?;
            let status = events
                .last()
                .map(|event| match event.event.kind {
                    EventKind::RunCompleted { .. } => "COMPLETED",
                    EventKind::RunCancelled { .. } => "CANCELLED",
                    EventKind::RunFailed { .. } => "FAILED",
                    _ => "ACTIVE",
                })
                .unwrap_or("EMPTY")
                .to_owned();
            runs.push(RunSummary {
                run_id,
                status,
                event_count: events.len(),
            });
        }
        let selected = if let Some(run_id) = self.selected {
            let events = store
                .events(run_id)
                .map_err(|error| format!("could not read selected run {run_id}: {error}"))?;
            if events.is_empty() {
                return Err(format!("selected run {run_id} has no events"));
            }
            let values = events
                .iter()
                .map(|stored| stored.event.clone())
                .collect::<Vec<_>>();
            let mut prefix = InMemoryEventStore::new();
            prefix
                .append_batch(&values)
                .map_err(|error| format!("could not stage selected run {run_id}: {error}"))?;
            Some(bound_tui_history(
                RuntimeService::new(prefix)
                    .recover(run_id)
                    .map_err(|error| format!("could not recover selected run {run_id}: {error}"))?,
            ))
        } else {
            None
        };
        Ok(TuiSnapshot { selected, runs })
    }

    fn select_run(&mut self, run_id: RunId) -> Result<(), String> {
        self.selected = Some(run_id);
        Ok(())
    }

    fn event_page(
        &mut self,
        before_sequence: Option<u64>,
        limit: usize,
    ) -> Result<Vec<StoredEvent>, String> {
        let run_id = self
            .selected
            .ok_or_else(|| "no run selected for event paging".to_owned())?;
        let store = SqliteEventStore::open(&self.database)
            .map_err(|error| format!("could not open {}: {error}", self.database.display()))?;
        let before = before_sequence.unwrap_or(u64::MAX);
        let mut page = store
            .events(run_id)
            .map_err(|error| format!("could not read event page for {run_id}: {error}"))?
            .into_iter()
            .filter(|event| event.sequence < before)
            .rev()
            .take(limit)
            .collect::<Vec<_>>();
        page.reverse();
        Ok(page)
    }
}

struct DemoTuiSource {
    snapshot: TuiSnapshot,
}

impl DemoTuiSource {
    fn new(agent_count: usize) -> Result<Self, String> {
        Ok(Self {
            snapshot: if agent_count == 4 {
                demo_snapshot()?
            } else {
                demo_snapshot_with_agent_count(agent_count)?
            },
        })
    }
}

impl TuiDataSource for DemoTuiSource {
    fn snapshot(&mut self) -> Result<TuiSnapshot, String> {
        Ok(self.snapshot.clone())
    }

    fn select_run(&mut self, _run_id: RunId) -> Result<(), String> {
        Ok(())
    }
}

fn run_tui(database: PathBuf, run: Option<u64>, demo: bool) -> Result<(), String> {
    if demo {
        let agent_count = std::env::var("ORYNTH_TUI_DEMO_AGENTS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|count| (1..=64).contains(count))
            .unwrap_or(4);
        run_fullscreen(DemoTuiSource::new(agent_count)?)
    } else {
        run_fullscreen(DbTuiSource::new(database, run))
    }
}

fn bound_tui_history(mut recovered: orynth_runtime::RecoveredRun) -> orynth_runtime::RecoveredRun {
    if recovered.events.len() > MAX_TUI_EVENT_WINDOW {
        let start = recovered.events.len() - MAX_TUI_EVENT_WINDOW;
        recovered.events = recovered.events.split_off(start);
    }
    recovered
}

fn demo_snapshot() -> Result<TuiSnapshot, String> {
    demo_snapshot_with_agent_count(4)
}

fn demo_snapshot_with_agent_count(agent_count: usize) -> Result<TuiSnapshot, String> {
    let run_id = RunId::from_u64(0xD3E0_0001);
    let task = Task {
        id: TaskId::from_u64(0xD3E0_0010),
        run_id,
        title: "Investigate authentication migration".to_owned(),
    };
    let manager = demo_agent(
        0xD3E0_0100,
        "MANAGER",
        "Coordinate the migration and review specialist evidence",
        "strong-reasoner-v1",
        ModelClass::Strong,
    );
    let mut service = RuntimeService::new(InMemoryEventStore::new());
    service
        .event_store_mut()
        .append_batch(&[
            Event::new(run_id, EventKind::RunCreated { run_id }),
            Event::new(
                run_id,
                EventKind::TaskCreated {
                    task_id: task.id,
                    run_id,
                    title: task.title.clone(),
                },
            ),
            Event::new(
                run_id,
                EventKind::AgentCreated {
                    agent: manager.clone(),
                },
            ),
            Event::new(
                run_id,
                EventKind::ModelRequested {
                    agent_id: manager.id,
                    model: manager.model.clone(),
                },
            ),
        ])
        .map_err(|error| format!("demo core events failed: {error}"))?;

    service
        .configure_budget(
            run_id,
            manager.id,
            BudgetLimits {
                max_tokens: Some(50_000),
                max_child_agents: Some(agent_count as u64),
                max_context_tokens: Some(16_384),
                ..BudgetLimits::default()
            },
        )
        .map_err(|error| format!("demo manager budget failed: {error}"))?;

    let auth = demo_agent(
        0xD3E0_0101,
        "AUTH-01",
        "Validate identity and token schema compatibility",
        "cheap-coder-v2",
        ModelClass::Cheap,
    );
    let db = demo_agent(
        0xD3E0_0102,
        "DB-02",
        "Check migration constraints and persisted identifiers",
        "cheap-coder-v2",
        ModelClass::Cheap,
    );
    let sec = demo_agent(
        0xD3E0_0103,
        "SEC-03",
        "Review capability and ownership boundaries",
        "local-reviewer",
        ModelClass::Local,
    );
    let mut specialists = vec![
        (
            auth.clone(),
            "Authentication specialist",
            vec!["src/auth/**".to_owned()],
            vec!["filesystem.read".to_owned()],
            true,
        ),
        (
            db.clone(),
            "Database specialist",
            vec!["migrations/**".to_owned()],
            vec!["filesystem.read".to_owned()],
            false,
        ),
        (
            sec.clone(),
            "Security reviewer",
            vec!["src/**".to_owned()],
            vec!["policy.inspect".to_owned()],
            false,
        ),
    ];
    while specialists.len() < agent_count.saturating_sub(1) {
        let index = specialists.len() + 1;
        let agent = demo_agent(
            0xD3E0_0100 + index as u64,
            &format!("WORK-{index:02}"),
            "Inspect a bounded migration workstream",
            "cheap-coder-v2",
            ModelClass::Cheap,
        );
        specialists.push((
            agent,
            "Migration specialist",
            vec![format!("workstream-{index}/**")],
            vec!["filesystem.read".to_owned()],
            false,
        ));
    }
    for (agent, role, scope, capabilities, promotable) in specialists {
        service
            .spawn_specialist(
                run_id,
                manager.id,
                agent.clone(),
                SpecialistProfile::new(agent.id, role)
                    .with_scope(scope)
                    .with_subscriptions(vec!["assumption.*".to_owned(), "tool.*".to_owned()])
                    .with_capabilities(capabilities)
                    .promotable(promotable),
            )
            .map_err(|error| format!("demo specialist failed: {error}"))?;
        service
            .configure_budget(
                run_id,
                agent.id,
                BudgetLimits {
                    max_tokens: Some(12_000),
                    max_context_tokens: Some(4_096),
                    ..BudgetLimits::default()
                },
            )
            .map_err(|error| format!("demo specialist budget failed: {error}"))?;
    }

    service
        .select_model(
            run_id,
            auth.id,
            ModelRef::new("mock", "strong-reasoner-v1", ModelClass::Strong),
        )
        .map_err(|error| format!("demo model switch failed: {error}"))?;
    service
        .select_model(run_id, db.id, db.model.clone())
        .map_err(|error| format!("demo database model assignment failed: {error}"))?;
    service
        .select_model(run_id, sec.id, sec.model.clone())
        .map_err(|error| format!("demo security model assignment failed: {error}"))?;
    service
        .record_agent_usage(
            run_id,
            auth.id,
            orynth_scheduler::BudgetUsage {
                tokens: 2_340,
                context_tokens: 640,
                ..orynth_scheduler::BudgetUsage::default()
            },
        )
        .map_err(|error| format!("demo usage failed: {error}"))?;
    service
        .claim_ownership(run_id, auth.id, "src/auth")
        .map_err(|error| format!("demo ownership failed: {error}"))?;
    service
        .grant_capability(
            run_id,
            CapabilityLease {
                agent_id: auth.id,
                task_id: Some(task.id),
                domain: CapabilityDomain::Filesystem,
                resource: "src/auth".to_owned(),
                expires_at_ms: u128::MAX,
            },
        )
        .map_err(|error| format!("demo capability failed: {error}"))?;

    let mut context = ContextGraph::new();
    let publication = context
        .publish(ContextDraft::new(
            "auth.schema",
            ContextKind::Contract,
            ContextOwner::Agent(auth.id),
            ContextScope::Private(auth.id),
            "users.id must remain UUID during the migration.",
        ))
        .map_err(|error| format!("demo context failed: {error}"))?;
    let context_events = ContextEventLog::from_transitions(publication.transitions)
        .to_events(run_id)
        .map_err(|error| format!("demo context encoding failed: {error}"))?;
    service
        .event_store_mut()
        .append_batch(&context_events)
        .map_err(|error| format!("demo context persistence failed: {error}"))?;

    service
        .publish_assumption(Assumption::new(
            run_id,
            auth.id,
            "users.id",
            "UUID",
            "authentication tokens encode the identifier as UUID",
        ))
        .map_err(|error| format!("demo first assumption failed: {error}"))?;
    service
        .publish_assumption(Assumption::new(
            run_id,
            db.id,
            "users.id",
            "BIGINT",
            "legacy database schema uses numeric identifiers",
        ))
        .map_err(|error| format!("demo conflict assumption failed: {error}"))?;
    service
        .record_health_signal(run_id, auth.id, HealthSignal::AssumptionConflict)
        .map_err(|error| format!("demo health signal failed: {error}"))?;

    service
        .send_message(
            IpcEnvelope::new(
                run_id,
                Some(task.id),
                manager.id,
                auth.id,
                IpcMessage::Question {
                    subject: "identifier migration".to_owned(),
                    why: "confirm the conflict resolution evidence".to_owned(),
                },
            )
            .with_provenance(IpcProvenance::Runtime),
        )
        .map_err(|error| format!("demo IPC failed: {error}"))?;

    let mut input = BTreeMap::new();
    input.insert("path".to_owned(), "src/auth/schema.rs".to_owned());
    let transaction_id = ToolTransactionId::from_u64(0xD3E0_0200);
    let proposal = orynth_tool_runtime::ToolProposal {
        run_id,
        task_id: Some(task.id),
        agent_id: auth.id,
        tool_name: "filesystem.inspect".to_owned(),
        input,
        provenance: ToolProvenance::Agent,
        input_origins: vec![TrustOrigin::Generated],
    };
    service
        .record_tool_transition(
            run_id,
            ToolTransition::Proposed {
                transaction_id,
                proposal,
                state: ToolState::AwaitingApproval,
            },
        )
        .map_err(|error| format!("demo tool proposal failed: {error}"))?;
    service
        .record_tool_transition(
            run_id,
            ToolTransition::StateChanged {
                transaction_id,
                state: ToolState::Verified,
                detail: Some("read-only schema inspection verified".to_owned()),
            },
        )
        .map_err(|error| format!("demo tool verification failed: {error}"))?;
    service
        .event_store_mut()
        .append(Event::new(
            run_id,
            EventKind::CacheObserved {
                provider: "mock".to_owned(),
                model: "strong-reasoner-v1".to_owned(),
                prefix_hash: [7; 32],
                estimated_prefix_tokens: 640,
                cached_input_tokens: 512,
            },
        ))
        .map_err(|error| format!("demo cache observation failed: {error}"))?;
    service
        .pause_agent(run_id, sec.id)
        .map_err(|error| format!("demo pause failed: {error}"))?;

    let recovered = service
        .recover(run_id)
        .map_err(|error| format!("demo recovery failed: {error}"))?;
    Ok(TuiSnapshot {
        selected: Some(recovered),
        runs: vec![RunSummary {
            run_id,
            status: "ACTIVE".to_owned(),
            event_count: service
                .event_store()
                .events(run_id)
                .map_err(|error| format!("demo event count failed: {error}"))?
                .len(),
        }],
    })
}

fn demo_agent(id: u64, name: &str, mission: &str, model: &str, class: ModelClass) -> AgentIdentity {
    let mut agent = AgentIdentity::new("demo", mission, ModelRef::new("mock", model, class));
    agent.id = AgentId::from_u64(id);
    agent.name = name.to_owned();
    agent
}

pub fn inspect(database: PathBuf, run: u64) -> Result<String, String> {
    let recovered = recover_recorded_prefix(&database, run, None)?;
    Ok(render_runtime_inspector(&recovered))
}

pub fn replay(database: PathBuf, run: u64, at: Option<u64>) -> Result<String, String> {
    let recovered = recover_recorded_prefix(&database, run, at)?;
    let mut output = String::from("Recorded replay (provider calls: 0)\n");
    output.push_str(&render_runtime_inspector(&recovered));
    Ok(output)
}

pub fn fork(
    database: PathBuf,
    run: u64,
    at: u64,
    child: u64,
    mode: ReplayMode,
) -> Result<String, String> {
    if run == 0 || child == 0 {
        return Err("parent and child run ids must be non-zero".to_string());
    }
    let mut store = SqliteEventStore::open(&database)
        .map_err(|error| format!("could not open {}: {error}", database.display()))?;
    let parent_run_id = RunId::from_u64(run);
    let branch = store
        .create_branch(
            parent_run_id,
            at,
            mode,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        )
        .map_err(|error| format!("could not create fork boundary: {error}"))?;
    let materialized = store
        .materialize_fork(branch.branch_id, RunId::from_u64(child))
        .map_err(|error| format!("could not materialize fork: {error}"))?;
    Ok(format!(
        "Fork materialized\nBranch: {}\nParent: {} at sequence {}\nChild: {}\nMode: {:?}\nCopied events: {}",
        materialized.branch_id,
        materialized.parent_run_id,
        materialized.fork_sequence,
        materialized.child_run_id,
        materialized.replay_mode,
        materialized.copied_event_count
    ))
}

pub fn diff(database: PathBuf, left: u64, right: u64) -> Result<String, String> {
    let left_run = recover_recorded_prefix(&database, left, None)?;
    let right_run = recover_recorded_prefix(&database, right, None)?;
    let mut differences = Vec::new();
    let scalar_fields = [
        (
            "status",
            format!("{:?}", left_run.state.status),
            format!("{:?}", right_run.state.status),
        ),
        (
            "events",
            left_run.events.len().to_string(),
            right_run.events.len().to_string(),
        ),
        (
            "tasks",
            left_run.state.tasks.len().to_string(),
            right_run.state.tasks.len().to_string(),
        ),
        (
            "agents",
            left_run.state.agents.len().to_string(),
            right_run.state.agents.len().to_string(),
        ),
        (
            "artifacts",
            left_run.state.artifacts.len().to_string(),
            right_run.state.artifacts.len().to_string(),
        ),
        (
            "context blocks",
            left_run.context.block_count().to_string(),
            right_run.context.block_count().to_string(),
        ),
        (
            "context invalidations",
            left_run.context.invalidation_count().to_string(),
            right_run.context.invalidation_count().to_string(),
        ),
        (
            "assumptions",
            left_run.assumptions.assumptions().len().to_string(),
            right_run.assumptions.assumptions().len().to_string(),
        ),
        (
            "conflicts",
            left_run.assumptions.conflicts().len().to_string(),
            right_run.assumptions.conflicts().len().to_string(),
        ),
        (
            "tools",
            left_run.tools.records().len().to_string(),
            right_run.tools.records().len().to_string(),
        ),
        (
            "capabilities",
            left_run.capabilities.leases().len().to_string(),
            right_run.capabilities.leases().len().to_string(),
        ),
        (
            "cache records",
            left_run.cache_telemetry.len().to_string(),
            right_run.cache_telemetry.len().to_string(),
        ),
        (
            "failure records",
            left_run.failures.records().len().to_string(),
            right_run.failures.records().len().to_string(),
        ),
    ];
    for (field, left_value, right_value) in scalar_fields {
        if left_value != right_value {
            differences.push(format!("  {field}: {left_value} -> {right_value}"));
        }
    }

    let mut agent_ids = BTreeSet::new();
    agent_ids.extend(left_run.manager.agents.keys().copied());
    agent_ids.extend(right_run.manager.agents.keys().copied());
    for agent_id in agent_ids {
        let left_agent = left_run.manager.agents.get(&agent_id);
        let right_agent = right_run.manager.agents.get(&agent_id);
        let left_value = left_agent.map(|agent| {
            format!(
                "{}/{}/ {:?}",
                agent.model.provider, agent.model.model, agent.status
            )
        });
        let right_value = right_agent.map(|agent| {
            format!(
                "{}/{}/ {:?}",
                agent.model.provider, agent.model.model, agent.status
            )
        });
        if left_value != right_value {
            differences.push(format!(
                "  agent {agent_id}: {} -> {}",
                left_value.unwrap_or_else(|| "<absent>".to_owned()),
                right_value.unwrap_or_else(|| "<absent>".to_owned())
            ));
        }
    }

    let mut output = format!(
        "Recorded projection diff (provider calls: 0)\nLeft: {}\nRight: {}\n",
        left_run.run_id, right_run.run_id
    );
    if differences.is_empty() {
        output.push_str("No differing recovered projection fields.\n");
    } else {
        output.push_str("Differences:\n");
        output.push_str(&differences.join("\n"));
        output.push('\n');
    }
    Ok(output)
}

const DEBUG_HELP: &str = "Commands: show | pane <overview|agents|events|breakpoints> | next | prev | select <index> | events | event <sequence> | breakpoints | help | quit";

fn bounded_debug_detail(detail: String) -> String {
    let mut chars = detail.chars();
    let bounded = chars.by_ref().take(512).collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}...")
    } else {
        bounded
    }
}

/// Run a small line-oriented debugger over a recovered projection.
///
/// The session is intentionally command-driven and terminal-independent. It
/// offers selection and inspection without adding a second state authority or
/// implying that debug commands mutate a live run.
pub fn run_debug_session<R: BufRead, W: Write>(
    recovered: &orynth_runtime::RecoveredRun,
    input: R,
    mut output: W,
) -> io::Result<()> {
    let mut state = InspectorState::new();
    writeln!(
        output,
        "Orynth debug session for {} (read-only)",
        recovered.run_id
    )?;
    writeln!(output, "{DEBUG_HELP}")?;
    write!(output, "debug> ")?;
    output.flush()?;
    for line in input.lines() {
        let line = line?;
        let command = line.trim();
        match command {
            "" => {}
            "help" => writeln!(output, "{DEBUG_HELP}")?,
            "show" => write!(output, "{}", render_inspector_pane(recovered, state))?,
            "next" => {
                let item_count = inspector_pane_item_count(recovered, state.pane());
                state.apply(InspectorAction::NextItem, item_count);
                write!(output, "{}", render_inspector_pane(recovered, state))?;
            }
            "prev" => {
                let item_count = inspector_pane_item_count(recovered, state.pane());
                state.apply(InspectorAction::PreviousItem, item_count);
                write!(output, "{}", render_inspector_pane(recovered, state))?;
            }
            "events" => {
                for event in &recovered.events {
                    writeln!(
                        output,
                        "#{} {}",
                        event.sequence,
                        bounded_debug_detail(format!("{:?}", event.event.kind))
                    )?;
                }
            }
            "breakpoints" => match scan_semantic_breakpoints(recovered) {
                Ok(breakpoints) if breakpoints.is_empty() => {
                    writeln!(output, "No semantic breakpoint hits.")?
                }
                Ok(breakpoints) => {
                    for breakpoint in breakpoints {
                        writeln!(
                            output,
                            "#{} {} {}",
                            breakpoint.sequence, breakpoint.kind, breakpoint.detail
                        )?;
                    }
                }
                Err(error) => writeln!(output, "Breakpoint scan unavailable: {error}")?,
            },
            "quit" | "exit" => {
                writeln!(output, "Leaving debug session.")?;
                break;
            }
            command if command.starts_with("pane ") => {
                let name = command.trim_start_matches("pane ").trim();
                match parse_debug_pane(name) {
                    Ok(pane) => {
                        state.set_pane(pane, inspector_pane_item_count(recovered, pane));
                        write!(output, "{}", render_inspector_pane(recovered, state))?;
                    }
                    Err(error) => writeln!(output, "{error}. {DEBUG_HELP}")?,
                }
            }
            command if command.starts_with("select ") => {
                let value = command.trim_start_matches("select ").trim();
                match value.parse::<usize>() {
                    Ok(index) => {
                        let item_count = inspector_pane_item_count(recovered, state.pane());
                        state.apply(InspectorAction::Select(index), item_count);
                        write!(output, "{}", render_inspector_pane(recovered, state))?;
                    }
                    Err(_) => writeln!(output, "select requires a numeric index.")?,
                }
            }
            command if command.starts_with("event ") => {
                let value = command.trim_start_matches("event ").trim();
                match value.parse::<u64>() {
                    Ok(sequence) => match recovered
                        .events
                        .iter()
                        .find(|event| event.sequence == sequence)
                    {
                        Some(event) => writeln!(
                            output,
                            "#{} {}",
                            event.sequence,
                            bounded_debug_detail(format!("{:?}", event.event.kind))
                        )?,
                        None => writeln!(output, "No event at sequence {sequence}.")?,
                    },
                    Err(_) => writeln!(output, "event requires a numeric sequence.")?,
                }
            }
            other => writeln!(output, "Unknown debug command {other:?}. {DEBUG_HELP}")?,
        }
        write!(output, "debug> ")?;
        output.flush()?;
    }
    Ok(())
}

fn parse_debug_pane(value: &str) -> Result<InspectorPane, String> {
    match value {
        "overview" => Ok(InspectorPane::Overview),
        "agents" => Ok(InspectorPane::Agents),
        "events" => Ok(InspectorPane::Events),
        "breakpoints" => Ok(InspectorPane::Breakpoints),
        other => Err(format!("unknown pane {other:?}")),
    }
}

fn debug_terminal(database: PathBuf, run: u64) -> Result<(), String> {
    let recovered = recover_recorded_prefix(&database, run, None)?;
    let stdin = io::stdin();
    let stdout = io::stdout();
    run_debug_session(&recovered, stdin.lock(), stdout.lock())
        .map_err(|error| format!("debug session failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use orynth_event_store::EventStore;
    use orynth_kernel::{Event, EventKind};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parser_requires_explicit_inspect_inputs() {
        assert!(parse_args(["inspect", "--db", "run.db"]).is_err());
        assert!(parse_args(["inspect", "--run", "1"]).is_err());
        assert_eq!(parse_args(["help"]), Ok(Command::Help));
        assert_eq!(
            parse_args(["replay", "--db", "run.db", "--run", "1", "--at", "12"]),
            Ok(Command::Replay {
                database: PathBuf::from("run.db"),
                run: 1,
                at: Some(12),
            })
        );
        assert_eq!(
            parse_args([
                "fork", "--db", "run.db", "--run", "1", "--at", "2", "--child", "3", "--mode",
                "live"
            ]),
            Ok(Command::Fork {
                database: PathBuf::from("run.db"),
                run: 1,
                at: 2,
                child: 3,
                mode: ReplayMode::ForkLive,
            })
        );
        assert_eq!(
            parse_args(["diff", "--db", "run.db", "--left", "1", "--right", "2"]),
            Ok(Command::Diff {
                database: PathBuf::from("run.db"),
                left: 1,
                right: 2,
            })
        );
        assert_eq!(
            parse_args(["debug", "--db", "run.db", "--run", "1"]),
            Ok(Command::Debug {
                database: PathBuf::from("run.db"),
                run: 1,
            })
        );
        assert_eq!(
            parse_args(["tui", "--demo"]),
            Ok(Command::Tui {
                database: PathBuf::from(".orynth/runtime.db"),
                run: None,
                demo: true,
            })
        );
    }

    #[test]
    fn demo_builds_a_real_runtime_snapshot() {
        let snapshot = demo_snapshot().expect("demo runtime should recover");
        let recovered = snapshot.selected.expect("demo should select its run");
        assert!(recovered.events.len() > 10);
        assert_eq!(recovered.manager.agents.len(), 4);
        assert_eq!(recovered.assumptions.conflicts().len(), 1);
        assert_eq!(recovered.messages.len(), 3);
        assert_eq!(recovered.tools.records().len(), 1);
        assert!(recovered.context.block_count() > 0);
    }

    #[test]
    fn tui_source_lists_and_recovers_a_persisted_run() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("orynth-tui-source-{suffix}.db"));
        let run_id = RunId::from_u64(701);
        let mut store = SqliteEventStore::open(&path).expect("SQLite store should open");
        store
            .append(Event::new(run_id, EventKind::RunCreated { run_id }))
            .expect("run event should append");
        drop(store);
        let mut source = DbTuiSource::new(path.clone(), None);
        let catalog = source.snapshot().expect("catalog should recover");
        assert!(catalog.selected.is_none());
        assert_eq!(catalog.runs[0].run_id, run_id);
        source.select_run(run_id).expect("run should select");
        let selected = source.snapshot().expect("selected run should recover");
        assert_eq!(selected.selected.expect("selected run").run_id, run_id);
        let older = source
            .event_page(Some(2), 16)
            .expect("older event page should load");
        assert_eq!(older.len(), 1);
        assert_eq!(older[0].event.run_id, run_id);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn inspect_recovers_a_real_sqlite_run() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("orynth-inspect-{suffix}.db"));
        let run_id = RunId::from_u64(19);
        let mut store = SqliteEventStore::open(&path).expect("SQLite store should open");
        store
            .append(Event::new(run_id, EventKind::RunCreated { run_id }))
            .expect("run event should append");
        drop(store);

        let output = inspect(path.clone(), run_id.value()).expect("run should recover");
        assert!(output.contains("run-0000000000000013"));
        assert!(output.contains("Events: 1"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn recorded_replay_recovers_only_the_selected_prefix() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("orynth-replay-{suffix}.db"));
        let run_id = RunId::from_u64(21);
        let mut store = SqliteEventStore::open(&path).expect("SQLite store should open");
        let first_sequence = store
            .append(Event::new(run_id, EventKind::RunCreated { run_id }))
            .expect("run event should append");
        store
            .append(Event::new(run_id, EventKind::RunCompleted { run_id }))
            .expect("completion event should append");
        drop(store);

        let output = replay(path.clone(), run_id.value(), Some(first_sequence))
            .expect("prefix should recover");
        assert!(output.contains("Recorded replay (provider calls: 0)"));
        assert!(output.contains("Events: 1"));
        let error = replay(path.clone(), 999, None).expect_err("unknown run should fail");
        assert!(error.contains("run 999 is not present"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn fork_command_materializes_a_real_sqlite_child_prefix() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("orynth-fork-{suffix}.db"));
        let run_id = RunId::from_u64(31);
        let child_id = RunId::from_u64(32);
        let mut store = SqliteEventStore::open(&path).expect("SQLite store should open");
        let sequence = store
            .append(Event::new(run_id, EventKind::RunCreated { run_id }))
            .expect("run event should append");
        drop(store);

        let output = fork(
            path.clone(),
            run_id.value(),
            sequence,
            child_id.value(),
            ReplayMode::Recorded,
        )
        .expect("fork should materialize");
        assert!(output.contains("Fork materialized"));
        let reopened = SqliteEventStore::open(&path).expect("SQLite store should reopen");
        let child_events = reopened
            .events(child_id)
            .expect("child events should be readable");
        assert_eq!(child_events.len(), 1);
        assert!(matches!(
            child_events[0].event.kind,
            EventKind::RunCreated { run_id } if run_id == child_id
        ));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn diff_compares_recovered_runs_without_reexecution() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("orynth-diff-{suffix}.db"));
        let left = RunId::from_u64(41);
        let right = RunId::from_u64(42);
        let mut store = SqliteEventStore::open(&path).expect("SQLite store should open");
        store
            .append(Event::new(left, EventKind::RunCreated { run_id: left }))
            .expect("left run event should append");
        store
            .append(Event::new(right, EventKind::RunCreated { run_id: right }))
            .expect("right run event should append");
        store
            .append(Event::new(right, EventKind::RunCompleted { run_id: right }))
            .expect("right completion should append");
        drop(store);

        let output = diff(path.clone(), left.value(), right.value()).expect("diff should recover");
        assert!(output.contains("Recorded projection diff (provider calls: 0)"));
        assert!(output.contains("status: Active -> Completed"));
        assert!(output.contains("events: 1 -> 2"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn debug_session_selects_events_without_mutating_recovered_state() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("orynth-debug-{suffix}.db"));
        let run_id = RunId::from_u64(51);
        let mut store = SqliteEventStore::open(&path).expect("SQLite store should open");
        store
            .append(Event::new(run_id, EventKind::RunCreated { run_id }))
            .expect("run event should append");
        drop(store);
        let recovered =
            recover_recorded_prefix(&path, run_id.value(), None).expect("run should recover");
        let sequence = recovered.events[0].sequence;
        let input = std::io::Cursor::new(format!(
            "pane events\nselect 0\nnext\nevent {sequence}\nbreakpoints\nquit\n"
        ));
        let mut bytes = Vec::new();
        run_debug_session(&recovered, input, &mut bytes).expect("session should run");
        let output = String::from_utf8(bytes).expect("debug output should be UTF-8");
        assert!(output.contains("Orynth debug session"));
        assert!(output.contains("Events pane"));
        assert!(output.contains("RunCreated"));
        assert!(output.contains("No semantic breakpoint hits."));
        assert!(output.contains("Leaving debug session."));
        let _ = std::fs::remove_file(path);
    }
}
