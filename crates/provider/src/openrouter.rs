//! OpenRouter Chat Completions transport behind Orynth's provider contract.
use std::{
    collections::{BTreeMap, VecDeque},
    io::{BufRead, BufReader, Read},
    sync::mpsc::{Receiver, RecvTimeoutError, sync_channel},
    thread,
    time::{Duration, Instant},
};

use orynth_kernel::{CancellationToken, ModelRef, Usage};
use reqwest::{
    StatusCode,
    blocking::{Client, Response},
};
use serde_json::{Value, json};

use crate::{
    FinishReason, MAX_PROVIDER_PAYLOAD_BYTES, MAX_TOOL_CALLS, ModelProvider, ModelRequest,
    ProviderCapabilities, ProviderError, ProviderEvent, ProviderUsage, RequestPart, ToolCall,
    ToolChoice,
};

pub const BASE_URL: &str = "https://openrouter.ai/api/v1";
const MAX_LINE: usize = MAX_PROVIDER_PAYLOAD_BYTES;
const REQUEST_DEADLINE: Duration = Duration::from_secs(90);

pub struct OpenRouterProvider {
    model: ModelRef,
    client: Client,
    endpoint: String,
    api_key: String,
    title: Option<String>,
}

impl OpenRouterProvider {
    pub fn from_env(
        model: ModelRef,
        base_url: &str,
        title: Option<String>,
    ) -> Result<Self, ProviderError> {
        let key = std::env::var("OPENROUTER_API_KEY").map_err(|_| {
            ProviderError::Failed(
                "OpenRouter is configured but OPENROUTER_API_KEY is not set".into(),
            )
        })?;
        Self::new(model, key, base_url, title)
    }

    pub fn new(
        model: ModelRef,
        api_key: String,
        base_url: &str,
        title: Option<String>,
    ) -> Result<Self, ProviderError> {
        if model.provider != "openrouter" || model.model.trim().is_empty() {
            return Err(ProviderError::InvalidRequest(
                "expected an OpenRouter model assignment".into(),
            ));
        }
        if api_key.trim().is_empty() {
            return Err(ProviderError::Failed("OPENROUTER_API_KEY is empty".into()));
        }
        if !(base_url.starts_with("https://")
            || base_url.starts_with("http://127.0.0.1:")
            || base_url.starts_with("http://localhost:"))
        {
            return Err(ProviderError::InvalidRequest(
                "OpenRouter base URL must use HTTPS".into(),
            ));
        }
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok();
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(90))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| {
                ProviderError::Failed("could not initialize OpenRouter HTTP client".into())
            })?;
        Ok(Self {
            model,
            client,
            endpoint: format!("{}/chat/completions", base_url.trim_end_matches('/')),
            api_key,
            title,
        })
    }
}

impl ModelProvider for OpenRouterProvider {
    fn model(&self) -> &ModelRef {
        &self.model
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            tools: true,
            parallel_tools: false,
            structured_output: false,
            vision: false,
            reasoning: false,
            prompt_cache: false,
            usage_metadata: true,
            cost_metadata: false,
            cancellation: true,
            context_limit: None,
            max_output_tokens: None,
        }
    }
    fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<Box<dyn Iterator<Item = Result<ProviderEvent, ProviderError>> + Send>, ProviderError>
    {
        self.validate_request(&request)?;
        if cancellation.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let body = encode_request(&request)?;
        let client = self.client.clone();
        let endpoint = self.endpoint.clone();
        let api_key = self.api_key.clone();
        let title = self.title.clone();
        let worker_cancellation = cancellation.clone();
        let deadline = Instant::now() + REQUEST_DEADLINE;
        let (sender, receiver) = sync_channel(8);
        thread::Builder::new()
            .name("orynth-openrouter-stream".into())
            .spawn(move || {
                let mut call = client
                    .post(&endpoint)
                    .bearer_auth(&api_key)
                    .header("Accept", "text/event-stream")
                    .header("X-OpenRouter-Metadata", "enabled")
                    .json(&body);
                if let Some(title) = &title {
                    call = call.header("X-Title", title);
                }
                if worker_cancellation.is_cancelled() {
                    let _ = sender.send(Err(ProviderError::Cancelled));
                    return;
                }
                let response = match call.send() {
                    Ok(response) => response,
                    Err(error) => {
                        let _ = sender.send(Err(map_transport(error)));
                        return;
                    }
                };
                if !response.status().is_success() {
                    let error = map_status(
                        response.status(),
                        response
                            .headers()
                            .get("retry-after")
                            .and_then(|value| value.to_str().ok()),
                    );
                    let _ = sender.send(Err(error));
                    return;
                }
                for event in OpenRouterStream::new(response, worker_cancellation) {
                    let terminal = event.is_err();
                    if sender.send(event).is_err() || terminal {
                        break;
                    }
                }
            })
            .map_err(|_| ProviderError::Failed("could not start OpenRouter transport".into()))?;
        Ok(Box::new(BoundedStream {
            receiver: Some(receiver),
            cancellation,
            deadline,
            finished: false,
            saw_finish: false,
        }))
    }
}

