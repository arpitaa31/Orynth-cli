mod worker;

const MAX_LIVE_PROVIDER_REQUESTS: usize = 48;
const LIVE_COORDINATOR_OUTPUT_TOKENS: u32 = 2048;
const MAX_COORDINATOR_TOOL_CALLS: usize = 8;
const MAX_COORDINATOR_TURNS: usize = 12;
const MAX_UNPRODUCTIVE_TURNS: usize = 3;
const MAX_TURN_RETRIES: usize = 2;

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::Instant,
};

use orynth_cli::{ModelSettings, RuntimeConfig};
use orynth_context::{ContextPrincipal, ProjectionRequest, PromptLayer};
use orynth_event_store::{EventStore, RunStatus, SqliteEventStore, StoredEvent};
use orynth_kernel::{
    AgentId, AgentIdentity, CancellationToken, Event, EventKind, ModelClass, ModelRef, Run, RunId,
    Task, TaskId,
};
use orynth_provider::{
    FinishReason, ModelProvider, ModelRequest, ProviderError, ProviderEvent, RequestPart, ToolCall,
    ToolChoice, ToolDefinition as ProviderToolDefinition, openrouter::OpenRouterProvider,
};
use orynth_runtime::{
    RuntimeService,
    conversation::{ConversationSpeaker, ConversationTurn},
};
use orynth_scheduler::{BudgetLimits, BudgetUsage};
use orynth_tui::{TuiDataSource, TuiPresentation, TuiSnapshot};

use crate::DbTuiSource;

#[derive(Default)]
struct LiveState {
    busy: bool,
    text: String,
    error: Option<String>,
    resolved_model: Option<String>,
    provider_name: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModelTurnState {
    Ready,
    Requesting,
    Streaming,
    TurnFinished,
    TextResponse,
    ToolCalls,
    OutputLimit,
    Cancelled,
    Timeout,
    Failed,
}

pub(super) struct LiveTuiSource {
    db: DbTuiSource,
    database: PathBuf,
    run_id: RunId,
    task_id: TaskId,
    agent_id: AgentId,
    model: ModelRef,
    worker_model: ModelRef,
    config: RuntimeConfig,
    workspace: PathBuf,
    state: Arc<Mutex<LiveState>>,
    cancellation: Arc<Mutex<Option<CancellationToken>>>,
    effect_gate: Arc<Mutex<()>>,
}

struct LiveTurn {
    database: PathBuf,
    run_id: RunId,
    task_id: TaskId,
    agent_id: AgentId,
    model: ModelRef,
    worker_model: ModelRef,
    config: RuntimeConfig,
    workspace: PathBuf,
    effect_gate: Arc<Mutex<()>>,
}

impl LiveTuiSource {
    pub(super) fn new(config: RuntimeConfig, workspace: Option<PathBuf>) -> Result<Self, String> {
        let coordinator = config
            .models
            .manager
            .as_ref()
            .ok_or("configure [models.coordinator]")?;
        let model = model_ref(coordinator)?;
        let worker_model = model_ref(config.models.worker.as_ref().unwrap_or(coordinator))?;
        OpenRouterProvider::from_env(
            model.clone(),
            &config.openrouter.base_url,
            config.openrouter.title.clone(),
        )
        .map_err(|error| error.to_string())?;
        let repository = std::env::current_dir()
            .map_err(|error| error.to_string())?
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let sandbox = repository.join("sandbox");
        let workspace = workspace
            .map(|path| {
                if path.is_absolute() {
                    path
                } else {
                    repository.join(path)
                }
            })
            .unwrap_or_else(|| sandbox.join("orynth-workspace"));
        ensure_workspace_dir(&sandbox)?;
        let sandbox = sandbox
            .canonicalize()
            .map_err(|error| format!("could not resolve approved sandbox root: {error}"))?;
        let workspace = prepare_workspace(&sandbox, &workspace)?;
        let database = PathBuf::from(&config.runtime.event_store);
        let run = Run::new();
        let task = Task::new(run.id, "Live Coordinator session");
        let agent = AgentIdentity::new(
            "COORDINATOR",
            "Help the user and coordinate bounded work",
            model.clone(),
        );
        let store = SqliteEventStore::open(&database).map_err(|error| error.to_string())?;
        let mut service = RuntimeService::new(store);
        service
            .event_store_mut()
            .append_batch(&[
                Event::new(run.id, EventKind::RunCreated { run_id: run.id }),
                Event::new(
                    run.id,
                    EventKind::TaskCreated {
                        task_id: task.id,
                        run_id: run.id,
                        title: task.title,
                    },
                ),
                Event::new(
                    run.id,
                    EventKind::AgentCreated {
                        agent: agent.clone(),
                    },
                ),
            ])
            .map_err(|error| error.to_string())?;
        service
            .configure_budget(
                run.id,
                agent.id,
                BudgetLimits {
                    max_tokens: Some(30_000),
                    max_wall_clock_ms: Some(600_000),
                    max_tool_calls: Some(MAX_COORDINATOR_TOOL_CALLS as u64),
                    max_child_agents: Some(1),
                    ..BudgetLimits::default()
                },
            )
            .map_err(|error| error.to_string())?;
        Ok(Self {
            db: DbTuiSource::new(database.clone(), Some(run.id.value())),
            database,
            run_id: run.id,
            task_id: task.id,
            agent_id: agent.id,
            model,
            worker_model,
            config,
            workspace,
            state: Arc::new(Mutex::new(LiveState::default())),
            cancellation: Arc::new(Mutex::new(None)),
            effect_gate: Arc::new(Mutex::new(())),
        })
    }
}

pub(super) fn run_site_test(config: RuntimeConfig) -> Result<String, String> {
    let source = LiveTuiSource::new(config, None)?;
    let store = SqliteEventStore::open(&source.database).map_err(|error| error.to_string())?;
    let mut service = RuntimeService::new(store);
    let task = "Make a simple personal website with the name Arpi and one short paragraph centered on the page.";
    service
        .record_conversation_turn(source.run_id, ConversationTurn::user(task))
        .map_err(|error| error.to_string())?;
    let result = worker::run(
        &mut service,
        worker::WorkerTask {
            run_id: source.run_id,
            task_id: source.task_id,
            coordinator_id: source.agent_id,
            model: source.worker_model.clone(),
            base_url: &source.config.openrouter.base_url,
            title: source.config.openrouter.title.clone(),
            workspace: &source.workspace,
            user_text: task,
            cancellation: CancellationToken::new(),
            effect_gate: Arc::clone(&source.effect_gate),
        },
    );
    let (summary, _) = match result {
        Ok(result) => result,
        Err(message) => {
            service
                .event_store_mut()
                .append(Event::new(
                    source.run_id,
                    EventKind::RunFailed {
                        run_id: source.run_id,
                        message: message.clone(),
                    },
                ))
                .map_err(|error| error.to_string())?;
            return Err(message);
        }
    };
    service
        .event_store_mut()
        .append(Event::new(
            source.run_id,
            EventKind::RunCompleted {
                run_id: source.run_id,
            },
        ))
        .map_err(|error| error.to_string())?;
    Ok(format!(
        "{summary}\nrun ID: {}\nworkspace: {}",
        source.run_id.value(),
        source.workspace.display()
    ))
}

fn ensure_workspace_dir(path: &std::path::Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(format!(
                    "workspace component is not an ordinary directory: {}",
                    path.display()
                ));
            }
            #[cfg(windows)]
            if std::os::windows::fs::MetadataExt::file_attributes(&metadata) & 0x400 != 0 {
                return Err(format!(
                    "workspace component is a reparse point: {}",
                    path.display()
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(path).map_err(|error| error.to_string())?;
        }
        Err(error) => return Err(error.to_string()),
    }
    Ok(())
}

fn prepare_workspace(
    sandbox: &std::path::Path,
    requested: &std::path::Path,
) -> Result<PathBuf, String> {
    if requested
        .components()
        .any(|component| component == std::path::Component::ParentDir)
    {
        return Err(format!(
            "live coding workspace must not contain '..': {}",
            requested.display()
        ));
    }
    let relative = relative_workspace_path(sandbox, requested).ok_or_else(|| {
        format!(
            "live coding workspace must be contained by {}: {}",
            sandbox.display(),
            requested.display()
        )
    })?;
    if relative.as_os_str().is_empty() {
        return Err("live coding workspace must be a directory below the approved sandbox root".into());
    }

    let mut current = sandbox.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(format!(
                "invalid live coding workspace path: {}",
                requested.display()
            ));
        };
        current.push(name);
        ensure_workspace_dir(&current)?;
    }

    let resolved = current
        .canonicalize()
        .map_err(|error| format!("could not resolve live coding workspace: {error}"))?;
    if !path_is_within(sandbox, &resolved) || resolved == sandbox {
        return Err(format!(
            "live coding workspace resolves outside the approved sandbox: {}",
            requested.display()
        ));
    }
    Ok(resolved)
}

