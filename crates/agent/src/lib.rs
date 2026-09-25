use std::{collections::BTreeSet, fmt};

use orynth_kernel::{
    AgentIdentity, CancellationToken, Event, EventKind, EventTrace, Run, Task, Usage,
};
use orynth_provider::{
    FinishReason, MAX_OUTPUT_CHUNKS, MAX_PROVIDER_PAYLOAD_BYTES, MAX_TOOL_CALLS, ModelProvider,
    ModelRequest, ProviderError, ProviderEvent, ProviderUsage, ToolCall,
};

pub const MAX_AGENT_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

pub struct AgentSession<P> {
    identity: AgentIdentity,
    provider: P,
}

impl<P> AgentSession<P>
where
    P: ModelProvider,
{
    pub fn new(identity: AgentIdentity, provider: P) -> Self {
        Self { identity, provider }
    }

    pub fn identity(&self) -> &AgentIdentity {
        &self.identity
    }

    pub fn run(
        &self,
        prompt: impl Into<String>,
        cancellation: CancellationToken,
    ) -> Result<AgentExecution, AgentError> {
        let prompt = prompt.into();
        let run = Run::new();
        let task = Task::new(run.id, prompt.clone());
        let mut trace = EventTrace::default();
        trace.record(Event::new(run.id, EventKind::RunCreated { run_id: run.id }));
        trace.record(Event::new(
            run.id,
            EventKind::TaskCreated {
                task_id: task.id,
                run_id: run.id,
                title: task.title.clone(),
            },
        ));
        trace.record(Event::new(
            run.id,
            EventKind::AgentCreated {
                agent: self.identity.clone(),
            },
        ));
        self.execute(run, task, prompt, trace, cancellation)
    }

    pub fn run_fork(
        &self,
        run_id: orynth_kernel::RunId,
        task: Task,
        prompt: impl Into<String>,
        prefix: EventTrace,
        cancellation: CancellationToken,
    ) -> Result<AgentExecution, AgentError> {
        if let Err(message) = validate_fork_prefix(&prefix, run_id, &task, self.identity.id) {
            return Err(AgentError::InvalidFork {
                message,
                trace: prefix,
            });
        }
        self.execute(
            Run { id: run_id },
            task,
            prompt.into(),
            prefix,
            cancellation,
        )
    }

    fn execute(
        &self,
        run: Run,
        task: Task,
        prompt: String,
        mut trace: EventTrace,
        cancellation: CancellationToken,
    ) -> Result<AgentExecution, AgentError> {
        let request = ModelRequest::new(
            run.id,
            task.id,
            self.identity.id,
            self.identity.model.clone(),
            prompt,
        );
        trace.record(Event::new(
            run.id,
            EventKind::ModelRequested {
                agent_id: self.identity.id,
                model: request.model.clone(),
            },
        ));

        let stream = match self.provider.stream(request, cancellation.clone()) {
            Ok(stream) => stream,
            Err(error) => return Err(self.failed(run.id, trace, error)),
        };

        let mut output_chunks = Vec::new();
        let mut retained_output_bytes = 0usize;
        let mut usage = Usage::default();
        let mut received_finish = false;
        let mut active_tool_calls = BTreeSet::new();
        let mut tool_calls = Vec::new();
        let mut next_chunk_index = trace
            .events()
            .iter()
            .filter(|event| matches!(event.kind, EventKind::ModelChunkReceived { .. }))
            .count() as u32;

        for item in stream {
            if cancellation.is_cancelled() {
                trace.record(Event::new(
                    run.id,
                    EventKind::ModelCancelled {
                        agent_id: self.identity.id,
                    },
                ));
                trace.record(Event::new(
                    run.id,
                    EventKind::RunCancelled { run_id: run.id },
                ));
                return Err(AgentError::Cancelled { trace });
            }

            let event = match item {
                Ok(event) => event,
                Err(ProviderError::Cancelled) => {
                    trace.record(Event::new(
                        run.id,
                        EventKind::ModelCancelled {
                            agent_id: self.identity.id,
                        },
                    ));
                    trace.record(Event::new(
                        run.id,
                        EventKind::RunCancelled { run_id: run.id },
                    ));
                    return Err(AgentError::Cancelled { trace });
                }
                Err(error) => return Err(self.failed(run.id, trace, error)),
            };
            if let Err(error) = event.validate() {
                return Err(self.failed(run.id, trace, error));
            }

            if received_finish {
                return Err(self.failed(
                    run.id,
                    trace,
                    ProviderError::MalformedStream(
                        "provider emitted an event after finish".to_owned(),
                    ),
                ));
            }

            let mut record_model_chunk = false;
            match event {
                ProviderEvent::TextDelta { text } => {
                    if !text.is_empty() {
                        if output_chunks.len() >= MAX_OUTPUT_CHUNKS {
                            return Err(self.failed(
                                run.id,
                                trace,
                                ProviderError::Failed(format!(
                                    "provider emitted more than {} output chunks",
                                    MAX_OUTPUT_CHUNKS
                                )),
                            ));
                        }
                        let retained = retained_output_bytes.saturating_add(text.len());
                        if retained > MAX_AGENT_OUTPUT_BYTES {
                            return Err(self.failed(
                                run.id,
                                trace,
                                ProviderError::Failed(format!(
                                    "provider output exceeds {} bytes",
                                    MAX_AGENT_OUTPUT_BYTES
                                )),
                            ));
                        }
                        retained_output_bytes = retained;
                        output_chunks.push(text);
                        record_model_chunk = true;
                    }
                }
                ProviderEvent::ReasoningDelta { .. } => record_model_chunk = true,
                ProviderEvent::ToolCallStarted { call_id, .. } => {
                    if active_tool_calls.len() >= MAX_TOOL_CALLS {
                        return Err(self.failed(
                            run.id,
                            trace,
                            ProviderError::MalformedStream(format!(
                                "provider emitted more than {} active tool calls",
                                MAX_TOOL_CALLS
                            )),
                        ));
                    }
                    if !active_tool_calls.insert(call_id) {
                        return Err(self.failed(
                            run.id,
                            trace,
                            ProviderError::MalformedStream("duplicate tool-call start".to_owned()),
                        ));
                    }
                    record_model_chunk = true;
                }
                ProviderEvent::ToolCallArgumentsDelta { call_id, .. } => {
                    if !active_tool_calls.contains(&call_id) {
                        return Err(self.failed(
                            run.id,
                            trace,
                            ProviderError::MalformedStream(
                                "tool-call arguments arrived before start".to_owned(),
                            ),
                        ));
                    }
                    record_model_chunk = true;
                }
                ProviderEvent::ToolCallCompleted { call } => {
                    if call.arguments.len() > MAX_PROVIDER_PAYLOAD_BYTES {
                        return Err(self.failed(
                            run.id,
                            trace,
                            ProviderError::MalformedStream(
                                "tool-call arguments exceed the provider bound".to_owned(),
                            ),
                        ));
                    }
                    if !active_tool_calls.remove(&call.call_id) {
                        return Err(self.failed(
                            run.id,
                            trace,
                            ProviderError::MalformedStream(
                                "tool-call completed without start".to_owned(),
                            ),
                        ));
                    }
                    if tool_calls.len() >= MAX_TOOL_CALLS {
                        return Err(self.failed(
                            run.id,
                            trace,
                            ProviderError::MalformedStream(format!(
                                "provider completed more than {} tool calls",
                                MAX_TOOL_CALLS
                            )),
                        ));
                    }
                    tool_calls.push(call);
                    record_model_chunk = true;
                }
                ProviderEvent::Usage(ProviderUsage {
                    usage: observed, ..
                }) => usage = observed,
                ProviderEvent::Finish(reason) => {
                    if !active_tool_calls.is_empty() && !matches!(reason, FinishReason::Cancelled) {
                        return Err(self.failed(
                            run.id,
                            trace,
                            ProviderError::MalformedStream(
                                "provider finished with incomplete tool call".to_owned(),
                            ),
                        ));
                    }
                    if matches!(reason, FinishReason::Cancelled) {
                        trace.record(Event::new(
                            run.id,
                            EventKind::ModelCancelled {
                                agent_id: self.identity.id,
                            },
                        ));
                        trace.record(Event::new(
                            run.id,
                            EventKind::RunCancelled { run_id: run.id },
                        ));
                        return Err(AgentError::Cancelled { trace });
                    }
                    received_finish = true;
                }
            }
            if record_model_chunk {
                trace.record(Event::new(
                    run.id,
                    EventKind::ModelChunkReceived {
                        agent_id: self.identity.id,
                        chunk_index: next_chunk_index,
                    },
                ));
                next_chunk_index = next_chunk_index.saturating_add(1);
            }
        }

        if cancellation.is_cancelled() {
            trace.record(Event::new(
                run.id,
                EventKind::ModelCancelled {
                    agent_id: self.identity.id,
                },
            ));
            trace.record(Event::new(
                run.id,
                EventKind::RunCancelled { run_id: run.id },
            ));
            return Err(AgentError::Cancelled { trace });
        }

        if !received_finish {
            return Err(self.failed(
                run.id,
                trace,
                ProviderError::Failed("stream ended before a final chunk".to_string()),
            ));
        }

        trace.record(Event::new(
            run.id,
            EventKind::ModelCompleted {
                agent_id: self.identity.id,
                usage,
            },
        ));
        trace.record(Event::new(
            run.id,
            EventKind::RunCompleted { run_id: run.id },
        ));

        Ok(AgentExecution {
            run,
            task,
            agent: self.identity.clone(),
            output_chunks,
            tool_calls,
            usage,
            trace,
        })
    }

    fn failed(
        &self,
        run_id: orynth_kernel::RunId,
        mut trace: EventTrace,
        error: ProviderError,
    ) -> AgentError {
        trace.record(Event::new(
            run_id,
            EventKind::ModelFailed {
                agent_id: self.identity.id,
                message: error.to_string(),
            },
        ));
        trace.record(Event::new(
            run_id,
            EventKind::RunFailed {
                run_id,
                message: error.to_string(),
            },
        ));
        AgentError::Provider { error, trace }
    }
}

