use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use orynth_context::{ContextPrincipal, ProjectionRequest, PromptLayer};
use orynth_event_store::SqliteEventStore;
use orynth_ipc::{IpcEnvelope, IpcMessage, IpcProvenance};
use orynth_kernel::{
    AgentId, AgentIdentity, CancellationToken, EventKind, ModelRef, RunId, TaskId, Usage,
};
use orynth_provider::{
    FinishReason, ModelProvider, ModelRequest, ProviderEvent, RequestPart, ToolChoice,
    ToolDefinition as ProviderToolDefinition, openrouter::OpenRouterProvider,
};
use orynth_runtime::RuntimeService;
use orynth_scheduler::{BudgetLimits, BudgetUsage};
use orynth_security::{CapabilityDomain, CapabilityLease, ExclusiveOwnershipPolicy};
use orynth_terminal_tools::{FILESYSTEM_WRITE_TOOL, FilesystemFixture, write_text_definition};
use orynth_tool_runtime::{
    ApprovalSource, ToolProposal, ToolProvenance, ToolRuntime, ToolState, ToolTransition,
};

use super::ensure_provider_request_budget;

const MAX_WORKER_CALLS: usize = 4;
const MAX_WORKER_REQUESTS: usize = 4;
const MAX_UNPRODUCTIVE_TURNS: usize = 3;
const MAX_TURN_RETRIES: usize = 2;
const WIRE_WRITE_TOOL: &str = "filesystem_write_text";

pub(super) fn small_site_task(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    (lower.contains("website") || lower.contains("web page") || lower.contains("webpage"))
        && (lower.contains("make ") || lower.contains("create ") || lower.contains("build "))
}

pub(super) struct WorkerTask<'a> {
    pub run_id: RunId,
    pub task_id: TaskId,
    pub coordinator_id: AgentId,
    pub model: ModelRef,
    pub base_url: &'a str,
    pub title: Option<String>,
    pub workspace: &'a Path,
    pub user_text: &'a str,
    pub cancellation: CancellationToken,
    pub effect_gate: Arc<Mutex<()>>,
}

pub(super) fn run(
    service: &mut RuntimeService<SqliteEventStore>,
    task: WorkerTask<'_>,
) -> Result<(String, Usage), String> {
    let provider =
        OpenRouterProvider::from_env(task.model.clone(), task.base_url, task.title.clone())
            .map_err(|error| error.to_string())?;
    run_with_provider(service, task, &provider)
}