struct BoundedStream {
    receiver: Option<Receiver<Result<ProviderEvent, ProviderError>>>,
    cancellation: CancellationToken,
    deadline: Instant,
    finished: bool,
    saw_finish: bool,
}

impl Iterator for BoundedStream {
    type Item = Result<ProviderEvent, ProviderError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        loop {
            if self.cancellation.is_cancelled() {
                self.finished = true;
                self.receiver = None;
                return Some(Err(ProviderError::Cancelled));
            }
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.finished = true;
                self.receiver = None;
                return Some(Err(ProviderError::Timeout));
            }
            match self
                .receiver
                .as_ref()
                .expect("active stream has a receiver")
                .recv_timeout(remaining.min(Duration::from_millis(100)))
            {
                Ok(Ok(event)) => {
                    if matches!(event, ProviderEvent::Finish(_)) {
                        self.saw_finish = true;
                    }
                    return Some(Ok(event));
                }
                Ok(Err(error)) => {
                    self.finished = true;
                    self.receiver = None;
                    return Some(Err(error));
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    self.finished = true;
                    self.receiver = None;
                    return if self.saw_finish {
                        None
                    } else {
                        Some(Err(ProviderError::MalformedStream(
                            "OpenRouter stream ended without completion".into(),
                        )))
                    };
                }
            }
        }
    }
}

fn encode_request(request: &ModelRequest) -> Result<Value, ProviderError> {
    let mut messages = Vec::new();
    for part in &request.parts {
        messages.push(match part {
            RequestPart::System(content) => json!({"role":"system","content":content}),
            RequestPart::User(content) => json!({"role":"user","content":content}),
            RequestPart::Model(content) => json!({"role":"assistant","content":content}),
            RequestPart::ModelToolCalls(calls) => json!({"role":"assistant","content":null,
                "tool_calls":calls.iter().map(|call| json!({"id":call.call_id,"type":"function",
                    "function":{"name":call.name,"arguments":call.arguments}})).collect::<Vec<_>>() }),
            RequestPart::ToolResult { call_id, content, .. } => json!({"role":"tool","tool_call_id":call_id,"content":content}),
            RequestPart::Image { .. } => return Err(ProviderError::CapabilityMismatch("image input is not implemented for OpenRouter".into())),
        });
    }
    let tools = request.tools.iter().map(|tool| {
        let schema: Value = serde_json::from_str(&tool.input_schema)
            .map_err(|_| ProviderError::InvalidRequest(format!("invalid tool schema for {}", tool.name)))?;
        Ok(json!({"type":"function","function":{"name":tool.name,"description":tool.description,"parameters":schema}}))
    }).collect::<Result<Vec<_>, ProviderError>>()?;
    let mut body = json!({"model":request.model.model,"messages":messages,"stream":true,
        "stream_options":{"include_usage":true}});
    if !request.tools.is_empty() {
        body["tools"] = json!(tools);
        body["tool_choice"] = json!(match request.tool_choice {
            ToolChoice::Auto => "auto",
            ToolChoice::Required => "required",
        });
        body["provider"] = json!({"require_parameters":true});
    }
    if let Some(max) = request.max_output_tokens {
        body["max_tokens"] = json!(max);
    }
    Ok(body)
}

fn map_transport(error: reqwest::Error) -> ProviderError {
    if error.is_timeout() {
        ProviderError::Timeout
    } else {
        ProviderError::Failed("OpenRouter transport error".into())
    }
}
fn map_status(status: StatusCode, retry_after: Option<&str>) -> ProviderError {
    match status.as_u16() {
        429 => ProviderError::RateLimited {
            retry_after_ms: retry_after
                .and_then(|v| v.parse::<u64>().ok())
                .map(|s| s.saturating_mul(1000)),
        },
        408 | 504 => ProviderError::Timeout,
        400 | 422 => ProviderError::InvalidRequest(format!(
            "OpenRouter rejected request (HTTP {status}); check model and tool support"
        )),
        404 => {
            ProviderError::ModelUnavailable(format!("OpenRouter model unavailable (HTTP {status})"))
        }
        401 | 403 => ProviderError::Failed(format!(
            "OpenRouter authentication or access failed (HTTP {status})"
        )),
        _ => ProviderError::Failed(format!("OpenRouter request failed (HTTP {status})")),
    }
}

