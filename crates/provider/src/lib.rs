use std::{collections::BTreeMap, fmt, sync::Arc};

use orynth_kernel::{AgentId, CancellationToken, ModelRef, RunId, TaskId, Usage};

pub const MAX_REQUEST_PARTS: usize = 128;
pub const MAX_PROVIDER_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_TOOL_DEFINITIONS: usize = 64;
pub const MAX_TOOL_CALLS: usize = 128;
pub const MAX_OUTPUT_CHUNKS: usize = 65_536;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderCapabilities {
    pub streaming: bool,
    pub tools: bool,
    pub parallel_tools: bool,
    pub structured_output: bool,
    pub vision: bool,
    pub reasoning: bool,
    pub prompt_cache: bool,
    pub usage_metadata: bool,
    pub cost_metadata: bool,
    pub cancellation: bool,
    pub context_limit: Option<u32>,
    pub max_output_tokens: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RequestPart {
    System(String),
    User(String),
    Model(String),
    ToolResult {
        call_id: String,
        content: String,
        is_error: bool,
    },
    Image {
        media_type: String,
        data: Vec<u8>,
    },
}

impl RequestPart {
    fn size_bytes(&self) -> usize {
        match self {
            Self::System(value) | Self::User(value) | Self::Model(value) => value.len(),
            Self::ToolResult {
                call_id, content, ..
            } => call_id.len().saturating_add(content.len()),
            Self::Image { media_type, data } => media_type.len().saturating_add(data.len()),
        }
    }

    fn requires_vision(&self) -> bool {
        matches!(self, Self::Image { .. })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// Provider-neutral structured schema, normally JSON Schema.
    pub input_schema: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StructuredOutputRequest {
    pub name: String,
    pub schema: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReasoningOptions {
    pub effort: Option<String>,
    pub budget_tokens: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRequest {
    pub run_id: RunId,
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub model: ModelRef,
    pub parts: Vec<RequestPart>,
    pub tools: Vec<ToolDefinition>,
    pub structured_output: Option<StructuredOutputRequest>,
    pub reasoning: Option<ReasoningOptions>,
    pub parallel_tool_calls: bool,
    pub max_output_tokens: Option<u32>,
    pub extensions: BTreeMap<String, String>,
}

impl ModelRequest {
    pub fn new(
        run_id: RunId,
        task_id: TaskId,
        agent_id: AgentId,
        model: ModelRef,
        prompt: impl Into<String>,
    ) -> Self {
        Self {
            run_id,
            task_id,
            agent_id,
            model,
            parts: vec![RequestPart::User(prompt.into())],
            tools: Vec::new(),
            structured_output: None,
            reasoning: None,
            parallel_tool_calls: false,
            max_output_tokens: None,
            extensions: BTreeMap::new(),
        }
    }

    pub fn with_system(mut self, content: impl Into<String>) -> Self {
        self.parts.insert(0, RequestPart::System(content.into()));
        self
    }

    pub fn with_tool(mut self, tool: ToolDefinition) -> Self {
        self.tools.push(tool);
        self
    }

    pub fn with_structured_output(mut self, output: StructuredOutputRequest) -> Self {
        self.structured_output = Some(output);
        self
    }

    pub fn with_reasoning(mut self, reasoning: ReasoningOptions) -> Self {
        self.reasoning = Some(reasoning);
        self
    }

    pub fn with_parallel_tool_calls(mut self, enabled: bool) -> Self {
        self.parallel_tool_calls = enabled;
        self
    }

    pub fn text_input(&self) -> String {
        self.parts
            .iter()
            .filter_map(|part| match part {
                RequestPart::System(text) | RequestPart::User(text) | RequestPart::Model(text) => {
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn validate(&self) -> Result<(), ProviderError> {
        if self.parts.len() > MAX_REQUEST_PARTS {
            return Err(ProviderError::InvalidRequest(format!(
                "request has too many parts: {}",
                self.parts.len()
            )));
        }
        if self.tools.len() > MAX_TOOL_DEFINITIONS {
            return Err(ProviderError::InvalidRequest(format!(
                "request has too many tools: {}",
                self.tools.len()
            )));
        }
        if self.parallel_tool_calls && self.tools.is_empty() {
            return Err(ProviderError::InvalidRequest(
                "parallel tool calls require at least one tool".to_owned(),
            ));
        }
        let part_bytes = self
            .parts
            .iter()
            .map(RequestPart::size_bytes)
            .try_fold(0usize, |total, size| total.checked_add(size))
            .ok_or_else(|| ProviderError::InvalidRequest("request size overflow".to_owned()))?;
        let tool_bytes = self.tools.iter().try_fold(0usize, |total, tool| {
            total
                .checked_add(tool.name.len())
                .and_then(|value| value.checked_add(tool.description.len()))
                .and_then(|value| value.checked_add(tool.input_schema.len()))
        });
        let metadata_bytes = self
            .structured_output
            .as_ref()
            .map(|output| output.name.len().saturating_add(output.schema.len()))
            .unwrap_or_default()
            .saturating_add(
                self.reasoning
                    .as_ref()
                    .and_then(|reasoning| reasoning.effort.as_ref())
                    .map(String::len)
                    .unwrap_or_default(),
            )
            .saturating_add(self.model.provider.len())
            .saturating_add(self.model.model.len());
        let extension_bytes = self
            .extensions
            .iter()
            .try_fold(0usize, |total, (key, value)| {
                total
                    .checked_add(key.len())
                    .and_then(|total| total.checked_add(value.len()))
            });
        let tool_bytes = tool_bytes
            .ok_or_else(|| ProviderError::InvalidRequest("request size overflow".to_owned()))?;
        let extension_bytes = extension_bytes
            .ok_or_else(|| ProviderError::InvalidRequest("request size overflow".to_owned()))?;
        let total = part_bytes
            .checked_add(tool_bytes)
            .and_then(|total| total.checked_add(metadata_bytes))
            .and_then(|total| total.checked_add(extension_bytes))
            .ok_or_else(|| ProviderError::InvalidRequest("request size overflow".to_owned()))?;
        if total > MAX_PROVIDER_PAYLOAD_BYTES {
            return Err(ProviderError::InvalidRequest(format!(
                "request payload exceeds {} bytes",
                MAX_PROVIDER_PAYLOAD_BYTES
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolCall {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCall,
    ContentFilter,
    Cancelled,
    Other(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CostMetadata {
    pub currency: String,
    pub input_microunits: u64,
    pub output_microunits: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderUsage {
    pub usage: Usage,
    pub cost: Option<CostMetadata>,
    pub prompt_cache_hit: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderEvent {
    TextDelta { text: String },
    ReasoningDelta { text: String },
    ToolCallStarted { call_id: String, name: String },
    ToolCallArgumentsDelta { call_id: String, delta: String },
    ToolCallCompleted { call: ToolCall },
    Usage(ProviderUsage),
    Finish(FinishReason),
}

impl ProviderEvent {
    pub fn validate(&self) -> Result<(), ProviderError> {
        let size = match self {
            Self::TextDelta { text } | Self::ReasoningDelta { text } => text.len(),
            Self::ToolCallStarted { call_id, name } => call_id.len().saturating_add(name.len()),
            Self::ToolCallArgumentsDelta { call_id, delta } => {
                call_id.len().saturating_add(delta.len())
            }
            Self::ToolCallCompleted { call } => call
                .call_id
                .len()
                .saturating_add(call.name.len())
                .saturating_add(call.arguments.len()),
            Self::Usage(ProviderUsage { cost, .. }) => cost
                .as_ref()
                .map(|cost| cost.currency.len())
                .unwrap_or_default(),
            Self::Finish(FinishReason::Other(reason)) => reason.len(),
            Self::Finish(_) => 0,
        };
        if size > MAX_PROVIDER_PAYLOAD_BYTES {
            return Err(ProviderError::MalformedStream(format!(
                "provider event exceeds {} bytes",
                MAX_PROVIDER_PAYLOAD_BYTES
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderError {
    Cancelled,
    InvalidRequest(String),
    CapabilityMismatch(String),
    ModelUnavailable(String),
    RateLimited { retry_after_ms: Option<u64> },
    Timeout,
    MalformedStream(String),
    Failed(String),
}

impl fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("provider request cancelled"),
            Self::InvalidRequest(message) => {
                write!(formatter, "invalid provider request: {message}")
            }
            Self::CapabilityMismatch(message) => {
                write!(formatter, "provider capability mismatch: {message}")
            }
            Self::ModelUnavailable(model) => write!(formatter, "model unavailable: {model}"),
            Self::RateLimited { retry_after_ms } => {
                write!(
                    formatter,
                    "provider rate limited (retry after {retry_after_ms:?} ms)"
                )
            }
            Self::Timeout => formatter.write_str("provider request timed out"),
            Self::MalformedStream(message) => {
                write!(formatter, "malformed provider stream: {message}")
            }
            Self::Failed(message) => write!(formatter, "provider failed: {message}"),
        }
    }
}

impl std::error::Error for ProviderError {}

pub trait ModelProvider: Send + Sync {
    fn model(&self) -> &ModelRef;

    fn capabilities(&self) -> ProviderCapabilities;

    fn validate_request(&self, request: &ModelRequest) -> Result<(), ProviderError> {
        request.validate()?;
        if request.model != *self.model() {
            return Err(ProviderError::ModelUnavailable(request.model.model.clone()));
        }
        let capabilities = self.capabilities();
        if !request.tools.is_empty() && !capabilities.tools {
            return Err(ProviderError::CapabilityMismatch(
                "tools are not supported".to_owned(),
            ));
        }
        if request.parallel_tool_calls && !capabilities.parallel_tools {
            return Err(ProviderError::CapabilityMismatch(
                "parallel tool calls are not supported".to_owned(),
            ));
        }
        if request.structured_output.is_some() && !capabilities.structured_output {
            return Err(ProviderError::CapabilityMismatch(
                "structured output is not supported".to_owned(),
            ));
        }
        if request.reasoning.is_some() && !capabilities.reasoning {
            return Err(ProviderError::CapabilityMismatch(
                "reasoning is not supported".to_owned(),
            ));
        }
        if request.parts.iter().any(RequestPart::requires_vision) && !capabilities.vision {
            return Err(ProviderError::CapabilityMismatch(
                "vision is not supported".to_owned(),
            ));
        }
        if request.max_output_tokens.is_some_and(|limit| {
            capabilities
                .max_output_tokens
                .is_some_and(|maximum| limit > maximum)
        }) {
            return Err(ProviderError::CapabilityMismatch(
                "requested output exceeds provider limit".to_owned(),
            ));
        }
        Ok(())
    }

    /// A pull-based typed stream supplies natural backpressure: the provider
    /// cannot enqueue another event until the caller requests the next one.
    fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<Box<dyn Iterator<Item = Result<ProviderEvent, ProviderError>> + Send>, ProviderError>;
}

#[derive(Clone, Debug)]
pub struct MockProvider {
    model: ModelRef,
    response: Arc<str>,
    chunk_size: usize,
    failure_after_chunks: Option<usize>,
    scripted: Option<Vec<Result<ProviderEvent, ProviderError>>>,
}

impl MockProvider {
    pub fn new(model: ModelRef, response: impl Into<String>) -> Self {
        Self {
            model,
            response: Arc::from(response.into()),
            chunk_size: 8,
            failure_after_chunks: None,
            scripted: None,
        }
    }

    pub fn with_chunk_size(mut self, chunk_size: usize) -> Self {
        self.chunk_size = chunk_size.max(1);
        self
    }

    pub fn with_failure_after_chunks(mut self, chunks: usize) -> Self {
        self.failure_after_chunks = Some(chunks);
        self
    }

    pub fn with_script(mut self, script: Vec<Result<ProviderEvent, ProviderError>>) -> Self {
        self.scripted = Some(script);
        self
    }

    pub fn with_tool_call(
        self,
        call_id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        let call_id = call_id.into();
        let name = name.into();
        let arguments = arguments.into();
        self.with_script(vec![
            Ok(ProviderEvent::ToolCallStarted {
                call_id: call_id.clone(),
                name: name.clone(),
            }),
            Ok(ProviderEvent::ToolCallArgumentsDelta {
                call_id: call_id.clone(),
                delta: arguments.clone(),
            }),
            Ok(ProviderEvent::ToolCallCompleted {
                call: ToolCall {
                    call_id,
                    name,
                    arguments,
                },
            }),
            Ok(ProviderEvent::Finish(FinishReason::ToolCall)),
        ])
    }

    pub fn with_malformed_tool_call(self) -> Self {
        self.with_script(vec![Err(ProviderError::MalformedStream(
            "tool-call arguments arrived before start".to_owned(),
        ))])
    }
}

impl ModelProvider for MockProvider {
    fn model(&self) -> &ModelRef {
        &self.model
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            tools: true,
            parallel_tools: true,
            structured_output: true,
            vision: false,
            reasoning: true,
            prompt_cache: false,
            usage_metadata: true,
            cost_metadata: false,
            cancellation: true,
            context_limit: Some(128_000),
            max_output_tokens: Some(16_384),
        }
    }

    fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<Box<dyn Iterator<Item = Result<ProviderEvent, ProviderError>> + Send>, ProviderError>
    {
        self.validate_request(&request)?;
        if self.response.len() > MAX_PROVIDER_PAYLOAD_BYTES {
            return Err(ProviderError::InvalidRequest(format!(
                "mock response exceeds {} bytes",
                MAX_PROVIDER_PAYLOAD_BYTES
            )));
        }
        if request.text_input().trim().is_empty() {
            return Err(ProviderError::InvalidRequest(
                "request must contain text input".to_owned(),
            ));
        }
        if let Some(scripted) = &self.scripted {
            return Ok(Box::new(ScriptedStream {
                events: scripted.clone().into_iter(),
                cancellation,
            }));
        }
        Ok(Box::new(MockStream {
            response: Arc::clone(&self.response),
            offset: 0,
            chunk_size: self.chunk_size,
            usage: Usage::new(
                request.text_input().split_whitespace().count() as u64,
                self.response.split_whitespace().count() as u64,
            ),
            cancellation,
            failure_after_chunks: self.failure_after_chunks,
            emitted_chunks: 0,
            sent_usage: false,
            sent_finish: false,
        }))
    }
}

struct MockStream {
    response: Arc<str>,
    offset: usize,
    chunk_size: usize,
    usage: Usage,
    cancellation: CancellationToken,
    failure_after_chunks: Option<usize>,
    emitted_chunks: usize,
    sent_usage: bool,
    sent_finish: bool,
}

impl Iterator for MockStream {
    type Item = Result<ProviderEvent, ProviderError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.cancellation.is_cancelled() {
            return Some(Err(ProviderError::Cancelled));
        }
        if self
            .failure_after_chunks
            .is_some_and(|limit| self.emitted_chunks >= limit)
        {
            return Some(Err(ProviderError::Failed(
                "configured mock failure".to_owned(),
            )));
        }
        if self.offset < self.response.len() {
            let mut end = self.offset;
            let mut chars = self.response[self.offset..].char_indices();
            for _ in 0..self.chunk_size {
                let Some((relative, character)) = chars.next() else {
                    break;
                };
                end = self.offset + relative + character.len_utf8();
            }
            if end <= self.offset {
                end = self.response.len();
            }
            let text = self.response[self.offset..end].to_owned();
            self.offset = end;
            self.emitted_chunks += 1;
            return Some(Ok(ProviderEvent::TextDelta { text }));
        }
        if !self.sent_usage {
            self.sent_usage = true;
            return Some(Ok(ProviderEvent::Usage(ProviderUsage {
                usage: self.usage,
                cost: None,
                prompt_cache_hit: None,
            })));
        }
        if !self.sent_finish {
            self.sent_finish = true;
            return Some(Ok(ProviderEvent::Finish(FinishReason::Stop)));
        }
        None
    }
}

struct ScriptedStream<I> {
    events: I,
    cancellation: CancellationToken,
}

impl<I> Iterator for ScriptedStream<I>
where
    I: Iterator<Item = Result<ProviderEvent, ProviderError>> + Send,
{
    type Item = Result<ProviderEvent, ProviderError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.cancellation.is_cancelled() {
            return Some(Err(ProviderError::Cancelled));
        }
        self.events.next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orynth_kernel::{AgentIdentity, ModelClass, Run, Task};

    fn request(model: ModelRef) -> ModelRequest {
        let run = Run::new();
        let task = Task::new(run.id, "test");
        let agent = AgentIdentity::new("test", "test", model.clone());
        ModelRequest::new(run.id, task.id, agent.id, model, "hello world")
    }

    #[test]
    fn mock_streams_typed_text_usage_and_finish_events() {
        let model = ModelRef::new("mock", "demo", ModelClass::Cheap);
        let provider = MockProvider::new(model.clone(), "abcdef").with_chunk_size(2);
        let events: Vec<_> = provider
            .stream(request(model), CancellationToken::new())
            .expect("stream should start")
            .collect::<Result<Vec<_>, _>>()
            .expect("stream should succeed");
        let text = events
            .iter()
            .filter_map(|event| match event {
                ProviderEvent::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert_eq!(text, "abcdef");
        assert!(events.iter().any(|event| matches!(
            event,
            ProviderEvent::Usage(ProviderUsage { usage, .. }) if *usage == Usage::new(2, 1)
        )));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ProviderEvent::Finish(FinishReason::Stop)))
        );
    }

    #[test]
    fn capability_negotiation_rejects_unsupported_request_parts() {
        let model = ModelRef::new("mock", "demo", ModelClass::Cheap);
        let request = request(model.clone())
            .with_system("instructions")
            .with_tool(ToolDefinition {
                name: "lookup".to_owned(),
                description: "lookup data".to_owned(),
                input_schema: "{}".to_owned(),
            })
            .with_parallel_tool_calls(true)
            .with_structured_output(StructuredOutputRequest {
                name: "answer".to_owned(),
                schema: "{}".to_owned(),
            });
        let provider = MockProvider::new(model, "ok");
        assert!(provider.validate_request(&request).is_ok());

        let vision = ModelRequest {
            parts: vec![RequestPart::Image {
                media_type: "image/png".to_owned(),
                data: vec![1, 2, 3],
            }],
            ..request
        };
        assert!(matches!(
            provider.validate_request(&vision),
            Err(ProviderError::CapabilityMismatch(_))
        ));
    }

    #[test]
    fn mock_tool_call_and_malformed_stream_are_typed() {
        let model = ModelRef::new("mock", "demo", ModelClass::Cheap);
        let tool_request = request(model.clone()).with_tool(ToolDefinition {
            name: "lookup".to_owned(),
            description: "lookup data".to_owned(),
            input_schema: "{}".to_owned(),
        });
        let events: Vec<_> = MockProvider::new(model.clone(), "")
            .with_tool_call("call-1", "lookup", "{\"q\":1}")
            .stream(tool_request, CancellationToken::new())
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(events.iter().any(|event| matches!(
            event,
            ProviderEvent::ToolCallCompleted { call } if call.call_id == "call-1"
        )));
        assert!(matches!(
            MockProvider::new(model, "")
                .with_malformed_tool_call()
                .stream(
                    request(ModelRef::new("mock", "demo", ModelClass::Cheap)),
                    CancellationToken::new()
                )
                .unwrap()
                .next(),
            Some(Err(ProviderError::MalformedStream(_)))
        ));
    }

    #[test]
    fn cancellation_and_provider_failures_are_observable() {
        let model = ModelRef::new("mock", "demo", ModelClass::Cheap);
        let token = CancellationToken::new();
        let mut stream = MockProvider::new(model.clone(), "abcdef")
            .with_chunk_size(2)
            .stream(request(model.clone()), token.clone())
            .unwrap();
        assert!(matches!(
            stream.next(),
            Some(Ok(ProviderEvent::TextDelta { .. }))
        ));
        token.cancel();
        assert_eq!(stream.next(), Some(Err(ProviderError::Cancelled)));

        let failure = MockProvider::new(model, "abcdef").with_failure_after_chunks(0);
        assert!(matches!(
            failure
                .stream(
                    request(ModelRef::new("mock", "demo", ModelClass::Cheap)),
                    CancellationToken::new()
                )
                .unwrap()
                .next(),
            Some(Err(ProviderError::Failed(_)))
        ));
    }

    #[test]
    fn empty_response_still_finishes_with_usage() {
        let model = ModelRef::new("mock", "demo", ModelClass::Cheap);
        let events: Vec<_> = MockProvider::new(model.clone(), "")
            .stream(request(model), CancellationToken::new())
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ProviderEvent::Usage(_)))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ProviderEvent::Finish(FinishReason::Stop)))
        );
    }
}