fn run_with_provider(
    service: &mut RuntimeService<SqliteEventStore>,
    task: WorkerTask<'_>,
    provider: &dyn ModelProvider,
) -> Result<(String, Usage), String> {
    let WorkerTask {
        run_id,
        task_id,
        coordinator_id,
        model,
        base_url,
        title,
        workspace,
        user_text,
        cancellation,
        effect_gate,
    } = task;
    if cancellation.is_cancelled() {
        return Err("worker cancelled".into());
    }
    let child = AgentIdentity::new(
        "SITE-01",
        "Create a tiny personal site in the selected workspace",
        model.clone(),
    );
    service
        .spawn_agent(run_id, coordinator_id, child.clone())
        .map_err(|error| error.to_string())?;
    let result = (|| {
        service
            .send_message(
                IpcEnvelope::new(
                    run_id,
                    Some(task_id),
                    coordinator_id,
                    child.id,
                    IpcMessage::ReviewRequest {
                        subject: "tiny personal website".into(),
                        instructions: user_text.into(),
                    },
                )
                .with_provenance(IpcProvenance::Runtime),
            )
            .map_err(|error| error.to_string())?;
        service
            .configure_budget(
                run_id,
                child.id,
                BudgetLimits {
                    max_tokens: Some(20_000),
                    max_tool_calls: Some(MAX_WORKER_CALLS as u64),
                    max_wall_clock_ms: Some(180_000),
                    max_child_agents: Some(0),
                    ..BudgetLimits::default()
                },
            )
            .map_err(|error| error.to_string())?;
        service
            .grant_capability(
                run_id,
                CapabilityLease {
                    agent_id: child.id,
                    task_id: Some(task_id),
                    domain: CapabilityDomain::Filesystem,
                    resource: ".".into(),
                    expires_at_ms: u128::MAX,
                },
            )
            .map_err(|error| error.to_string())?;
        let mut fixture = FilesystemFixture::new(workspace).map_err(|error| error.to_string())?;
        let _ = (base_url, title);
        let system = "You are SITE-01, an Orynth coding worker. Create a tiny personal website using only the filesystem_write_text tool. Files must be index.html and optionally styles.css in the current workspace. Use no external resources, shell, scripts, or hosted tools. Keep each file under 4 KiB. After writing, give a short factual summary.";
        let recovered = service.recover(run_id).map_err(|error| error.to_string())?;
        let projected = recovered.context.project(
            ContextPrincipal::Agent(child.id),
            &ProjectionRequest {
                max_blocks: Some(4),
                max_tokens: Some(2_000),
                ..ProjectionRequest::all()
            },
        );
        let references = projected
            .blocks
            .iter()
            .map(|entry| entry.block.reference())
            .collect();
        let context_prompt = recovered.context.render_prompt(
            ContextPrincipal::Agent(child.id),
            &[PromptLayer::stable("worker context", references)],
            &format!(
                "Runtime facts: selected coding workspace is {}; only index.html and styles.css may be written.",
                workspace.display()
            ),
        ).map_err(|error| error.to_string())?;
        let mut next_parts = vec![
            RequestPart::System(system.into()),
            RequestPart::System(context_prompt.text),
            RequestPart::User(user_text.into()),
        ];
        let mut total_usage = Usage::default();
        let mut saw_usage = false;
        let mut tool_count = 0usize;
        let mut wrote_index = false;
        let mut unproductive_turns = 0usize;
        let started = Instant::now();
        let mut final_text = String::new();
        for _ in 0..MAX_WORKER_REQUESTS {
            if cancellation.is_cancelled() {
                return Err("worker cancelled".into());
            }
            if started.elapsed().as_millis() > 180_000 {
                return Err("worker wall-clock budget exceeded".into());
            }
            let mut request = ModelRequest::new(run_id, task_id, child.id, model.clone(), "");
            request.parts = next_parts.clone();
            request.max_output_tokens = Some(2048);
            if !wrote_index {
                request.tool_choice = ToolChoice::Required;
            }
            request.tools = vec![ProviderToolDefinition {
            name: WIRE_WRITE_TOOL.into(),
            description: "Write one UTF-8 file inside Orynth's selected workspace".into(),
            input_schema: r#"{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"],"additionalProperties":false}"#.into(),
        }];
            ensure_provider_request_budget(service, run_id)?;
            service
                .record_model_transition(
                    run_id,
                    EventKind::ModelRequested {
                        agent_id: child.id,
                        model: model.clone(),
                    },
                )
                .map_err(|error| error.to_string())?;
            let stream = worker_stream_with_retry(provider, request, cancellation.clone())?;
            let mut calls = Vec::new();
            let mut chunk_index = 0u32;
            let mut usage = None;
            let mut finish = None;
            let mut text = String::new();
            for item in stream {
                match item.map_err(|error| error.to_string())? {
                    ProviderEvent::TextDelta { text: delta } => {
                        if text.len().saturating_add(delta.len()) > 16 * 1024 {
                            return Err("worker output exceeds limit".into());
                        }
                        text.push_str(&delta);
                        service
                            .record_model_transition(
                                run_id,
                                EventKind::ModelChunkReceived {
                                    agent_id: child.id,
                                    chunk_index,
                                },
                            )
                            .map_err(|error| error.to_string())?;
                        chunk_index = chunk_index.saturating_add(1);
                    }
                    ProviderEvent::ToolCallCompleted { call } => calls.push(call),
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
                                    agent_id: child.id,
                                    resolved_model,
                                    provider_request_id: request_id,
                                    provider_name,
                                },
                            )
                            .map_err(|error| error.to_string())?;
                    }
                    ProviderEvent::Finish(reason) => finish = Some(reason),
                    _ => {}
                }
            }
            let Some(finish) = finish else {
                return Err("worker provider stream ended early".into());
            };
            if cancellation.is_cancelled() {
                return Err("worker cancelled".into());
            }
            if started.elapsed().as_millis() > 180_000 {
                return Err("worker wall-clock budget exceeded".into());
            }
            service
                .record_model_transition(
                    run_id,
                    EventKind::ModelTurnCompleted {
                        agent_id: child.id,
                        usage,
                    },
                )
                .map_err(|error| error.to_string())?;
            service
                .record_model_transition(
                    run_id,
                    EventKind::ModelTurnOutcome {
                        agent_id: child.id,
                        finish: match &finish {
                            FinishReason::Stop => "stop".into(),
                            FinishReason::ToolCall => "tool_calls".into(),
                            FinishReason::Length => "length".into(),
                            FinishReason::Cancelled => "cancelled".into(),
                            FinishReason::ContentFilter => "content_filter".into(),
                            FinishReason::Other(value) => value.clone(),
                        },
                        continued: finish != FinishReason::Stop,
                        unproductive: finish == FinishReason::Length
                            && text.trim().is_empty()
                            && calls.is_empty(),
                    },
                )
                .map_err(|error| error.to_string())?;
            if let Some(usage) = usage {
                saw_usage = true;
                total_usage = total_usage.saturating_add(usage);
                service
                    .record_agent_usage(
                        run_id,
                        child.id,
                        BudgetUsage {
                            tokens: usage.total_tokens(),
                            ..BudgetUsage::default()
                        },
                    )
                    .map_err(|error| error.to_string())?;
            }
            if calls.is_empty() {
                if finish == FinishReason::Stop {
                    final_text = text;
                    break;
                }
                if finish == FinishReason::Length {
                    if !text.trim().is_empty() {
                        next_parts.push(RequestPart::Model(text));
                        unproductive_turns = 0;
                    } else {
                        unproductive_turns = unproductive_turns.saturating_add(1);
                    }
                    if unproductive_turns >= MAX_UNPRODUCTIVE_TURNS {
                        return Err("worker produced repeated empty length turns".into());
                    }
                    next_parts.push(RequestPart::System(
                        "The previous worker turn reached its output limit. Continue from the current workspace state. Return only the next required filesystem action or a concise factual summary.".into(),
                    ));
                    continue;
                }
                let label = match finish {
                    FinishReason::Cancelled => "cancelled".to_owned(),
                    FinishReason::ContentFilter => "content_filter".to_owned(),
                    FinishReason::Other(value) => value,
                    FinishReason::ToolCall => "tool_calls without a tool call".to_owned(),
                    FinishReason::Stop => "stop".to_owned(),
                    FinishReason::Length => "length".to_owned(),
                };
                return Err(format!("worker response ended with {label}"));
            }
            if finish != FinishReason::ToolCall {
                if finish == FinishReason::Length {
                    // A complete action is usable even when the provider reports
                    // that the surrounding model turn exhausted its allowance.
                } else {
                    return Err("worker returned tool calls without a tool-call finish".into());
                }
            }
            unproductive_turns = 0;
            if tool_count.saturating_add(calls.len()) > MAX_WORKER_CALLS {
                return Err("worker tool-call budget exceeded".into());
            }
            next_parts.push(RequestPart::ModelToolCalls(calls.clone()));
            for call in calls {
                if cancellation.is_cancelled() {
                    return Err("worker cancelled".into());
                }
                if started.elapsed().as_millis() > 180_000 {
                    return Err("worker wall-clock budget exceeded".into());
                }
                if call.name != WIRE_WRITE_TOOL {
                    return Err("worker requested an unavailable tool".into());
                }
                let args: serde_json::Value = serde_json::from_str(&call.arguments)
                    .map_err(|_| "worker supplied invalid tool JSON")?;
                let path = args["path"].as_str().ok_or("tool path missing")?;
                let content = args["content"].as_str().ok_or("tool content missing")?;
                if !matches!(path, "index.html" | "styles.css") {
                    return Err("worker may write only index.html or styles.css".into());
                }
                if content.len() > 4096 || content.is_empty() {
                    return Err("worker file content is empty or too large".into());
                }
                let lower_content = content.to_ascii_lowercase();
                if path == "index.html"
                    && (!content.contains("Arpi") || !lower_content.contains("<p"))
                {
                    return Err("index.html must contain Arpi and a paragraph".into());
                }
                if lower_content.contains("<script")
                    || lower_content.contains("<iframe")
                    || lower_content.contains("https://")
                    || lower_content.contains("http://")
                    || lower_content.contains("@import")
                    || lower_content.contains("url(")
                {
                    return Err("personal site must not need external resources or scripts".into());
                }
                if fixture.root().join(path).exists() {
                    return Err(format!("{path} already exists; refusing overwrite"));
                }
                service
                    .claim_ownership(run_id, child.id, path)
                    .map_err(|error| error.to_string())?;
                let mut tools = ToolRuntime::new();
                tools
                    .register(write_text_definition())
                    .map_err(|error| error.to_string())?;
                tools
                    .capabilities_mut()
                    .grant(CapabilityLease {
                        agent_id: child.id,
                        task_id: Some(task_id),
                        domain: CapabilityDomain::Filesystem,
                        resource: path.into(),
                        expires_at_ms: u128::MAX,
                    })
                    .map_err(|error| error.to_string())?;
                let ownership = ExclusiveOwnershipPolicy::new(child.id, path)
                    .map_err(|error| error.to_string())?;
                let proposal = ToolProposal {
                    run_id,
                    task_id: Some(task_id),
                    agent_id: child.id,
                    tool_name: FILESYSTEM_WRITE_TOOL.into(),
                    input: BTreeMap::from([
                        ("path".into(), path.into()),
                        ("content".into(), content.into()),
                    ]),
                    provenance: ToolProvenance::Agent,
                    input_origins: Vec::new(),
                };
                let mut tx = tools
                    .validate_with_ownership(&proposal, now_ms(), &ownership)
                    .map_err(|error| error.to_string())?;
                service
                    .record_tool_transition(run_id, ToolTransition::proposed(&tx))
                    .map_err(|error| error.to_string())?;
                let preview = tools
                    .preview(&tx, &fixture)
                    .map_err(|error| error.to_string())?;
                service
                    .record_tool_transition(
                        run_id,
                        ToolTransition::previewed(tx.id, preview)
                            .map_err(|error| error.to_string())?,
                    )
                    .map_err(|error| error.to_string())?;
                if tx.state == ToolState::AwaitingApproval {
                    tools
                        .approve(&mut tx, ApprovalSource::Policy)
                        .map_err(|error| error.to_string())?;
                    service
                        .record_tool_transition(
                            run_id,
                            ToolTransition::state_changed(tx.id, tx.state, None)
                                .map_err(|error| error.to_string())?,
                        )
                        .map_err(|error| error.to_string())?;
                }
                let _effect_guard = effect_gate
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if cancellation.is_cancelled() {
                    return Err("worker cancelled".into());
                }
                tools
                    .execute_with_ownership(&mut tx, &mut fixture, &ownership)
                    .map_err(|error| error.to_string())?;
                service
                    .record_tool_transition(
                        run_id,
                        ToolTransition::state_changed(tx.id, tx.state, None)
                            .map_err(|error| error.to_string())?,
                    )
                    .map_err(|error| error.to_string())?;
                tools
                    .verify(&mut tx, &fixture)
                    .map_err(|error| error.to_string())?;
                service
                    .record_tool_transition(
                        run_id,
                        ToolTransition::state_changed(tx.id, tx.state, None)
                            .map_err(|error| error.to_string())?,
                    )
                    .map_err(|error| error.to_string())?;
                tools.commit(&mut tx).map_err(|error| error.to_string())?;
                service
                    .record_tool_transition(
                        run_id,
                        ToolTransition::state_changed(tx.id, tx.state, None)
                            .map_err(|error| error.to_string())?,
                    )
                    .map_err(|error| error.to_string())?;
                service
                    .record_agent_usage(
                        run_id,
                        child.id,
                        BudgetUsage {
                            tool_calls: 1,
                            ..BudgetUsage::default()
                        },
                    )
                    .map_err(|error| error.to_string())?;
                if path == "index.html" {
                    wrote_index = true;
                }
                tool_count += 1;
                next_parts.push(RequestPart::ToolResult {
                    call_id: call.call_id,
                    content: format!("Created and verified {path} in Orynth workspace"),
                    is_error: false,
                });
            }
        }
        if !wrote_index {
            return Err("worker did not create index.html".into());
        }
        if final_text.trim().is_empty() {
            return Err("worker did not provide a final summary".into());
        }
        service
            .record_agent_usage(
                run_id,
                child.id,
                BudgetUsage {
                    wall_clock_ms: started.elapsed().as_millis() as u64,
                    ..BudgetUsage::default()
                },
            )
            .map_err(|error| error.to_string())?;
        if saw_usage {
            service
                .record_model_transition(
                    run_id,
                    EventKind::ModelCompleted {
                        agent_id: child.id,
                        usage: total_usage,
                    },
                )
                .map_err(|error| error.to_string())?;
        } else {
            service
                .record_model_transition(
                    run_id,
                    EventKind::ModelFinishedWithoutUsage { agent_id: child.id },
                )
                .map_err(|error| error.to_string())?;
        }
        let summary = format!(
            "Worker created and verified {} file(s) in the selected Orynth workspace: index.html{}.",
            tool_count,
            if fixture.root().join("styles.css").exists() {
                ", styles.css"
            } else {
                ""
            },
        );
        service
            .send_message(IpcEnvelope::new(
                run_id,
                Some(task_id),
                child.id,
                coordinator_id,
                IpcMessage::Handoff {
                    summary: summary.clone(),
                },
            ))
            .map_err(|error| error.to_string())?;
        Ok((summary, total_usage))
    })();
    if let Err(message) = &result {
        let kind = if cancellation.is_cancelled() {
            EventKind::ModelCancelled { agent_id: child.id }
        } else {
            EventKind::ModelFailed {
                agent_id: child.id,
                message: message.clone(),
            }
        };
        let _ = service.record_model_transition(run_id, kind);
    }
    result
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn worker_stream_with_retry(
    provider: &dyn ModelProvider,
    request: ModelRequest,
    cancellation: CancellationToken,
) -> Result<
    Box<dyn Iterator<Item = Result<ProviderEvent, orynth_provider::ProviderError>> + Send>,
    String,