fn validate_fork_prefix(
    prefix: &EventTrace,
    run_id: orynth_kernel::RunId,
    task: &Task,
    agent_id: orynth_kernel::AgentId,
) -> Result<(), String> {
    if prefix.is_empty() {
        return Err("fork prefix must contain at least run.created".to_string());
    }
    if prefix.events().iter().any(|event| event.run_id != run_id) {
        return Err("fork prefix contains an event from another run".to_string());
    }
    if !matches!(
        prefix.events().first().map(|event| &event.kind),
        Some(EventKind::RunCreated { run_id: created }) if *created == run_id
    ) {
        return Err("fork prefix must begin with run.created for the child run".to_string());
    }
    if !prefix.events().iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::TaskCreated {
                task_id,
                run_id: task_run,
                ..
            } if *task_id == task.id && *task_run == run_id
        )
    }) {
        return Err("fork prefix does not contain the requested task".to_string());
    }
    if !prefix.events().iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::AgentCreated { agent } if agent.id == agent_id
        )
    }) {
        return Err("fork prefix does not contain the session agent".to_string());
    }
    if prefix.events().iter().any(|event| {
        matches!(
            event.kind,
            EventKind::RunCompleted { .. }
                | EventKind::RunCancelled { .. }
                | EventKind::RunFailed { .. }
        )
    }) {
        return Err("live fork prefix cannot contain a terminal run event".to_string());
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentExecution {
    pub run: Run,
    pub task: Task,
    pub agent: AgentIdentity,
    /// Streamed text deltas remain separately owned so callers do not require
    /// the provider boundary to construct one unbounded response string.
    pub output_chunks: Vec<String>,
    /// Completed provider tool calls are handed to the runtime/tool boundary;
    /// the provider never executes them or becomes an authority for effects.
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
    pub trace: EventTrace,
}

impl AgentExecution {
    pub fn output_text(&self) -> String {
        self.output_chunks.concat()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentError {
    InvalidFork {
        message: String,
        trace: EventTrace,
    },
    Cancelled {
        trace: EventTrace,
    },
    Provider {
        error: ProviderError,
        trace: EventTrace,
    },
}

impl AgentError {
    pub fn trace(&self) -> &EventTrace {
        match self {
            Self::InvalidFork { trace, .. }
            | Self::Cancelled { trace }
            | Self::Provider { trace, .. } => trace,
        }
    }
}

impl fmt::Display for AgentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFork { message, .. } => write!(formatter, "invalid agent fork: {message}"),
            Self::Cancelled { .. } => formatter.write_str("agent execution cancelled"),
            Self::Provider { error, .. } => write!(formatter, "agent provider error: {error}"),
        }
    }
}