fn relative_workspace_path(
    sandbox: &std::path::Path,
    requested: &std::path::Path,
) -> Option<PathBuf> {
    if let Ok(relative) = requested.strip_prefix(sandbox) {
        return Some(relative.to_path_buf());
    }
    #[cfg(windows)]
    {
        let normalize = |path: &std::path::Path| {
            path.to_string_lossy()
                .replace('/', "\\")
                .trim_end_matches('\\')
                .to_ascii_lowercase()
        };
        let root = normalize(sandbox);
        let candidate = normalize(requested);
        let prefix = format!("{root}\\");
        return candidate
            .strip_prefix(&prefix)
            .map(PathBuf::from);
    }
    #[cfg(not(windows))]
    {
        None
    }
}

fn path_is_within(root: &std::path::Path, candidate: &std::path::Path) -> bool {
    if candidate.strip_prefix(root).is_ok() {
        return true;
    }
    #[cfg(windows)]
    {
        let normalize = |path: &std::path::Path| {
            path.to_string_lossy()
                .replace('/', "\\")
                .trim_end_matches('\\')
                .to_ascii_lowercase()
        };
        let root = normalize(root);
        let candidate = normalize(candidate);
        return candidate == root || candidate.starts_with(&(root + "\\"));
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn model_ref(settings: &ModelSettings) -> Result<ModelRef, String> {
    if settings.provider != "openrouter" {
        return Err("model provider must be openrouter".into());
    }
    let class = match settings.model_class.as_str() {
        "strong" => ModelClass::Strong,
        "economy" | "cheap" => ModelClass::Cheap,
        _ => return Err("model class must be strong or economy".into()),
    };
    Ok(ModelRef::new("openrouter", settings.model.clone(), class))
}

impl TuiDataSource for LiveTuiSource {
    fn snapshot(&mut self) -> Result<TuiSnapshot, String> {
        let mut snapshot = self.db.snapshot()?;
        let (resolved, provider_name) = self
            .state
            .lock()
            .ok()
            .map(|state| (state.resolved_model.clone(), state.provider_name.clone()))
            .unwrap_or_default();
        snapshot.presentation = Some(TuiPresentation {
            title: "LIVE · OPENROUTER".into(),
            description: format!(
                "Route: {}{}{}",
                self.model.model,
                resolved
                    .map(|name| format!(" · Model: {name}"))
                    .unwrap_or_default(),
                provider_name
                    .map(|name| format!(" · Provider: {name}"))
                    .unwrap_or_default()
            ),
            demo: false,
        });
        Ok(snapshot)
    }
    fn select_run(&mut self, run_id: RunId) -> Result<(), String> {
        self.db.select_run(run_id)
    }
    fn event_page(
        &mut self,
        before_sequence: Option<u64>,
        limit: usize,
    ) -> Result<Vec<StoredEvent>, String> {
        self.db.event_page(before_sequence, limit)
    }
    fn is_live(&self) -> bool {
        true
    }
    fn live_text(&self) -> Option<String> {
        let state = self.state.lock().ok()?;
        if let Some(error) = &state.error {
            return Some(format!("Provider error: {error}"));
        }
        if state.busy {
            Some(if state.text.is_empty() {
                "Thinking…".into()
            } else {
                state.text.clone()
            })
        } else {
            None
        }
    }
    fn submit_text(&mut self, text: String) -> Result<(), String> {
        if text.len() > 16 * 1024 {
            return Err("Message exceeds 16 KiB".into());
        }
        let mut state = self.state.lock().map_err(|_| "live state unavailable")?;
        if state.busy {
            return Err("Coordinator is still responding".into());
        }
        let store = SqliteEventStore::open(&self.database).map_err(|error| error.to_string())?;
        let mut service = RuntimeService::new(store);
        service
            .record_conversation_turn(self.run_id, ConversationTurn::user(text))
            .map_err(|error| error.to_string())?;
        state.busy = true;
        state.text.clear();
        state.error = None;
        state.resolved_model = None;
        state.provider_name = None;
        drop(state);
        let database = self.database.clone();
        let shared = Arc::clone(&self.state);
        let run_id = self.run_id;
        let task_id = self.task_id;
        let agent_id = self.agent_id;
        let model = self.model.clone();
        let worker_model = self.worker_model.clone();
        let config = self.config.clone();
        let workspace = self.workspace.clone();
        let effect_gate = Arc::clone(&self.effect_gate);
        let cancellation = CancellationToken::new();
        *self
            .cancellation
            .lock()
            .map_err(|_| "cancellation state unavailable")? = Some(cancellation.clone());
        let slot = Arc::clone(&self.cancellation);
        thread::spawn(move || {
            let failure_database = database.clone();
            let turn = LiveTurn {
                database,
                run_id,
                task_id,
                agent_id,
                model,
                worker_model,
                config,
                workspace,
                effect_gate,
            };
            let result = execute_turn(turn, cancellation.clone(), &shared);
            if let Err(error) = &result
                && let Ok(store) = SqliteEventStore::open(&failure_database)
                && store
                    .reconstruct(run_id)
                    .is_ok_and(|state| state.status == RunStatus::Active)
            {
                let mut service = RuntimeService::new(store);
                let kind = if cancellation.is_cancelled() {
                    EventKind::ModelTurnCancelled { agent_id }
                } else {
                    EventKind::ModelTurnFailed {
                        agent_id,
                        message: error.clone(),
                    }
                };
                let _ = service.record_model_transition(run_id, kind);
            }
            if let Ok(mut state) = shared.lock() {
                state.busy = false;
                state.text.clear();
                state.error = result.err();
            }
            if let Ok(mut slot) = slot.lock() {
                *slot = None;
            }
        });
        Ok(())
    }
}

impl Drop for LiveTuiSource {
    fn drop(&mut self) {
        let _effect_guard = self
            .effect_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (busy, error) = self
            .state
            .lock()
            .map(|state| (state.busy, state.error.clone()))
            .unwrap_or((false, None));
        if let Ok(slot) = self.cancellation.lock()
            && let Some(token) = &*slot
        {
            token.cancel();
        }
        let _ = finalize_live_run(&self.database, self.run_id, busy, error);
    }
}

fn finalize_live_run(
    database: &std::path::Path,
    run_id: RunId,
    busy: bool,
    error: Option<String>,
) -> Result<(), String> {
    let mut store = SqliteEventStore::open(database).map_err(|error| error.to_string())?;
    if store
        .reconstruct(run_id)
        .map_err(|error| error.to_string())?
        .status
        != RunStatus::Active
    {
        return Ok(());
    }
    let kind = if busy {
        EventKind::RunCancelled { run_id }
    } else if let Some(message) = error {
        EventKind::RunFailed { run_id, message }
    } else {
        EventKind::RunCompleted { run_id }
    };
    store
        .append(Event::new(run_id, kind))
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn execute_turn(
    turn: LiveTurn,
    cancellation: CancellationToken,
    shared: &Arc<Mutex<LiveState>>,
) -> Result<(), String> {
    let LiveTurn {
        database,
        run_id,
        task_id,
        agent_id,
        model,
        worker_model,
        config,
        workspace,
        effect_gate,
    } = turn;
    let store = SqliteEventStore::open(&database).map_err(|error| error.to_string())?;
    let events = store.events(run_id).map_err(|error| error.to_string())?;
    let mut request = ModelRequest::new(run_id, task_id, agent_id, model.clone(), "");
    request.parts = vec![RequestPart::System(
        "You are Orynth's Coordinator. Orynth owns authoritative state and side effects. Use only reported runtime facts. Do not claim actions or worker results until Orynth confirms them. Do not reveal hidden reasoning. Delegate a tiny website to one worker when useful.".into())];
    let recovered =
        RuntimeService::new(SqliteEventStore::open(&database).map_err(|error| error.to_string())?)
            .recover(run_id)
            .map_err(|error| error.to_string())?;
    let roster = recovered
        .manager
        .agents
        .values()
        .take(8)
        .map(|agent| format!("{}: {:?} ({})", agent.name, agent.status, agent.model.model))
        .collect::<Vec<_>>()
        .join("; ");
    let projected = recovered.context.project(
        ContextPrincipal::Agent(agent_id),
        &ProjectionRequest {
            max_blocks: Some(8),
            max_tokens: Some(4_000),
            ..ProjectionRequest::all()
        },
    );
    let references = projected
        .blocks
        .iter()
        .map(|entry| entry.block.reference())
        .collect();
    let prompt = recovered.context.render_prompt(ContextPrincipal::Agent(agent_id),
        &[PromptLayer::stable("projected context", references)],
        &format!(
            "Runtime facts: active agents: {roster}. Selected coding workspace: {}.",
            workspace.display()
        ))
        .map_err(|error| error.to_string())?;
    request.parts.push(RequestPart::System(prompt.text));
    let mut latest_user = String::new();
    for stored in events {
        if let EventKind::ConversationTurn { version, payload } = stored.event.kind {
            let turn =
                ConversationTurn::decode(version, &payload).map_err(|error| error.to_string())?;
            match turn.speaker {
                ConversationSpeaker::User => {
                    latest_user = turn.content.clone();
                    request.parts.push(RequestPart::User(turn.content));
                }
                ConversationSpeaker::Coordinator(_) => {
                    request.parts.push(RequestPart::Model(turn.content))
                }
            }
        }
    }
    bound_conversation_parts(&mut request.parts);
    request.max_output_tokens = Some(LIVE_COORDINATOR_OUTPUT_TOKENS);
    if worker::small_site_task(&latest_user) {
        request.tools.push(ProviderToolDefinition {
            name: "delegate_personal_site".into(),
            description: "Assign the user's tiny personal website request to one Orynth coding worker in the selected sandbox".into(),
            input_schema: r#"{"type":"object","properties":{},"additionalProperties":false}"#.into(),
        });
        request.tool_choice = ToolChoice::Required;
    }
    let mut service = RuntimeService::new(store);
    let provider = OpenRouterProvider::from_env(
        model.clone(),
        &config.openrouter.base_url,
        config.openrouter.title.clone(),
    )
    .map_err(|error| error.to_string())?;
    let mut next_parts = request.parts;
    let mut tool_choice = request.tool_choice;
    let tools = request.tools;
    let mut unproductive_turns = 0usize;
    let mut total_tool_calls = 0usize;
    let mut partial_output = String::new();

    for turn_index in 0..MAX_COORDINATOR_TURNS {
        if cancellation.is_cancelled() {
            return Err("Coordinator request cancelled".into());
        }
        let mut next = ModelRequest::new(run_id, task_id, agent_id, model.clone(), "");
        next.parts = next_parts.clone();
        next.tools = tools.clone();
        next.tool_choice = tool_choice;
        next.max_output_tokens = Some(LIVE_COORDINATOR_OUTPUT_TOKENS);
        let (output, calls, reason, usage) =
            run_coordinator_request(&mut service, &provider, next, cancellation.clone(), shared)?;
        match reason {
            FinishReason::Stop => {
                if output.trim().is_empty() {
                    return Err("Coordinator returned an empty response".into());
                }
                service
                    .record_conversation_turn(
                        run_id,
                        ConversationTurn::coordinator(agent_id, output),
                    )
                    .map_err(|error| error.to_string())?;
                return Ok(());
            }
            FinishReason::ToolCall => {
                if calls.is_empty() {
                    return Err("Coordinator reported tool_calls without an action".into());
                }
                total_tool_calls = total_tool_calls.saturating_add(calls.len());
                if total_tool_calls > MAX_COORDINATOR_TOOL_CALLS {
                    return Err("Coordinator tool-call budget exceeded".into());
                }
                next_parts.push(RequestPart::ModelToolCalls(calls.clone()));
                let mut actions = CoordinatorActionContext {
                    service: &mut service,
                    run_id,
                    task_id,
                    agent_id,
                    worker_model: &worker_model,
                    config: &config,
                    workspace: &workspace,
                    user_text: &latest_user,
                    cancellation: &cancellation,
                    effect_gate: &effect_gate,
                };
                let results = execute_coordinator_actions(&mut actions, &calls)?;
                for (call_id, result) in results {
                    next_parts.push(RequestPart::ToolResult {
                        call_id,
                        content: result,
                        is_error: false,
                    });
                }
                tool_choice = ToolChoice::Auto;
                unproductive_turns = 0;
            }
            FinishReason::Length => {
                if !calls.is_empty() {
                    total_tool_calls = total_tool_calls.saturating_add(calls.len());
                    if total_tool_calls > MAX_COORDINATOR_TOOL_CALLS {
                        return Err("Coordinator tool-call budget exceeded".into());
                    }
                    next_parts.push(RequestPart::ModelToolCalls(calls.clone()));
                    let mut actions = CoordinatorActionContext {
                        service: &mut service,
                        run_id,
                        task_id,
                        agent_id,
                        worker_model: &worker_model,
                        config: &config,
                        workspace: &workspace,
                        user_text: &latest_user,
                        cancellation: &cancellation,
                        effect_gate: &effect_gate,
                    };
                    let results = execute_coordinator_actions(&mut actions, &calls)?;
                    for (call_id, result) in results {
                        next_parts.push(RequestPart::ToolResult {
                            call_id,
                            content: result,
                            is_error: false,
                        });
                    }
                    tool_choice = ToolChoice::Auto;
                    unproductive_turns = 0;
                } else {
                    let has_output = !output.trim().is_empty();
                    if has_output {
                        partial_output.push_str(&output);
                        next_parts.push(RequestPart::Model(output));
                    }
                    unproductive_turns = if !has_output {
                        unproductive_turns.saturating_add(1)
                    } else {
                        0
                    };
                    if unproductive_turns >= MAX_UNPRODUCTIVE_TURNS {
                        return Err(format_coordinator_turn_status(
                            &model,
                            shared,
                            &reason,
                            &partial_output,
                            usage,
                        ));
                    }
                    next_parts.push(RequestPart::System(
                        "The previous model turn reached its output limit before this task was complete. Continue from the current Orynth runtime state. Return only the next runtime action or a concise final answer; do not repeat completed work or hidden reasoning.".into(),
                    ));
                }
            }
            other => {
                return Err(format!(
                    "Coordinator response ended with {}",
                    finish_reason_label(&other)
                ));
            }
        }
        if turn_index + 1 == MAX_COORDINATOR_TURNS {
            return Err("Coordinator model-turn budget exhausted".into());
        }
    }
    Err("Coordinator model-turn budget exhausted".into())
}

struct CoordinatorActionContext<'a> {
    service: &'a mut RuntimeService<SqliteEventStore>,
    run_id: RunId,
    task_id: TaskId,
    agent_id: AgentId,
    worker_model: &'a ModelRef,
    config: &'a RuntimeConfig,
    workspace: &'a std::path::Path,
    user_text: &'a str,
    cancellation: &'a CancellationToken,
    effect_gate: &'a Arc<Mutex<()>>,
}

fn execute_coordinator_actions(
    context: &mut CoordinatorActionContext<'_>,
    calls: &[ToolCall],
) -> Result<Vec<(String, String)>, String> {
    let mut results = Vec::with_capacity(calls.len());
    for call in calls {
        if call.name != "delegate_personal_site"
            || !worker::small_site_task(context.user_text)
            || !serde_json::from_str::<serde_json::Value>(&call.arguments)
                .is_ok_and(|value| value.as_object().is_some_and(|map| map.is_empty()))
        {
            return Err(format!(
                "Coordinator proposed invalid action: {}",
                call.name
            ));
        }
        let (summary, _) = worker::run(
            context.service,
            worker::WorkerTask {
                run_id: context.run_id,
                task_id: context.task_id,
                coordinator_id: context.agent_id,
                model: context.worker_model.clone(),
                base_url: &context.config.openrouter.base_url,
                title: context.config.openrouter.title.clone(),
                workspace: context.workspace,
                user_text: context.user_text,
                cancellation: context.cancellation.clone(),
                effect_gate: Arc::clone(context.effect_gate),
            },
        )?;
        results.push((call.call_id.clone(), summary));
    }
    Ok(results)
}

fn bound_conversation_parts(parts: &mut Vec<RequestPart>) {
    if parts.len() > 64 {
        // Retain both runtime/system layers while bounding older conversation.
        parts.drain(2..parts.len() - 62);
    }
}

fn run_coordinator_request(
    service: &mut RuntimeService<SqliteEventStore>,
    provider: &dyn ModelProvider,
    request: ModelRequest,
    cancellation: CancellationToken,
    shared: &Arc<Mutex<LiveState>>,
) -> Result<
    (
        String,
        Vec<ToolCall>,
        FinishReason,
        Option<orynth_kernel::Usage>,
    ),
    String,
> {
    let mut attempts = 0usize;
    loop {
        let result = run_coordinator_request_once(
            service,
            provider,
            request.clone(),
            cancellation.clone(),
            shared,
        );
        match result {
            Err(message)
                if attempts < MAX_TURN_RETRIES
                    && (message.starts_with("Coordinator provider timeout")
                        || message.starts_with("Coordinator provider rate limited")) =>
            {
                if cancellation.is_cancelled() {
                    return Err("Coordinator request cancelled".into());
                }
                attempts = attempts.saturating_add(1);
                let delay_ms = 50u64.saturating_mul(attempts as u64);
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            }
            other => return other,
        }
    }
}

fn run_coordinator_request_once(
    service: &mut RuntimeService<SqliteEventStore>,
    provider: &dyn ModelProvider,
    request: ModelRequest,
    cancellation: CancellationToken,
    shared: &Arc<Mutex<LiveState>>,
) -> Result<
    (
        String,
        Vec<ToolCall>,
        FinishReason,
        Option<orynth_kernel::Usage>,
    ),
    String,
> {
    let mut state = ModelTurnState::Ready;
    debug_assert_eq!(state, ModelTurnState::Ready);
    state = ModelTurnState::Requesting;
    let run_id = request.run_id;
    let agent_id = request.agent_id;
    ensure_provider_request_budget(service, run_id)?;
    service
        .record_model_transition(
            run_id,
            EventKind::ModelRequested {
                agent_id,
                model: request.model.clone(),
            },
        )
        .map_err(|error| error.to_string())?;
    let started = Instant::now();
    let mut output = String::new();
    let mut calls = Vec::new();
    let mut usage = None;
    let mut finish = None;
    let mut chunk_index = 0u32;
    debug_assert_eq!(state, ModelTurnState::Requesting);
    state = ModelTurnState::Streaming;
    debug_assert_eq!(state, ModelTurnState::Streaming);
    for event in provider
        .stream(request, cancellation)
        .map_err(coordinator_provider_error)?
    {
        match event.map_err(coordinator_provider_error)? {
            ProviderEvent::TextDelta { text } => {
                if output.len().saturating_add(text.len()) > 16 * 1024 {
                    return Err("Coordinator reply exceeds 16 KiB".into());
                }
                output.push_str(&text);
                if let Ok(mut state) = shared.lock() {
                    state.text = output.clone();
                }
                service
                    .record_model_transition(
                        run_id,
                        EventKind::ModelChunkReceived {
                            agent_id,
                            chunk_index,
                        },
                    )
                    .map_err(|error| error.to_string())?;
                chunk_index = chunk_index.saturating_add(1);
            }
            ProviderEvent::ToolCallCompleted { call } => {
                if calls.len() >= MAX_COORDINATOR_TOOL_CALLS {
                    return Err("Coordinator tool-call limit reached".into());
                }
                calls.push(call);
            }
            ProviderEvent::ToolCallStarted { .. }
            | ProviderEvent::ToolCallArgumentsDelta { .. } => {}
            ProviderEvent::Usage(observed) => usage = Some(observed.usage),
            ProviderEvent::ResponseMetadata {
                resolved_model,
                request_id,
                provider_name,
            } => {
                service
                    .record_model_transition(
                        run_id,
                        EventKind::ModelResponseMetadata {
                            agent_id,
                            resolved_model: resolved_model.clone(),
                            provider_request_id: request_id,
                            provider_name: provider_name.clone(),
                        },
                    )
                    .map_err(|error| error.to_string())?;
                if let Ok(mut state) = shared.lock() {
                    if resolved_model.is_some() {
                        state.resolved_model = resolved_model;
                    }
                    if provider_name.is_some() {
                        state.provider_name = provider_name;
                    }
                }
            }
            ProviderEvent::Finish(reason) => finish = Some(reason),
            ProviderEvent::ReasoningDelta { .. } => {}
        }
    }
    let finish = finish.ok_or("OpenRouter stream ended without completion")?;
    state = ModelTurnState::TurnFinished;
    debug_assert_eq!(state, ModelTurnState::TurnFinished);
    state = match &finish {
        FinishReason::Stop => ModelTurnState::TextResponse,
        FinishReason::ToolCall => ModelTurnState::ToolCalls,
        FinishReason::Length => ModelTurnState::OutputLimit,
        FinishReason::Cancelled => ModelTurnState::Cancelled,
        FinishReason::ContentFilter | FinishReason::Other(_) => ModelTurnState::Failed,
    };
    if state == ModelTurnState::Cancelled {
        return Err("Coordinator request cancelled".into());
    }
    if matches!(finish, FinishReason::Length) {
        // A provider-reported length finish is a completed, bounded turn.
        // The caller decides whether to continue after preserving visible text.
    } else if !matches!(finish, FinishReason::Stop | FinishReason::ToolCall) {
        return Err(format!(
            "Coordinator response ended with {}",
            finish_reason_label(&finish)
        ));
    }
    if (finish == FinishReason::Stop && output.trim().is_empty())
        || (finish == FinishReason::Stop && !calls.is_empty())
        || (finish == FinishReason::ToolCall && calls.is_empty())
    {
        return Err("Coordinator returned an empty response".into());
    }
    service
        .record_model_transition(run_id, EventKind::ModelTurnCompleted { agent_id, usage })
        .map_err(|error| error.to_string())?;
    service
        .record_model_transition(
            run_id,
            EventKind::ModelTurnOutcome {
                agent_id,
                finish: finish_reason_label(&finish),
                continued: !matches!(finish, FinishReason::Stop),
                unproductive: finish == FinishReason::Length
                    && output.trim().is_empty()
                    && calls.is_empty(),
            },
        )
        .map_err(|error| error.to_string())?;
    service
        .record_agent_usage(
            run_id,
            agent_id,
            BudgetUsage {
                tokens: usage.map_or(0, |observed| observed.total_tokens()),
                wall_clock_ms: started.elapsed().as_millis() as u64,
                ..BudgetUsage::default()
            },
        )
        .map_err(|error| error.to_string())?;
    Ok((output, calls, finish, usage))
}

fn coordinator_provider_error(error: ProviderError) -> String {
    let state = match &error {
        ProviderError::Cancelled => ModelTurnState::Cancelled,
        ProviderError::Timeout => ModelTurnState::Timeout,
        _ => ModelTurnState::Failed,
    };
    let _ = state;
    match error {
        ProviderError::Cancelled => "Coordinator request cancelled".into(),
        ProviderError::Timeout => "Coordinator provider timeout".into(),
        ProviderError::RateLimited { retry_after_ms } => format!(
            "Coordinator provider rate limited{}",
            retry_after_ms
                .map(|value| format!("; retry after {value} ms"))
                .unwrap_or_default()
        ),
        ProviderError::MalformedStream(message) => {
            format!("Coordinator provider stream malformed: {message}")
        }
        other => format!("Coordinator provider error: {other}"),
    }
}

fn format_coordinator_turn_status(
    model: &ModelRef,
    shared: &Arc<Mutex<LiveState>>,
    reason: &FinishReason,
    output: &str,
    usage: Option<orynth_kernel::Usage>,
) -> String {
    let (resolved, provider) = shared
        .lock()
        .ok()
        .map(|state| (state.resolved_model.clone(), state.provider_name.clone()))
        .unwrap_or_default();
    format!(
        "Coordinator response stopped\nRoute: {}\nResolved model: {}\nProvider: {}\nFinish: {}\nInput tokens: {}\nOutput tokens: {}\nVisible text: {}",
        model.model,
        resolved.as_deref().unwrap_or("not reported"),
        provider.as_deref().unwrap_or("not reported"),
        finish_reason_label(reason),
        usage.map_or(0, |value| value.input_tokens),
        usage.map_or(0, |value| value.output_tokens),
        if output.trim().is_empty() {
            "no"
        } else {
            "yes"
        },
    )
}

fn finish_reason_label(reason: &FinishReason) -> String {
    match reason {
        FinishReason::Stop => "stop".into(),
        FinishReason::ToolCall => "tool_calls".into(),
        FinishReason::Length => "length (output limit reached)".into(),
        FinishReason::Cancelled => "cancelled".into(),
        FinishReason::ContentFilter => "content_filter".into(),
        FinishReason::Other(value) => format!("unknown ({value})"),
    }
}

fn ensure_provider_request_budget(
    service: &RuntimeService<SqliteEventStore>,
    run_id: RunId,
) -> Result<(), String> {
    let count = service
        .event_store()
        .events(run_id)
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|stored| matches!(stored.event.kind, EventKind::ModelRequested { .. }))
        .count();
    if count >= MAX_LIVE_PROVIDER_REQUESTS {
        Err(format!(
            "live provider request limit reached ({MAX_LIVE_PROVIDER_REQUESTS})"
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orynth_event_store::AgentStatus;
    use orynth_provider::{
        MockProvider, ProviderCapabilities, ProviderError, ProviderUsage, ToolCall,
    };
    use std::{
        fs,
        io::{Read, Write},
        net::TcpListener,
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    fn workspace_test_root() -> (PathBuf, PathBuf) {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("orynth-workspace-test-{suffix}"));
        let sandbox = root.join("sandbox");
        fs::create_dir_all(&sandbox).expect("sandbox should be created");
        (root, sandbox)
    }

    #[test]
    fn arbitrary_existing_and_nested_sandbox_workspaces_are_allowed() {
        let (root, sandbox) = workspace_test_root();
        for relative in ["phase-g-personal-site", "multi-agent-test", "foo/bar"] {
            let requested = sandbox.join(relative);
            let resolved = prepare_workspace(&sandbox, &requested).expect("workspace is valid");
            assert!(path_is_within(&sandbox, &resolved));
        }
        let absolute = prepare_workspace(&sandbox, &sandbox.join("absolute-test"))
            .expect("workspace is valid")
            .canonicalize()
            .expect("workspace should resolve");
        let resolved = prepare_workspace(&sandbox, &absolute).expect("absolute path is valid");
        assert!(path_is_within(&sandbox, &resolved));
        let dot_relative = sandbox
            .parent()
            .expect("sandbox has a parent")
            .join(".")
            .join("sandbox")
            .join("dot-relative");
        assert!(prepare_workspace(&sandbox, &dot_relative).is_ok());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_escape_and_parent_paths_are_rejected() {
        let (root, sandbox) = workspace_test_root();
        let outside = root.join("outside");
        fs::create_dir_all(&outside).expect("outside fixture should be created");
        assert!(prepare_workspace(&sandbox, &outside).is_err());
        assert!(prepare_workspace(&sandbox, &sandbox.join("..").join("outside")).is_err());
        assert!(prepare_workspace(&sandbox, &sandbox).is_err());
        let _ = fs::remove_dir_all(root);
    }

    struct RetryProvider {
        model: ModelRef,
        attempts: AtomicUsize,
    }

    impl ModelProvider for RetryProvider {
        fn model(&self) -> &ModelRef {
            &self.model
        }

        fn capabilities(&self) -> ProviderCapabilities {
            MockProvider::new(self.model.clone(), "").capabilities()
        }

        fn stream(
            &self,
            request: ModelRequest,
            cancellation: CancellationToken,
        ) -> Result<
            Box<dyn Iterator<Item = Result<ProviderEvent, ProviderError>> + Send>,
            ProviderError,
        > {
            self.validate_request(&request)?;
            if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(ProviderError::Timeout);
            }
            Ok(Box::new(
                vec![
                    Ok(ProviderEvent::TextDelta {
                        text: "recovered".into(),
                    }),
                    Ok(ProviderEvent::Finish(FinishReason::Stop)),
                ]
                .into_iter()
                .map(move |event| {
                    if cancellation.is_cancelled() {
                        Err(ProviderError::Cancelled)
                    } else {
                        event
                    }
                }),
            ))
        }
    }

    fn coordinator_fixture() -> (
        std::path::PathBuf,
        RuntimeService<SqliteEventStore>,
        RunId,
        TaskId,
        AgentId,
        ModelRef,
        Arc<Mutex<LiveState>>,
    ) {
        let database = std::env::temp_dir().join(format!(
            "orynth-coordinator-turn-{}.db",
            RunId::new().value()
        ));
        let run = Run::new();
        let task = Task::new(run.id, "coordinator turn");
        let model = ModelRef::new("mock", "coordinator", ModelClass::Cheap);
        let agent = AgentIdentity::new("COORDINATOR", "coordinate", model.clone());
        let mut service = RuntimeService::new(SqliteEventStore::open(&database).unwrap());
        service
            .event_store_mut()
            .append_batch(&[
                Event::new(run.id, EventKind::RunCreated { run_id: run.id }),
                Event::new(
                    run.id,
                    EventKind::TaskCreated {
                        task_id: task.id,
                        run_id: run.id,
                        title: task.title,
                    },
                ),
                Event::new(
                    run.id,
                    EventKind::AgentCreated {
                        agent: agent.clone(),
                    },
                ),
            ])
            .unwrap();
        (
            database,
            service,
            run.id,
            task.id,
            agent.id,
            model,
            Arc::new(Mutex::new(LiveState::default())),
        )
    }

    fn cleanup_database(database: &std::path::Path) {
        let _ = std::fs::remove_file(database);
        let _ = std::fs::remove_file(database.with_extension("db-wal"));
        let _ = std::fs::remove_file(database.with_extension("db-shm"));
    }

    fn coordinator_request(
        run_id: RunId,
        task_id: TaskId,
        agent_id: AgentId,
        model: ModelRef,
    ) -> ModelRequest {
        ModelRequest::new(run_id, task_id, agent_id, model, "reply")
    }

    #[test]
    fn coordinator_turn_lifecycle_handles_finish_states_without_false_failure() {
        let (database, mut service, run_id, task_id, agent_id, model, shared) =
            coordinator_fixture();
        let provider = MockProvider::new(model.clone(), "one short sentence").with_chunk_size(2);
        let (output, calls, finish, usage) = run_coordinator_request(
            &mut service,
            &provider,
            coordinator_request(run_id, task_id, agent_id, model.clone()),
            CancellationToken::new(),
            &shared,
        )
        .unwrap();
        assert_eq!(output, "one short sentence");
        assert!(calls.is_empty());
        assert_eq!(finish, FinishReason::Stop);
        assert!(usage.is_some());
        drop(service);
        cleanup_database(&database);

        let (database, mut service, run_id, task_id, agent_id, model, shared) =
            coordinator_fixture();
        let provider = MockProvider::new(model.clone(), "").with_script(vec![
            Ok(ProviderEvent::ToolCallCompleted {
                call: ToolCall {
                    call_id: "call-1".into(),
                    name: "spawn_agent".into(),
                    arguments: "{}".into(),
                },
            }),
            Ok(ProviderEvent::Finish(FinishReason::ToolCall)),
        ]);
        let (output, calls, finish, _) = run_coordinator_request(
            &mut service,
            &provider,
            coordinator_request(run_id, task_id, agent_id, model),
            CancellationToken::new(),
            &shared,
        )
        .unwrap();
        assert!(output.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(finish, FinishReason::ToolCall);
        drop(service);
        cleanup_database(&database);

        let (database, mut service, run_id, task_id, agent_id, model, shared) =
            coordinator_fixture();
        let provider = MockProvider::new(model.clone(), "").with_script(vec![
            Ok(ProviderEvent::ToolCallCompleted {
                call: ToolCall {
                    call_id: "call-1".into(),
                    name: "message_agent".into(),
                    arguments: "{}".into(),
                },
            }),
            Ok(ProviderEvent::ToolCallCompleted {
                call: ToolCall {
                    call_id: "call-2".into(),
                    name: "wait_for_agent".into(),
                    arguments: "{}".into(),
                },
            }),
            Ok(ProviderEvent::Finish(FinishReason::ToolCall)),
        ]);
        let (_, calls, finish, _) = run_coordinator_request(
            &mut service,
            &provider,
            coordinator_request(run_id, task_id, agent_id, model),
            CancellationToken::new(),
            &shared,
        )
        .unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(finish, FinishReason::ToolCall);
        drop(service);
        cleanup_database(&database);

        let (database, mut service, run_id, task_id, agent_id, model, shared) =
            coordinator_fixture();
        let provider = MockProvider::new(model.clone(), "partial answer").with_script(vec![
            Ok(ProviderEvent::TextDelta {
                text: "partial answer".into(),
            }),
            Ok(ProviderEvent::Usage(ProviderUsage {
                usage: orynth_kernel::Usage::new(11, 2048),
                cost: None,
                prompt_cache_hit: None,
            })),
            Ok(ProviderEvent::Finish(FinishReason::Length)),
        ]);
        let (output, _, finish, usage) = run_coordinator_request(
            &mut service,
            &provider,
            coordinator_request(run_id, task_id, agent_id, model.clone()),
            CancellationToken::new(),
            &shared,
        )
        .unwrap();
        assert_eq!(output, "partial answer");
        assert_eq!(finish, FinishReason::Length);
        assert_eq!(usage.unwrap().output_tokens, 2048);
        let status = format_coordinator_turn_status(&model, &shared, &finish, &output, usage);
        assert!(status.contains("Coordinator response stopped"));
        assert!(status.contains("Visible text: yes"));
        drop(service);
        cleanup_database(&database);

        let (database, mut service, run_id, task_id, agent_id, model, shared) =
            coordinator_fixture();
        let provider = MockProvider::new(model.clone(), "").with_script(vec![
            Ok(ProviderEvent::Usage(ProviderUsage {
                usage: orynth_kernel::Usage::new(11, 2048),
                cost: None,
                prompt_cache_hit: None,
            })),
            Ok(ProviderEvent::Finish(FinishReason::Length)),
        ]);
        let (output, _, finish, usage) = run_coordinator_request(
            &mut service,
            &provider,
            coordinator_request(run_id, task_id, agent_id, model.clone()),
            CancellationToken::new(),
            &shared,
        )
        .unwrap();
        let status = format_coordinator_turn_status(&model, &shared, &finish, &output, usage);
        assert!(status.contains("Visible text: no"));
        assert!(status.contains("Output tokens: 2048"));
        drop(service);
        cleanup_database(&database);
    }

    #[test]
    fn coordinator_turn_lifecycle_reports_stream_and_provider_failures() {
        for error in [
            ProviderError::Timeout,
            ProviderError::Cancelled,
            ProviderError::RateLimited {
                retry_after_ms: Some(250),
            },
        ] {
            let (database, mut service, run_id, task_id, agent_id, model, shared) =
                coordinator_fixture();
            let provider = MockProvider::new(model.clone(), "").with_script(vec![Err(error)]);
            let result = run_coordinator_request(
                &mut service,
                &provider,
                coordinator_request(run_id, task_id, agent_id, model),
                CancellationToken::new(),
                &shared,
            );
            let message = result.expect_err("provider failures must be surfaced");
            assert!(!message.contains("did not finish normally"));
            drop(service);
            cleanup_database(&database);
        }

        let (database, mut service, run_id, task_id, agent_id, model, shared) =
            coordinator_fixture();
        let provider = MockProvider::new(model.clone(), "partial").with_script(vec![Ok(
            ProviderEvent::TextDelta {
                text: "partial".into(),
            },
        )]);
        let result = run_coordinator_request(
            &mut service,
            &provider,
            coordinator_request(run_id, task_id, agent_id, model),
            CancellationToken::new(),
            &shared,
        );
        assert!(matches!(result, Err(message) if message.contains("ended without completion")));
        drop(service);
        cleanup_database(&database);
    }

    #[test]
    fn timeout_is_retried_without_replacing_the_logical_agent() {
        let (database, mut service, run_id, task_id, agent_id, model, shared) =
            coordinator_fixture();
        let provider = RetryProvider {
            model: model.clone(),
            attempts: AtomicUsize::new(0),
        };
        let (output, calls, finish, _) = run_coordinator_request(
            &mut service,
            &provider,
            coordinator_request(run_id, task_id, agent_id, model),
            CancellationToken::new(),
            &shared,
        )
        .unwrap();
        assert_eq!(output, "recovered");
        assert!(calls.is_empty());
        assert_eq!(finish, FinishReason::Stop);
        assert_eq!(provider.attempts.load(Ordering::SeqCst), 2);
        assert_eq!(service.recover(run_id).unwrap().state.agents.len(), 1);
        drop(service);
        cleanup_database(&database);
    }

    #[test]
    fn live_run_outcomes_survive_reopen() {
        let database = std::env::temp_dir().join(format!(
            "orynth-phase-g-interrupted-{}.db",
            RunId::new().value()
        ));
        let run_id = RunId::new();
        let completed_run_id = RunId::new();
        let model = ModelRef::new("mock", "coordinator", ModelClass::Cheap);
        let agent = AgentIdentity::new("COORDINATOR", "coordinate", model.clone());
        let mut store = SqliteEventStore::open(&database).unwrap();
        store
            .append_batch(&[
                Event::new(run_id, EventKind::RunCreated { run_id }),
                Event::new(
                    run_id,
                    EventKind::AgentCreated {
                        agent: agent.clone(),
                    },
                ),
                Event::new(
                    run_id,
                    EventKind::ModelRequested {
                        agent_id: agent.id,
                        model: model.clone(),
                    },
                ),
            ])
            .unwrap();
        store
            .append_batch(&[
                Event::new(
                    completed_run_id,
                    EventKind::RunCreated {
                        run_id: completed_run_id,
                    },
                ),
                Event::new(
                    completed_run_id,
                    EventKind::AgentCreated {
                        agent: agent.clone(),
                    },
                ),
                Event::new(
                    completed_run_id,
                    EventKind::ModelRequested {
                        agent_id: agent.id,
                        model,
                    },
                ),
                Event::new(
                    completed_run_id,
                    EventKind::ModelTurnCompleted {
                        agent_id: agent.id,
                        usage: None,
                    },
                ),
            ])
            .unwrap();
        drop(store);
        finalize_live_run(&database, run_id, true, None).unwrap();
        finalize_live_run(&database, completed_run_id, false, None).unwrap();
        let reopened = SqliteEventStore::open(&database).unwrap();
        assert_eq!(
            reopened.reconstruct(run_id).unwrap().status,
            RunStatus::Cancelled
        );
        assert_eq!(
            reopened.reconstruct(run_id).unwrap().agents[&agent.id].status,
            AgentStatus::Cancelled
        );
        assert_eq!(
            reopened.reconstruct(completed_run_id).unwrap().status,
            RunStatus::Completed
        );
        assert_eq!(
            reopened.reconstruct(completed_run_id).unwrap().agents[&agent.id].status,
            AgentStatus::Completed
        );
        assert!(
            reopened
                .events(run_id)
                .unwrap()
                .iter()
                .any(|event| matches!(event.event.kind, EventKind::ModelRequested { .. }))
        );
        assert!(
            !reopened
                .events(run_id)
                .unwrap()
                .iter()
                .any(|event| matches!(event.event.kind, EventKind::ModelTurnCompleted { .. }))
        );
        drop(reopened);
        let _ = std::fs::remove_file(&database);
        let _ = std::fs::remove_file(database.with_extension("db-wal"));
        let _ = std::fs::remove_file(database.with_extension("db-shm"));
    }

    #[test]
    fn long_conversation_retains_runtime_system_context() {
        let mut parts = vec![
            RequestPart::System("Coordinator policy".into()),
            RequestPart::System("Actual runtime roster".into()),
        ];
        parts.extend((0..80).map(|index| RequestPart::User(format!("message {index}"))));
        bound_conversation_parts(&mut parts);
        assert_eq!(parts.len(), 64);
        assert!(matches!(&parts[0], RequestPart::System(text) if text == "Coordinator policy"));
        assert!(matches!(&parts[1], RequestPart::System(text) if text == "Actual runtime roster"));
        assert!(matches!(&parts[63], RequestPart::User(text) if text == "message 79"));
    }

    #[test]
    fn openrouter_http_stream_smoke_is_fully_offline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let read = socket.read(&mut buffer).unwrap();
                assert!(read > 0);
                request.extend_from_slice(&buffer[..read]);
            }
            let headers = String::from_utf8_lossy(&request).to_ascii_lowercase();
            assert!(headers.starts_with("post /chat/completions "));
            assert!(headers.contains("authorization: bearer local-test-secret"));
            let body = concat!(
                "data: {\"id\":\"gen-test\",\"model\":\"example/free\",\"choices\":[{\"delta\":{\"content\":\"ORYNTH_\"},\"finish_reason\":null}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"CONNECTED\"},\"finish_reason\":null}]}\n\n",
                "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n",
            );
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            socket.flush().unwrap();
        });
        let model = ModelRef::new("openrouter", "openrouter/free", ModelClass::Cheap);
        let provider =
            OpenRouterProvider::new(model.clone(), "local-test-secret".into(), &base_url, None)
                .unwrap();
        let request = ModelRequest::new(
            RunId::new(),
            TaskId::new(),
            AgentId::new(),
            model,
            "Reply with exactly ORYNTH_CONNECTED",
        );
        let events = provider
            .stream(request, CancellationToken::new())
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        server.join().unwrap();
        assert!(events.iter().any(|event| matches!(event, ProviderEvent::ResponseMetadata { resolved_model: Some(model), request_id: Some(id), .. } if model == "example/free" && id == "gen-test")));
        assert_eq!(
            events
                .iter()
                .filter_map(|event| match event {
                    ProviderEvent::TextDelta { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>(),
            "ORYNTH_CONNECTED"
        );
        assert!(events.contains(&ProviderEvent::Finish(FinishReason::Stop)));
    }

    #[test]
    fn openrouter_refuses_http_redirects() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut buffer = [0u8; 4096];
            assert!(socket.read(&mut buffer).unwrap() > 0);
            socket.write_all(b"HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:1/other-host\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let model = ModelRef::new("openrouter", "openrouter/free", ModelClass::Cheap);
        let provider =
            OpenRouterProvider::new(model.clone(), "local-test-secret".into(), &base_url, None)
                .unwrap();
        let request =
            ModelRequest::new(RunId::new(), TaskId::new(), AgentId::new(), model, "hello");
        let result = provider
            .stream(request, CancellationToken::new())
            .unwrap()
            .next();
        server.join().unwrap();
        assert!(
            matches!(result, Some(Err(ProviderError::Failed(message))) if message.contains("307"))
        );
    }

    #[test]
    fn openrouter_tool_probe_decodes_fragmented_call_over_local_http() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            let (header_end, length) = loop {
                let read = socket.read(&mut buffer).unwrap();
                assert!(read > 0);
                request.extend_from_slice(&buffer[..read]);
                if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length: ")
                                .and_then(|value| value.parse::<usize>().ok())
                        })
                        .unwrap();
                    break (end + 4, length);
                }
            };
            while request.len() < header_end + length {
                let read = socket.read(&mut buffer).unwrap();
                assert!(read > 0);
                request.extend_from_slice(&buffer[..read]);
            }
            let payload: serde_json::Value =
                serde_json::from_slice(&request[header_end..header_end + length]).unwrap();
            assert_eq!(payload["tool_choice"], "required");
            assert_eq!(payload["provider"]["require_parameters"], true);
            assert_eq!(payload["tools"][0]["function"]["name"], "orynth_tool_probe");
            let chunks = [
                serde_json::json!({"id":"gen-tool","model":"example/free","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"orynth_tool_probe","arguments":"{\"value\":\"ORYNTH_"}}]},"finish_reason":null}]}),
                serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"TOOLS_OK\"}"}}]},"finish_reason":null}]}),
                serde_json::json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
            ];
            let mut body = chunks
                .iter()
                .map(|chunk| format!("data: {chunk}\n\n"))
                .collect::<String>();
            body.push_str("data: [DONE]\n\n");
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            socket.flush().unwrap();
        });
        let model = ModelRef::new("openrouter", "openrouter/free", ModelClass::Cheap);
        let provider =
            OpenRouterProvider::new(model.clone(), "local-test-secret".into(), &base_url, None)
                .unwrap();
        let result = crate::run_tool_probe(&provider, model).unwrap();
        server.join().unwrap();
        assert_eq!(result.0.as_deref(), Some("example/free"));
    }

    #[test]
    fn coordinator_remains_available_across_completed_provider_turns() {
        let database =
            std::env::temp_dir().join(format!("orynth-phase-g-turns-{}.db", RunId::new().value()));
        let run = Run::new();
        let task = Task::new(run.id, "conversation");
        let model = ModelRef::new("mock", "coordinator", ModelClass::Cheap);
        let agent = AgentIdentity::new("COORDINATOR", "coordinate", model.clone());
        let store = SqliteEventStore::open(&database).unwrap();
        let mut service = RuntimeService::new(store);
        service
            .event_store_mut()
            .append_batch(&[
                Event::new(run.id, EventKind::RunCreated { run_id: run.id }),
                Event::new(
                    run.id,
                    EventKind::TaskCreated {
                        task_id: task.id,
                        run_id: run.id,
                        title: task.title,
                    },
                ),
                Event::new(
                    run.id,
                    EventKind::AgentCreated {
                        agent: agent.clone(),
                    },
                ),
            ])
            .unwrap();
        let shared = Arc::new(Mutex::new(LiveState::default()));
        for (input, answer) in [
            ("Hello", "Hi"),
            ("What agents are active?", "COORDINATOR"),
            ("Summarize the task", "The task is active."),
            ("What remains?", "Continue the requested work."),
            (
                "Give the current status",
                "The Coordinator remains available.",
            ),
        ] {
            service
                .record_conversation_turn(run.id, ConversationTurn::user(input))
                .unwrap();
            let request = ModelRequest::new(run.id, task.id, agent.id, model.clone(), input);
            let provider = MockProvider::new(model.clone(), answer);
            let (output, calls, finish, usage) = run_coordinator_request(
                &mut service,
                &provider,
                request,
                CancellationToken::new(),
                &shared,
            )
            .unwrap();
            assert_eq!(output, answer);
            assert!(calls.is_empty());
            assert_eq!(finish, FinishReason::Stop);
            assert!(usage.is_some());
            service
                .record_conversation_turn(run.id, ConversationTurn::coordinator(agent.id, output))
                .unwrap();
            assert_eq!(
                service.recover(run.id).unwrap().state.agents[&agent.id].status,
                AgentStatus::Running
            );
        }
        let recovered = service.recover(run.id).unwrap();
        assert_eq!(
            recovered
                .events
                .iter()
                .filter(|stored| matches!(stored.event.kind, EventKind::ConversationTurn { .. }))
                .count(),
            10
        );
        assert_eq!(
            recovered
                .events
                .iter()
                .filter(|stored| matches!(stored.event.kind, EventKind::ModelTurnCompleted { .. }))
                .count(),
            5
        );
        assert!(recovered.state.agents[&agent.id].usage.total_tokens() > 0);
        for _ in 2..MAX_LIVE_PROVIDER_REQUESTS {
            service
                .record_model_transition(
                    run.id,
                    EventKind::ModelRequested {
                        agent_id: agent.id,
                        model: model.clone(),
                    },
                )
                .unwrap();
            service
                .record_model_transition(
                    run.id,
                    EventKind::ModelTurnCompleted {
                        agent_id: agent.id,
                        usage: None,
                    },
                )
                .unwrap();
        }
        let denied = run_coordinator_request(
            &mut service,
            &MockProvider::new(model.clone(), "should not be sent"),
            ModelRequest::new(run.id, task.id, agent.id, model, "one too many"),
            CancellationToken::new(),
            &shared,
        );
        assert!(matches!(denied, Err(message) if message.contains("request limit reached")));
        drop(service);
        let _ = std::fs::remove_file(&database);
        let _ = std::fs::remove_file(database.with_extension("db-wal"));
        let _ = std::fs::remove_file(database.with_extension("db-shm"));
    }
}