#[derive(Default)]
struct PendingCall {
    id: String,
    name: String,
    arguments: String,
    started: bool,
}

fn finish_reason(value: &str) -> FinishReason {
    match value {
        "stop" => FinishReason::Stop,
        "length" => FinishReason::Length,
        "tool_calls" => FinishReason::ToolCall,
        "content_filter" => FinishReason::ContentFilter,
        other => FinishReason::Other(other.into()),
    }
}

fn merge_finish_reason(
    current: &mut Option<FinishReason>,
    incoming: FinishReason,
) -> Result<(), ProviderError> {
    match current {
        None => *current = Some(incoming),
        Some(existing) if *existing == incoming => {}
        Some(_) => {
            return Err(ProviderError::MalformedStream(
                "conflicting finish reasons".into(),
            ));
        }
    }
    Ok(())
}

struct OpenRouterStream {
    reader: BufReader<Response>,
    cancellation: CancellationToken,
    queued: VecDeque<Result<ProviderEvent, ProviderError>>,
    calls: BTreeMap<u64, PendingCall>,
    seen_done: bool,
    pending_finish: Option<FinishReason>,
    seen_model: Option<String>,
    seen_request_id: Option<String>,
    seen_provider: Option<String>,
    failed: bool,
    bytes: usize,
}
impl OpenRouterStream {
    fn new(response: Response, cancellation: CancellationToken) -> Self {
        Self {
            reader: BufReader::new(response),
            cancellation,
            queued: VecDeque::new(),
            calls: BTreeMap::new(),
            seen_done: false,
            pending_finish: None,
            seen_model: None,
            seen_request_id: None,
            seen_provider: None,
            failed: false,
            bytes: 0,
        }
    }
    fn next_line(&mut self) -> Result<Option<String>, ProviderError> {
        let mut bytes = Vec::new();
        let read = (&mut self.reader)
            .take((MAX_LINE + 1) as u64)
            .read_until(b'\n', &mut bytes)
            .map_err(|_| ProviderError::Failed("OpenRouter stream read error".into()))?;
        if read == 0 {
            return Ok(None);
        }
        self.bytes = self.bytes.saturating_add(read);
        if read > MAX_LINE || self.bytes > MAX_PROVIDER_PAYLOAD_BYTES {
            return Err(ProviderError::MalformedStream(
                "OpenRouter response exceeds limit".into(),
            ));
        }
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| ProviderError::MalformedStream("OpenRouter stream is not UTF-8".into()))
    }
    fn accept(&mut self, data: &str) -> Result<(), ProviderError> {
        let value: Value = serde_json::from_str(data)
            .map_err(|_| ProviderError::MalformedStream("invalid OpenRouter SSE JSON".into()))?;
        let resolved_model = value["model"].as_str().map(str::to_owned);
        let request_id = value["id"].as_str().map(str::to_owned);
        let provider_name = value["openrouter_metadata"]["endpoints"]["available"]
            .as_array()
            .and_then(|endpoints| endpoints.iter().find(|entry| entry["selected"] == true))
            .and_then(|entry| entry["provider"].as_str())
            .map(str::to_owned);
        let new_model = resolved_model.filter(|model| self.seen_model.as_ref() != Some(model));
        let new_request_id = request_id.filter(|id| self.seen_request_id.as_ref() != Some(id));
        let new_provider = provider_name.filter(|name| self.seen_provider.as_ref() != Some(name));
        if new_model.is_some() || new_request_id.is_some() || new_provider.is_some() {
            if let Some(model) = &new_model {
                self.seen_model = Some(model.clone());
            }
            if let Some(id) = &new_request_id {
                self.seen_request_id = Some(id.clone());
            }
            if let Some(name) = &new_provider {
                self.seen_provider = Some(name.clone());
            }
            self.queued.push_back(Ok(ProviderEvent::ResponseMetadata {
                resolved_model: new_model,
                request_id: new_request_id,
                provider_name: new_provider,
            }));
        }
        if !value["error"].is_null() {
            return Err(ProviderError::Failed(
                "OpenRouter stream reported an error".into(),
            ));
        }
        if let Some(usage) = value.get("usage").filter(|v| !v.is_null()) {
            let input = usage["prompt_tokens"].as_u64();
            let output = usage["completion_tokens"].as_u64();
            if let (Some(input), Some(output)) = (input, output) {
                let mut usage_value = Usage::new(input, output);
                usage_value.cached_input_tokens =
                    usage["prompt_tokens_details"]["cached_tokens"].as_u64();
                if usage_value
                    .cached_input_tokens
                    .is_some_and(|cached| cached > input)
                {
                    return Err(ProviderError::MalformedStream(
                        "cached input tokens exceed prompt tokens".into(),
                    ));
                }
                self.queued
                    .push_back(Ok(ProviderEvent::Usage(ProviderUsage {
                        usage: usage_value,
                        cost: None,
                        prompt_cache_hit: None,
                    })));
            }
        }
        let Some(choices) = value.get("choices").and_then(Value::as_array) else {
            return Ok(());
        };
        if choices.len() > 1 {
            return Err(ProviderError::MalformedStream(
                "multiple choices are not supported".into(),
            ));
        }
        let Some(choice) = choices.first() else {
            return Ok(());
        };
        if let Some(text) = choice["delta"]["content"]
            .as_str()
            .filter(|s| !s.is_empty())
        {
            self.queued
                .push_back(Ok(ProviderEvent::TextDelta { text: text.into() }));
        }
        if let Some(chunks) = choice["delta"]["tool_calls"].as_array() {
            for chunk in chunks {
                let index = chunk["index"].as_u64().ok_or_else(|| {
                    ProviderError::MalformedStream("tool call missing index".into())
                })?;
                if index >= MAX_TOOL_CALLS as u64 {
                    return Err(ProviderError::MalformedStream("too many tool calls".into()));
                }
                let call = self.calls.entry(index).or_default();
                if let Some(id) = chunk["id"].as_str() {
                    call.id.push_str(id);
                }
                if let Some(name) = chunk["function"]["name"].as_str() {
                    call.name.push_str(name);
                }
                if let Some(delta) = chunk["function"]["arguments"].as_str() {
                    call.arguments.push_str(delta);
                    if call.arguments.len() > MAX_PROVIDER_PAYLOAD_BYTES {
                        return Err(ProviderError::MalformedStream(
                            "tool arguments exceed limit".into(),
                        ));
                    }
                    if call.started {
                        self.queued
                            .push_back(Ok(ProviderEvent::ToolCallArgumentsDelta {
                                call_id: call.id.clone(),
                                delta: delta.into(),
                            }));
                    }
                }
                if !call.started && !call.id.is_empty() && !call.name.is_empty() {
                    call.started = true;
                    self.queued.push_back(Ok(ProviderEvent::ToolCallStarted {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                    }));
                    if !call.arguments.is_empty() {
                        self.queued
                            .push_back(Ok(ProviderEvent::ToolCallArgumentsDelta {
                                call_id: call.id.clone(),
                                delta: call.arguments.clone(),
                            }));
                    }
                }
            }
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            merge_finish_reason(&mut self.pending_finish, finish_reason(reason))?;
            for (_, call) in std::mem::take(&mut self.calls) {
                if !call.started || call.id.is_empty() || call.name.is_empty() {
                    return Err(ProviderError::MalformedStream(
                        "incomplete tool call".into(),
                    ));
                }
                self.queued.push_back(Ok(ProviderEvent::ToolCallCompleted {
                    call: ToolCall {
                        call_id: call.id,
                        name: call.name,
                        arguments: call.arguments,
                    },
                }));
            }
        }
        Ok(())
    }
}
impl Iterator for OpenRouterStream {
    type Item = Result<ProviderEvent, ProviderError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.seen_done && self.queued.is_empty() {
            return None;
        }
        if self.cancellation.is_cancelled() {
            self.failed = true;
            return Some(Err(ProviderError::Cancelled));
        }
        if let Some(item) = self.queued.pop_front() {
            return Some(item);
        }
        loop {
            match self.next_line() {
                Ok(Some(line)) if line.starts_with("data:") => {
                    let data = line[5..].trim();
                    if data == "[DONE]" {
                        self.seen_done = true;
                        let Some(reason) = self.pending_finish.take() else {
                            self.failed = true;
                            return Some(Err(ProviderError::MalformedStream(
                                "OpenRouter ended without finish reason".into(),
                            )));
                        };
                        self.queued.push_back(Ok(ProviderEvent::Finish(reason)));
                        return self.queued.pop_front();
                    }
                    if let Err(error) = self.accept(data) {
                        self.failed = true;
                        return Some(Err(error));
                    }
                    if let Some(item) = self.queued.pop_front() {
                        return Some(item);
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => {
                    self.failed = true;
                    return Some(Err(ProviderError::MalformedStream(
                        "OpenRouter stream ended before [DONE]".into(),
                    )));
                }
                Err(error) => {
                    self.failed = true;
                    return Some(Err(error));
                }
            }
            if self.cancellation.is_cancelled() {
                self.failed = true;
                return Some(Err(ProviderError::Cancelled));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orynth_kernel::{AgentId, ModelClass, RunId, TaskId};
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    fn model() -> ModelRef {
        ModelRef::new("openrouter", "openrouter/free", ModelClass::Cheap)
    }
    fn request() -> ModelRequest {
        ModelRequest::new(
            RunId::from_u64(1),
            TaskId::from_u64(2),
            AgentId::from_u64(3),
            model(),
            "hello",
        )
    }
    fn serve(
        status: &str,
        body: impl Into<String>,
    ) -> (String, thread::JoinHandle<(String, Value)>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let status = status.to_owned();
        let body = body.into();
        let handle = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut data = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                let count = socket.read(&mut chunk).unwrap();
                assert!(count > 0);
                data.extend_from_slice(&chunk[..count]);
                if let Some(end) = data.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&data[..end]);
                    let length = header
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|n| n.parse::<usize>().ok())
                        })
                        .unwrap();
                    if data.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let end = data
                .windows(4)
                .position(|bytes| bytes == b"\r\n\r\n")
                .unwrap();
            let headers = String::from_utf8_lossy(&data[..end]).into_owned();
            let payload = serde_json::from_slice(&data[end + 4..]).unwrap();
            let _ = write!(
                socket,
                "HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            (headers, payload)
        });
        (base, handle)
    }

    fn stream_fixture(body: &str) -> Result<Vec<ProviderEvent>, ProviderError> {
        let (base, server) = serve("200 OK", body);
        let provider = OpenRouterProvider::new(model(), "test-secret".into(), &base, None).unwrap();
        let result = provider
            .stream(request(), CancellationToken::new())
            .unwrap()
            .collect::<Result<Vec<_>, _>>();
        server.join().unwrap();
        result
    }

    #[test]
    fn streams_text_metadata_usage_and_finish_from_mock_http() {
        let (base, server) = serve(
            "200 OK",
            concat!(
                "data: {\"id\":\"gen-1\",\"model\":\"vendor/free:free\",\"choices\":[{\"delta\":{\"content\":\"ORYNTH_\"},\"finish_reason\":null}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"CONNECTED\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: {\"choices\":[],\"openrouter_metadata\":{\"endpoints\":{\"available\":[{\"provider\":\"Example Provider\",\"selected\":true}]}},\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2,\"prompt_tokens_details\":{\"cached_tokens\":5}}}\n\n",
                "data: [DONE]\n\n"
            ),
        );
        let provider =
            OpenRouterProvider::new(model(), "test-secret".into(), &base, Some("Orynth".into()))
                .unwrap();
        let events = provider
            .stream(request(), CancellationToken::new())
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            matches!(&events[0], ProviderEvent::ResponseMetadata { resolved_model: Some(name), .. } if name == "vendor/free:free")
        );
        assert!(
            matches!(&events[3], ProviderEvent::ResponseMetadata { provider_name: Some(name), .. } if name == "Example Provider")
        );
        assert!(
            matches!(&events[4], ProviderEvent::Usage(u) if u.usage.cached_input_tokens == Some(5))
        );
        assert_eq!(events[5], ProviderEvent::Finish(FinishReason::Stop));
        let (headers, payload) = server.join().unwrap();
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("authorization: bearer test-secret")
        );
        assert!(headers.contains("X-Title: Orynth") || headers.contains("x-title: Orynth"));
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("x-openrouter-metadata: enabled")
        );
        assert_eq!(payload["model"], "openrouter/free");
        assert_eq!(payload["stream_options"]["include_usage"], true);
    }

    #[test]
    fn identical_finish_metadata_is_idempotent_before_done() {
        let events = stream_fixture(concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        ))
        .unwrap();
        assert!(events.contains(&ProviderEvent::TextDelta {
            text: "hello".into(),
        }));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ProviderEvent::Finish(_)))
                .count(),
            1
        );
        assert_eq!(
            events.last(),
            Some(&ProviderEvent::Finish(FinishReason::Stop))
        );
    }

    #[test]
    fn reasoning_and_visible_content_deltas_keep_only_visible_text() {
        let events = stream_fixture(concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning\":\"sanitized internal reasoning\",\"content\":\"ORYNTH_\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"reasoning_details\":[{\"type\":\"reasoning.text\",\"text\":\"sanitized metadata\"}],\"content\":\"CONNECTED\"},\"finish_reason\":\"length\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":34,\"completion_tokens\":32}}\n\n",
            "data: [DONE]\n\n"
        ))
        .unwrap();
        let visible_text = events
            .iter()
            .filter_map(|event| match event {
                ProviderEvent::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert_eq!(visible_text, "ORYNTH_CONNECTED");
        assert!(events.iter().any(|event| matches!(
            event,
            ProviderEvent::Usage(usage) if usage.usage == Usage::new(34, 32)
        )));
        assert!(events.contains(&ProviderEvent::Finish(FinishReason::Length)));

        let reasoning_only = stream_fixture(concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning\":\"sanitized reasoning-only chunk\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":34,\"completion_tokens\":32}}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
            "data: [DONE]\n\n"
        ))
        .unwrap();
        assert!(
            !reasoning_only
                .iter()
                .any(|event| matches!(event, ProviderEvent::TextDelta { .. }))
        );
        assert!(reasoning_only.iter().any(
            |event| matches!(event, ProviderEvent::Usage(usage) if usage.usage.output_tokens == 32)
        ));
        assert!(reasoning_only.contains(&ProviderEvent::Finish(FinishReason::Length)));
    }

    #[test]
    fn usage_only_chunks_before_or_after_finish_preserve_usage() {
        let events = stream_fixture(concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3}}\n\n",
            "data: [DONE]\n\n"
        ))
        .unwrap();
        let usage_index = events
            .iter()
            .position(|event| matches!(event, ProviderEvent::Usage(_)))
            .unwrap();
        let finish_index = events
            .iter()
            .position(|event| matches!(event, ProviderEvent::Finish(FinishReason::Stop)))
            .unwrap();
        assert!(usage_index < finish_index);
        assert!(matches!(
            events[usage_index],
            ProviderEvent::Usage(ProviderUsage {
                usage: Usage {
                    input_tokens: 7,
                    output_tokens: 3,
                    ..
                },
                ..
            })
        ));

        let events = stream_fixture(concat!(
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":0}}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        ))
        .unwrap();
        assert!(matches!(
            events.first(),
            Some(ProviderEvent::Usage(ProviderUsage {
                usage: Usage {
                    input_tokens: 4,
                    output_tokens: 0,
                    ..
                },
                ..
            }))
        ));
        assert_eq!(
            events.last(),
            Some(&ProviderEvent::Finish(FinishReason::Stop))
        );
    }

    #[test]
    fn conflicting_finish_metadata_remains_malformed() {
        let result = stream_fixture(concat!(
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n"
        ));
        assert!(matches!(
            result,
            Err(ProviderError::MalformedStream(message)) if message.contains("conflicting finish reasons")
        ));
    }

    #[test]
    fn multiple_choice_streams_are_rejected_explicitly() {
        let result = stream_fixture(concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"one\"},\"finish_reason\":null},{\"delta\":{\"content\":\"two\"},\"finish_reason\":null}]}\n\n",
            "data: [DONE]\n\n"
        ));
        assert!(matches!(
            result,
            Err(ProviderError::MalformedStream(message)) if message.contains("multiple choices")
        ));
    }

    #[test]
    fn duplicate_done_is_transport_eof_and_emits_one_finish() {
        let events = stream_fixture(concat!(
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
            "data: [DONE]\n\n"
        ))
        .unwrap();
        assert_eq!(events, vec![ProviderEvent::Finish(FinishReason::Stop)]);
    }

    #[test]
    fn decodes_two_interleaved_tool_calls_and_keeps_execution_local() {
        let (base, server) = serve(
            "200 OK",
            concat!(
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"one\",\"function\":{\"name\":\"filesystem_write_text\",\"arguments\":\"{\\\"path\\\":\"}},{\"index\":1,\"id\":\"two\",\"function\":{\"name\":\"filesystem_write_text\",\"arguments\":\"{\\\"path\\\":\"}}]},\"finish_reason\":null}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"index.html\\\"}\"}},{\"index\":1,\"function\":{\"arguments\":\"\\\"styles.css\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
                "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
                "data: [DONE]\n\n"
            ),
        );
        let provider = OpenRouterProvider::new(model(), "test-secret".into(), &base, None).unwrap();
        let mut request = request();
        request.tools.push(crate::ToolDefinition {
            name: "filesystem_write_text".into(),
            description: "write".into(),
            input_schema: r#"{"type":"object"}"#.into(),
        });
        let events = provider
            .stream(request, CancellationToken::new())
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let calls = events
            .iter()
            .filter_map(|e| {
                if let ProviderEvent::ToolCallCompleted { call } = e {
                    Some(call)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].arguments, "{\"path\":\"index.html\"}");
        assert_eq!(calls[1].arguments, "{\"path\":\"styles.css\"}");
        assert_eq!(
            events.last(),
            Some(&ProviderEvent::Finish(FinishReason::ToolCall))
        );
        let (_, payload) = server.join().unwrap();
        assert_eq!(payload["tools"][0]["type"], "function");
    }

    #[test]
    fn errors_are_redacted_and_truncated_stream_fails() {
        let (base, server) = serve("401 Unauthorized", "secret provider body");
        let provider = OpenRouterProvider::new(model(), "test-secret".into(), &base, None).unwrap();
        let error = provider
            .stream(request(), CancellationToken::new())
            .unwrap()
            .next()
            .unwrap()
            .unwrap_err();
        assert!(!error.to_string().contains("secret"));
        server.join().unwrap();
        let (base, server) = serve(
            "200 OK",
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\n",
        );
        let provider = OpenRouterProvider::new(model(), "test-secret".into(), &base, None).unwrap();
        let result = provider
            .stream(request(), CancellationToken::new())
            .unwrap()
            .collect::<Result<Vec<_>, _>>();
        assert!(matches!(result, Err(ProviderError::MalformedStream(_))));
        server.join().unwrap();
    }

    #[test]
    fn cancellation_before_dispatch_does_not_connect() {
        let provider =
            OpenRouterProvider::new(model(), "test-secret".into(), "http://127.0.0.1:1", None)
                .unwrap();
        let token = CancellationToken::new();
        token.cancel();
        assert!(matches!(
            provider.stream(request(), token),
            Err(ProviderError::Cancelled)
        ));
    }

    #[test]
    fn unavailable_network_and_timeout_are_typed_errors() {
        let provider =
            OpenRouterProvider::new(model(), "test-secret".into(), "http://127.0.0.1:1", None)
                .unwrap();
        let mut stream = provider
            .stream(request(), CancellationToken::new())
            .unwrap();
        assert!(matches!(stream.next(), Some(Err(ProviderError::Failed(_)))));

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (_socket, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_millis(150));
        });
        let mut provider =
            OpenRouterProvider::new(model(), "test-secret".into(), &base, None).unwrap();
        provider.client = Client::builder()
            .timeout(Duration::from_millis(40))
            .build()
            .unwrap();
        let mut stream = provider
            .stream(request(), CancellationToken::new())
            .unwrap();
        assert!(matches!(stream.next(), Some(Err(ProviderError::Timeout))));
        server.join().unwrap();
    }

    #[test]
    fn cancellation_stops_stream_before_next_delta() {
        let (base, server) = serve(
            "200 OK",
            concat!(
                "data: {\"id\":\"gen-1\",\"choices\":[{\"delta\":{\"content\":\"first\"},\"finish_reason\":null}]}\n\n",
                "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n"
            ),
        );
        let provider = OpenRouterProvider::new(model(), "test-secret".into(), &base, None).unwrap();
        let token = CancellationToken::new();
        let mut stream = provider.stream(request(), token.clone()).unwrap();
        assert!(matches!(
            stream.next(),
            Some(Ok(ProviderEvent::ResponseMetadata { .. }))
        ));
        token.cancel();
        assert!(matches!(stream.next(), Some(Err(ProviderError::Cancelled))));
        server.join().unwrap();
    }

    #[test]
    fn cancellation_returns_while_network_body_is_stalled() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = [0u8; 4096];
            assert!(socket.read(&mut request).unwrap() > 0);
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap();
            socket.flush().unwrap();
            thread::sleep(Duration::from_millis(400));
        });
        let provider = OpenRouterProvider::new(model(), "test-secret".into(), &base, None).unwrap();
        let token = CancellationToken::new();
        let mut stream = provider.stream(request(), token.clone()).unwrap();
        let cancelling = thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            token.cancel();
        });
        let started = Instant::now();
        assert!(matches!(stream.next(), Some(Err(ProviderError::Cancelled))));
        assert!(started.elapsed() < Duration::from_millis(300));
        cancelling.join().unwrap();
        server.join().unwrap();
    }

    #[test]
    fn request_deadline_ends_a_stalled_stream() {
        let (_sender, receiver) = sync_channel(1);
        let mut stream = BoundedStream {
            receiver: Some(receiver),
            cancellation: CancellationToken::new(),
            deadline: Instant::now() + Duration::from_millis(40),
            finished: false,
            saw_finish: false,
        };
        let started = Instant::now();
        assert!(matches!(stream.next(), Some(Err(ProviderError::Timeout))));
        assert!(started.elapsed() < Duration::from_millis(300));
        assert!(stream.next().is_none());
    }

    #[test]
    fn cancellation_closes_the_transport_channel() {
        let (sender, receiver) = sync_channel(1);
        let cancellation = CancellationToken::new();
        let mut stream = BoundedStream {
            receiver: Some(receiver),
            cancellation: cancellation.clone(),
            deadline: Instant::now() + Duration::from_secs(1),
            finished: false,
            saw_finish: false,
        };
        cancellation.cancel();
        assert!(matches!(stream.next(), Some(Err(ProviderError::Cancelled))));
        assert!(
            sender
                .send(Ok(ProviderEvent::TextDelta {
                    text: "late".into(),
                }))
                .is_err()
        );
    }

    #[test]
    fn tool_continuation_serializes_as_assistant_call_and_local_result() {
        let mut request = request();
        request.tools.push(crate::ToolDefinition {
            name: "delegate_personal_site".into(),
            description: "delegate".into(),
            input_schema: r#"{"type":"object"}"#.into(),
        });
        request.tool_choice = ToolChoice::Required;
        request
            .parts
            .push(RequestPart::ModelToolCalls(vec![ToolCall {
                call_id: "call-1".into(),
                name: "delegate_personal_site".into(),
                arguments: "{}".into(),
            }]));
        request.parts.push(RequestPart::ToolResult {
            call_id: "call-1".into(),
            content: "worker verified index.html".into(),
            is_error: false,
        });
        let body = encode_request(&request).unwrap();
        assert_eq!(body["tool_choice"], "required");
        assert_eq!(body["provider"]["require_parameters"], true);
        assert_eq!(body["messages"][1]["role"], "assistant");
        assert_eq!(body["messages"][2]["role"], "tool");
        assert_eq!(body["messages"][2]["tool_call_id"], "call-1");
        assert!(body.get("plugins").is_none());
    }

    #[test]
    fn classifies_rate_limit_and_server_error_without_response_body() {
        for (status, expected_rate_limit) in [
            ("429 Too Many Requests", true),
            ("503 Service Unavailable", false),
        ] {
            let (base, server) = serve(status, "secret provider error");
            let provider =
                OpenRouterProvider::new(model(), "test-secret".into(), &base, None).unwrap();
            let error = provider
                .stream(request(), CancellationToken::new())
                .unwrap()
                .next()
                .unwrap()
                .unwrap_err();
            assert_eq!(
                matches!(error, ProviderError::RateLimited { .. }),
                expected_rate_limit
            );
            assert!(!error.to_string().contains("secret"));
            server.join().unwrap();
        }
    }

    #[test]
    fn rejects_malformed_and_empty_streams() {
        for body in ["data: {bad-json}\n\n", "data: [DONE]\n\n", ""] {
            let (base, server) = serve("200 OK", body);
            let provider =
                OpenRouterProvider::new(model(), "test-secret".into(), &base, None).unwrap();
            let result = provider
                .stream(request(), CancellationToken::new())
                .unwrap()
                .collect::<Result<Vec<_>, _>>();
            assert!(matches!(result, Err(ProviderError::MalformedStream(_))));
            server.join().unwrap();
        }
    }

    #[test]
    fn bounds_total_stream_bytes_even_when_individual_lines_are_valid() {
        let comment = format!(":{}\n", "x".repeat(MAX_PROVIDER_PAYLOAD_BYTES / 2));
        let (base, server) = serve("200 OK", format!("{comment}{comment}data: [DONE]\n\n"));
        let provider = OpenRouterProvider::new(model(), "test-secret".into(), &base, None).unwrap();
        let result = provider
            .stream(request(), CancellationToken::new())
            .unwrap()
            .collect::<Result<Vec<_>, _>>();
        assert!(
            matches!(result, Err(ProviderError::MalformedStream(message)) if message.contains("exceeds limit"))
        );
        server.join().unwrap();
    }
}