impl std::error::Error for AgentError {}

#[cfg(test)]
mod tests {
    use super::*;
    use orynth_kernel::{AgentId, ModelClass, ModelRef};
    use orynth_provider::MockProvider;

    fn session() -> AgentSession<MockProvider> {
        let model = ModelRef::new("mock", "foundation", ModelClass::Cheap);
        let identity = AgentIdentity::new("foundation-agent", "test the foundation", model.clone());
        AgentSession::new(
            identity,
            MockProvider::new(model, "hello from the provider"),
        )
    }

    #[test]
    fn execution_keeps_logical_identity_and_records_transitions() {
        let session = session();
        let identity = session.identity().id;
        let execution = session
            .run("say hello", CancellationToken::new())
            .expect("mock execution should succeed");

        assert_eq!(execution.agent.id, identity);
        assert_eq!(execution.output_text(), "hello from the provider");
        assert_eq!(execution.usage.total_tokens(), 6);
        assert!(matches!(
            execution.trace.events().last().map(|event| &event.kind),
            Some(EventKind::RunCompleted { .. })
        ));
    }

    #[test]
    fn cancellation_records_a_transition_without_accepting_chunks() {
        let session = session();
        let token = CancellationToken::new();
        token.cancel();

        let error = session
            .run("cancel me", token)
            .expect_err("run should cancel");

        assert!(matches!(error, AgentError::Cancelled { .. }));
        assert!(
            !error
                .trace()
                .events()
                .iter()
                .any(|event| matches!(event.kind, EventKind::ModelChunkReceived { .. }))
        );
    }