> {
    let mut attempts = 0usize;
    loop {
        match provider.stream(request.clone(), cancellation.clone()) {
            Ok(stream) => return Ok(stream),
            Err(orynth_provider::ProviderError::Timeout) if attempts < MAX_TURN_RETRIES => {
                if cancellation.is_cancelled() {
                    return Err("worker cancelled".into());
                }
                attempts = attempts.saturating_add(1);
                std::thread::sleep(std::time::Duration::from_millis(
                    50u64.saturating_mul(attempts as u64),
                ));
            }
            Err(orynth_provider::ProviderError::RateLimited { .. })
                if attempts < MAX_TURN_RETRIES =>
            {
                if cancellation.is_cancelled() {
                    return Err("worker cancelled".into());
                }
                attempts = attempts.saturating_add(1);
                std::thread::sleep(std::time::Duration::from_millis(
                    50u64.saturating_mul(attempts as u64),
                ));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orynth_event_store::EventStore;
    use orynth_kernel::{Event, ModelClass, Run, Task};
    use orynth_provider::{ProviderCapabilities, ProviderError, ProviderUsage, ToolCall};
    use std::{
        collections::VecDeque,
        sync::{
            Mutex,
            atomic::{AtomicU64, Ordering},
        },
    };

    static NEXT_TEST: AtomicU64 = AtomicU64::new(1);

    struct SequenceProvider {
        model: ModelRef,
        scripts: Mutex<VecDeque<Vec<ProviderEvent>>>,
    }
    impl ModelProvider for SequenceProvider {
        fn model(&self) -> &ModelRef {
            &self.model
        }
        fn capabilities(&self) -> ProviderCapabilities {
            orynth_provider::MockProvider::new(self.model.clone(), "").capabilities()
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
            if cancellation.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            let mut scripts = self.scripts.lock().unwrap();
            if !request
                .parts
                .iter()
                .any(|part| matches!(part, RequestPart::ToolResult { .. }))
            {
                assert_eq!(request.tool_choice, ToolChoice::Required);
                assert_eq!(request.tools[0].name, WIRE_WRITE_TOOL);
            } else {
                assert_eq!(request.tool_choice, ToolChoice::Auto);
                assert!(
                    request
                        .parts
                        .iter()
                        .any(|part| matches!(part, RequestPart::ToolResult { .. }))
                );
            }
            let script = scripts.pop_front().expect("unexpected provider request");
            Ok(Box::new(script.into_iter().map(Ok)))
        }
    }

    fn seeded_service(
        label: &str,
    ) -> (
        RuntimeService<SqliteEventStore>,
        RunId,
        TaskId,
        AgentId,
        std::path::PathBuf,
    ) {
        let id = NEXT_TEST.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "orynth-phase-g-{label}-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        let run = Run::new();
        let task = Task::new(run.id, "tiny website");
        let coordinator = AgentIdentity::new(
            "COORDINATOR",
            "coordinate",
            ModelRef::new("mock", "site", ModelClass::Cheap),
        );
        let mut service =
            RuntimeService::new(SqliteEventStore::open(root.join("events.db")).unwrap());
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
                        agent: coordinator.clone(),
                    },
                ),
            ])
            .unwrap();
        service
            .configure_budget(
                run.id,
                coordinator.id,
                BudgetLimits {
                    max_child_agents: Some(1),
                    ..BudgetLimits::default()
                },
            )
            .unwrap();
        (service, run.id, task.id, coordinator.id, root)
    }

    #[test]
    fn mock_provider_writes_site_only_through_verified_tool_transaction() {
        let (mut service, run_id, task_id, coordinator_id, root) = seeded_service("site");
        let model = ModelRef::new("mock", "site", ModelClass::Cheap);
        let html = "<!doctype html><html><head><style>body{min-height:100vh;display:grid;place-items:center;text-align:center}</style></head><body><main><h1>Arpi</h1><p>Hello there.</p></main></body></html>";
        let args = serde_json::json!({"path":"index.html","content":html}).to_string();
        let call = ToolCall {
            call_id: "call-1".into(),
            name: WIRE_WRITE_TOOL.into(),
            arguments: args.clone(),
        };
        let provider = SequenceProvider {
            model: model.clone(),
            scripts: Mutex::new(VecDeque::from([
                vec![
                    ProviderEvent::TextDelta {
                        text: "I will prepare the requested page.".into(),
                    },
                    ProviderEvent::Finish(FinishReason::Length),
                ],
                vec![
                    ProviderEvent::ToolCallStarted {
                        call_id: call.call_id.clone(),
                        name: call.name.clone(),
                    },
                    ProviderEvent::ToolCallArgumentsDelta {
                        call_id: call.call_id.clone(),
                        delta: args,
                    },
                    ProviderEvent::ToolCallCompleted { call },
                    // The action is complete even though the surrounding
                    // model turn exhausts its output allowance.
                    ProviderEvent::Finish(FinishReason::Length),
                ],
                vec![
                    ProviderEvent::TextDelta {
                        text: "Done. I also deployed the site publicly.".into(),
                    },
                    ProviderEvent::Finish(FinishReason::Stop),
                ],
            ])),
        };
        let result = run_with_provider(
            &mut service,
            WorkerTask {
                run_id,
                task_id,
                coordinator_id,
                model,
                base_url: "",
                title: None,
                workspace: &root,
                user_text: "Make a simple personal website for Arpi",
                cancellation: CancellationToken::new(),
                effect_gate: Arc::new(Mutex::new(())),
            },
            &provider,
        )
        .unwrap();
        assert!(result.0.contains("index.html"));
        assert!(!result.0.contains("deployed"));
        assert_eq!(
            std::fs::read_to_string(root.join("index.html")).unwrap(),
            html
        );
        let recovered = service.recover(run_id).unwrap();
        assert_eq!(recovered.manager.agents.len(), 2);
        assert!(
            recovered
                .tools
                .records()
                .values()
                .any(|record| record.state == ToolState::Committed)
        );
        let worker = recovered
            .manager
            .agents
            .values()
            .find(|agent| agent.name == "SITE-01")
            .unwrap();
        assert!(worker.owned_resources.contains(&"index.html".to_owned()));
        assert!(recovered.messages.iter().any(|message| matches!(&message.payload, IpcMessage::Handoff { summary } if summary.contains("created and verified") && !summary.contains("deployed"))));
        drop(service);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn mock_provider_cannot_write_outside_site_workspace() {
        let (mut service, run_id, task_id, coordinator_id, root) = seeded_service("outside");
        let model = ModelRef::new("mock", "site", ModelClass::Cheap);
        let call = ToolCall {
            call_id: "call-1".into(),
            name: WIRE_WRITE_TOOL.into(),
            arguments: r#"{"path":"../outside.html","content":"<h1>Arpi</h1><p>Hello</p>"}"#.into(),
        };
        let provider = SequenceProvider {
            model: model.clone(),
            scripts: Mutex::new(VecDeque::from([vec![
                ProviderEvent::ToolCallCompleted { call },
                ProviderEvent::Finish(FinishReason::ToolCall),
            ]])),
        };
        let result = run_with_provider(
            &mut service,
            WorkerTask {
                run_id,
                task_id,
                coordinator_id,
                model,
                base_url: "",
                title: None,
                workspace: &root,
                user_text: "Make a simple personal website for Arpi",
                cancellation: CancellationToken::new(),
                effect_gate: Arc::new(Mutex::new(())),
            },
            &provider,
        );
        assert!(result.is_err());
        assert!(!root.parent().unwrap().join("outside.html").exists());
        assert!(service.recover(run_id).unwrap().tools.records().is_empty());
        drop(service);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn worker_replay_keeps_first_turn_usage_after_later_failure() {
        let (mut service, run_id, task_id, coordinator_id, root) = seeded_service("usage-failure");
        let model = ModelRef::new("mock", "site", ModelClass::Cheap);
        let html = "<!doctype html><html><body><h1>Arpi</h1><p>Hello.</p></body></html>";
        let call = ToolCall {
            call_id: "call-1".into(),
            name: WIRE_WRITE_TOOL.into(),
            arguments: serde_json::json!({"path":"index.html","content":html}).to_string(),
        };
        let reported = Usage::new(9, 4);
        let provider = SequenceProvider {
            model: model.clone(),
            scripts: Mutex::new(VecDeque::from([
                vec![
                    ProviderEvent::ToolCallCompleted { call },
                    ProviderEvent::Usage(ProviderUsage {
                        usage: reported,
                        cost: None,
                        prompt_cache_hit: None,
                    }),
                    ProviderEvent::Finish(FinishReason::ToolCall),
                ],
                vec![ProviderEvent::Finish(FinishReason::Stop)],
            ])),
        };
        let result = run_with_provider(
            &mut service,
            WorkerTask {
                run_id,
                task_id,
                coordinator_id,
                model,
                base_url: "",
                title: None,
                workspace: &root,
                user_text: "Make a simple personal website for Arpi",
                cancellation: CancellationToken::new(),
                effect_gate: Arc::new(Mutex::new(())),
            },
            &provider,
        );
        assert!(matches!(result, Err(message) if message.contains("final summary")));
        let recovered = service.recover(run_id).unwrap();
        let worker = recovered
            .state
            .agents
            .values()
            .find(|agent| agent.identity.name == "SITE-01")
            .unwrap();
        assert_eq!(worker.status, orynth_event_store::AgentStatus::Failed);
        assert_eq!(worker.usage, reported);
        assert!(recovered.events.iter().any(|stored| matches!(stored.event.kind, EventKind::ModelTurnCompleted { usage: Some(value), .. } if value == reported)));
        drop(service);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shutdown_cancellation_prevents_waiting_tool_effect() {
        let (mut service, run_id, task_id, coordinator_id, root) = seeded_service("cancel-effect");
        let model = ModelRef::new("mock", "site", ModelClass::Cheap);
        let html = "<!doctype html><html><body><h1>Arpi</h1><p>Hello.</p></body></html>";
        let call = ToolCall {
            call_id: "call-1".into(),
            name: WIRE_WRITE_TOOL.into(),
            arguments: serde_json::json!({"path":"index.html","content":html}).to_string(),
        };
        let provider = SequenceProvider {
            model: model.clone(),
            scripts: Mutex::new(VecDeque::from([vec![
                ProviderEvent::ToolCallCompleted { call },
                ProviderEvent::Finish(FinishReason::ToolCall),
            ]])),
        };
        let gate = Arc::new(Mutex::new(()));
        let held = gate.lock().unwrap();
        let worker_gate = Arc::clone(&gate);
        let cancellation = CancellationToken::new();
        let worker_root = root.clone();
        let worker_cancellation = cancellation.clone();
        let handle = std::thread::spawn(move || {
            let result = run_with_provider(
                &mut service,
                WorkerTask {
                    run_id,
                    task_id,
                    coordinator_id,
                    model,
                    base_url: "",
                    title: None,
                    workspace: &worker_root,
                    user_text: "Make a simple personal website for Arpi",
                    cancellation: worker_cancellation,
                    effect_gate: worker_gate,
                },
                &provider,
            );
            (result, service)
        });
        let observer = SqliteEventStore::open(root.join("events.db")).unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if observer
                .events(run_id)
                .unwrap()
                .iter()
                .any(|stored| matches!(stored.event.kind, EventKind::ToolTransition { .. }))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "worker did not reach tool runtime"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        cancellation.cancel();
        drop(held);
        let (result, service) = handle.join().unwrap();
        assert!(matches!(result, Err(message) if message.contains("cancelled")));
        assert!(!root.join("index.html").exists());
        assert!(
            service
                .recover(run_id)
                .unwrap()
                .tools
                .records()
                .values()
                .all(|record| record.state != ToolState::Committed)
        );
        drop(service);
        drop(observer);
        assert!(
            root.canonicalize()
                .unwrap()
                .starts_with(std::env::temp_dir().canonicalize().unwrap())
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
