use std::fmt;

use orynth_kernel::{AgentId, CancellationToken, ModelRef, RunId, TaskId, Usage};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderCapabilities {
    pub streaming: bool,
    pub usage_metadata: bool,
    pub cache_usage_metadata: bool,
    pub cancellation: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRequest {
    pub run_id: RunId,
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub model: ModelRef,
    pub prompt: String,
    pub max_output_tokens: Option<u32>,
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
            prompt: prompt.into(),
            max_output_tokens: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelChunk {
    pub text: String,
    pub usage: Option<Usage>,
    pub done: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderError {
    Cancelled,
    InvalidRequest(String),
    ModelUnavailable(String),
    Failed(String),
}

impl fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("provider request cancelled"),
            Self::InvalidRequest(message) => {
                write!(formatter, "invalid provider request: {message}")
            }
            Self::ModelUnavailable(model) => write!(formatter, "model unavailable: {model}"),
            Self::Failed(message) => write!(formatter, "provider failed: {message}"),
        }
    }
}

impl std::error::Error for ProviderError {}

pub trait ModelProvider: Send + Sync {
    fn model(&self) -> &ModelRef;

    fn capabilities(&self) -> ProviderCapabilities;

    fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<Box<dyn Iterator<Item = Result<ModelChunk, ProviderError>> + Send>, ProviderError>;
}

#[derive(Clone, Debug)]
pub struct MockProvider {
    model: ModelRef,
    response: String,
    chunk_size: usize,
    failure_after_chunks: Option<usize>,
}

impl MockProvider {
    pub fn new(model: ModelRef, response: impl Into<String>) -> Self {
        Self {
            model,
            response: response.into(),
            chunk_size: 8,
            failure_after_chunks: None,
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
}

impl ModelProvider for MockProvider {
    fn model(&self) -> &ModelRef {
        &self.model
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            usage_metadata: true,
            cache_usage_metadata: false,
            cancellation: true,
        }
    }

    fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<Box<dyn Iterator<Item = Result<ModelChunk, ProviderError>> + Send>, ProviderError>
    {
        if request.model != self.model {
            return Err(ProviderError::ModelUnavailable(request.model.model));
        }

        if request.prompt.trim().is_empty() {
            return Err(ProviderError::InvalidRequest(
                "prompt must not be empty".to_string(),
            ));
        }

        let characters: Vec<char> = self.response.chars().collect();
        let chunks = if characters.is_empty() {
            vec![String::new()]
        } else {
            characters
                .chunks(self.chunk_size)
                .map(|chunk| chunk.iter().collect())
                .collect()
        };

        let usage = Usage::new(
            request.prompt.split_whitespace().count() as u64,
            self.response.split_whitespace().count() as u64,
        );

        Ok(Box::new(MockStream {
            chunks,
            index: 0,
            usage,
            cancellation,
            failure_after_chunks: self.failure_after_chunks,
        }))
    }
}

struct MockStream {
    chunks: Vec<String>,
    index: usize,
    usage: Usage,
    cancellation: CancellationToken,
    failure_after_chunks: Option<usize>,
}

impl Iterator for MockStream {
    type Item = Result<ModelChunk, ProviderError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.cancellation.is_cancelled() {
            return Some(Err(ProviderError::Cancelled));
        }

        if self
            .failure_after_chunks
            .is_some_and(|limit| self.index >= limit)
        {
            return Some(Err(ProviderError::Failed(
                "configured mock failure".to_string(),
            )));
        }

        let text = self.chunks.get(self.index)?.clone();
        self.index += 1;
        let done = self.index == self.chunks.len();

        Some(Ok(ModelChunk {
            text,
            usage: done.then_some(self.usage),
            done,
        }))
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
    fn mock_streams_ordered_chunks_and_final_usage() {
        let model = ModelRef::new("mock", "demo", ModelClass::Cheap);
        let provider = MockProvider::new(model.clone(), "abcdef").with_chunk_size(2);
        let chunks: Vec<_> = provider
            .stream(request(model), CancellationToken::new())
            .expect("stream should start")
            .collect::<Result<Vec<_>, _>>()
            .expect("stream should succeed");

        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.text.as_str())
                .collect::<String>(),
            "abcdef"
        );
        assert!(chunks.last().expect("final chunk").done);
        assert_eq!(
            chunks.last().and_then(|chunk| chunk.usage),
            Some(Usage::new(2, 1))
        );
    }

    #[test]
    fn cancellation_is_reported_before_next_chunk() {
        let model = ModelRef::new("mock", "demo", ModelClass::Cheap);
        let token = CancellationToken::new();
        let mut stream = MockProvider::new(model.clone(), "abcdef")
            .with_chunk_size(2)
            .stream(request(model), token.clone())
            .expect("stream should start");

        assert!(stream.next().expect("first chunk").is_ok());
        token.cancel();

        assert_eq!(stream.next(), Some(Err(ProviderError::Cancelled)));
    }
}