    #[test]
    fn provider_failure_is_observable() {
        let model = ModelRef::new("mock", "foundation", ModelClass::Cheap);
        let identity = AgentIdentity::new("foundation-agent", "test errors", model.clone());
        let session = AgentSession::new(
            identity,
            MockProvider::new(model, "response").with_failure_after_chunks(0),
        );

        let error = session
            .run("fail", CancellationToken::new())
            .expect_err("mock failure should propagate");

        assert!(matches!(error, AgentError::Provider { .. }));
        assert!(
            error
                .trace()
                .events()
                .iter()
                .any(|event| matches!(event.kind, EventKind::ModelFailed { .. }))
        );
    }

    #[test]
    fn provider_output_retention_is_bounded() {
        let model = ModelRef::new("mock", "foundation", ModelClass::Cheap);
        let identity = AgentIdentity::new("foundation-agent", "bound output", model.clone());
        let oversized =
            MockProvider::new(model.clone(), "").with_script(vec![Ok(ProviderEvent::TextDelta {
                text: "x".repeat(MAX_AGENT_OUTPUT_BYTES + 1),
            })]);
        let error = AgentSession::new(identity, oversized)
            .run("bound me", CancellationToken::new())
            .expect_err("oversized provider output should fail closed");
        assert!(matches!(error, AgentError::Provider { .. }));
    }

    #[test]
    fn provider_output_chunk_cardinality_is_bounded() {
        let model = ModelRef::new("mock", "foundation", ModelClass::Cheap);
        let identity = AgentIdentity::new("foundation-agent", "bound chunks", model.clone());
        let script = vec![
            Ok(ProviderEvent::TextDelta {
                text: "x".to_owned(),
            });
            MAX_OUTPUT_CHUNKS + 1
        ];
        let provider = MockProvider::new(model, "").with_script(script);
        let error = AgentSession::new(identity, provider)
            .run("bound chunks", CancellationToken::new())
            .expect_err("too many provider chunks should fail closed");
        assert!(matches!(error, AgentError::Provider { .. }));
    }

    #[test]
    fn fork_execution_reuses_child_identity_and_streams_live_provider_output() {
        let session = session();
        let run = Run::new();
        let task = Task::new(run.id, "continue fork");
        let mut prefix = EventTrace::default();
        prefix.record(Event::new(run.id, EventKind::RunCreated { run_id: run.id }));
        prefix.record(Event::new(
            run.id,
            EventKind::TaskCreated {
                task_id: task.id,
                run_id: run.id,
                title: task.title.clone(),
            },
        ));
        prefix.record(Event::new(
            run.id,
            EventKind::AgentCreated {
                agent: session.identity().clone(),
            },
        ));

        let execution = session
            .run_fork(
                run.id,
                task,
                "continue from the fork",
                prefix,
                CancellationToken::new(),
            )
            .expect("fork provider execution should succeed");

        assert_eq!(execution.run.id, run.id);
        assert_eq!(execution.agent.id, session.identity().id);
        assert_eq!(execution.output_text(), "hello from the provider");
        assert!(matches!(
            execution.trace.events().last().map(|event| &event.kind),
            Some(EventKind::RunCompleted { run_id }) if *run_id == run.id
        ));
    }

    #[test]
    fn fork_execution_rejects_terminal_or_mismatched_prefixes_without_calling_provider() {
        let session = session();
        let run = Run::new();
        let task = Task::new(run.id, "invalid fork");
        let mut prefix = EventTrace::default();
        prefix.record(Event::new(run.id, EventKind::RunCreated { run_id: run.id }));
        prefix.record(Event::new(
            run.id,
            EventKind::RunCompleted { run_id: run.id },
        ));

        let error = session
            .run_fork(
                run.id,
                task,
                "must not run",
                prefix,
                CancellationToken::new(),
            )
            .expect_err("terminal prefix should be rejected");
        assert!(matches!(error, AgentError::InvalidFork { .. }));
    }

    #[allow(dead_code)]
    fn _agent_id_is_a_distinct_type(_: AgentId) {}
}
