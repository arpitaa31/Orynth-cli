use std::fmt;

use orynth_kernel::{
    AgentIdentity, CancellationToken, Event, EventKind, EventTrace, Run, Task, Usage,
};
use orynth_provider::{ModelProvider, ModelRequest, ProviderError};

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

        let mut output = String::new();
        let mut usage = Usage::default();
        let mut received_final_chunk = false;

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

            let chunk = match item {
                Ok(chunk) => chunk,
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

            let chunk_index = trace
                .events()
                .iter()
                .filter(|event| matches!(event.kind, EventKind::ModelChunkReceived { .. }))
                .count() as u32;
            output.push_str(&chunk.text);
            if let Some(chunk_usage) = chunk.usage {
                usage = chunk_usage;
            }
            received_final_chunk |= chunk.done;
            trace.record(Event::new(
                run.id,
                EventKind::ModelChunkReceived {
                    agent_id: self.identity.id,
                    chunk_index,
                },
            ));
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

        if !received_final_chunk {
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
            output,
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
    pub output: String,
    pub usage: Usage,
    pub trace: EventTrace,
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
        assert_eq!(execution.output, "hello from the provider");
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
        assert_eq!(execution.output, "hello from the provider");
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
