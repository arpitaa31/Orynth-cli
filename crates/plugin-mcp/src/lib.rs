//! MCP interoperability adapter.
//!
//! MCP metadata and results are external inputs. This adapter translates them
//! into Orynth contracts but never turns server-provided annotations into
//! capabilities, risk permissions, or trusted content.

use std::{
    collections::BTreeMap,
    io::Read,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use orynth_kernel::{AgentId, TaskId, TrustOrigin};
use orynth_plugin_api::{
    PluginError, PluginKind, PluginManifest, PluginRequest, PluginResponse, PluginTransport,
    authorize_manifest_with_ownership,
};
use orynth_plugin_process::{ProcessCommand, ProcessSession, spawn_process_session};
use orynth_tool_runtime::{
    CapabilityRequirement, RiskLevel, ToolDefinition, ToolError, TrustPolicy,
};
use reqwest::{
    Url,
    blocking::{Client, Response},
};
use serde_json::{Map, Value, json};

pub const MCP_PROTOCOL_VERSION: u16 = 1;
pub const MCP_LEGACY_PROTOCOL_VERSION: &str = "2025-11-25";
pub const MCP_MODERN_PROTOCOL_VERSION: &str = "2026-07-28";
const MAX_DISCOVERY_PAGE_ITEMS: usize = 128;
const MAX_DISCOVERY_CURSOR_BYTES: usize = 512;
const MAX_RESOURCE_URI_BYTES: usize = 4096;
const MAX_DESCRIPTION_BYTES: usize = 64 * 1024;
const MAX_SSE_EVENT_BYTES: usize = 1024 * 1024;
const MAX_SSE_RECONNECTS: usize = 2;
const MAX_SSE_RETRY_DELAY_MS: u64 = 5_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpServerInfo {
    pub name: String,
    pub version: String,
    pub protocol_version: u16,
    /// The protocol version returned by the server during legacy
    /// negotiation. `None` is allowed only for caller-supplied metadata before
    /// a session has connected.
    pub wire_protocol_version: Option<String>,
    pub metadata: BTreeMap<String, String>,
}

impl McpServerInfo {
    pub fn validate(&self) -> Result<(), McpError> {
        if self.name.trim().is_empty() || self.version.trim().is_empty() {
            return Err(McpError::Invalid("server name and version are required"));
        }
        if self.protocol_version != MCP_PROTOCOL_VERSION {
            return Err(McpError::UnsupportedVersion(self.protocol_version));
        }
        if let Some(version) = &self.wire_protocol_version {
            validate_wire_protocol_version(version)?;
        }
        if self.metadata.len() > 64 {
            return Err(McpError::TooLarge(self.metadata.len()));
        }
        for (key, value) in &self.metadata {
            if key.trim().is_empty() || value.trim().is_empty() {
                return Err(McpError::Invalid("server metadata must not be blank"));
            }
        }
        Ok(())
    }

    pub fn origin(&self) -> TrustOrigin {
        TrustOrigin::McpMetadata
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpToolDescription {
    pub name: String,
    pub description: String,
    pub input_schema: Vec<u8>,
}

impl McpToolDescription {
    pub fn validate(&self) -> Result<(), McpError> {
        if self.name.trim().is_empty() || self.description.trim().is_empty() {
            return Err(McpError::Invalid("tool name and description are required"));
        }
        if self.name.len() > 256 || self.description.len() > MAX_DESCRIPTION_BYTES {
            return Err(McpError::TooLarge(
                self.name.len().max(self.description.len()),
            ));
        }
        if self.input_schema.len() > 1024 * 1024 {
            return Err(McpError::TooLarge(self.input_schema.len()));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpResourceDescription {
    pub uri: String,
    pub name: String,
    pub description: String,
}

impl McpResourceDescription {
    pub fn validate(&self) -> Result<(), McpError> {
        if self.uri.trim().is_empty()
            || self.name.trim().is_empty()
            || self.description.trim().is_empty()
        {
            return Err(McpError::Invalid(
                "resource URI, name, and description are required",
            ));
        }
        if self.uri.len() > MAX_RESOURCE_URI_BYTES
            || self.name.len() > 256
            || self.description.len() > MAX_DESCRIPTION_BYTES
        {
            return Err(McpError::TooLarge(
                self.uri
                    .len()
                    .max(self.name.len())
                    .max(self.description.len()),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpToolPage {
    pub tools: Vec<McpToolDescription>,
    pub next_cursor: Option<String>,
}

impl McpToolPage {
    fn validate(&self) -> Result<(), McpError> {
        validate_page_len(self.tools.len())?;
        validate_cursor(self.next_cursor.as_deref())?;
        for tool in &self.tools {
            tool.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpResourcePage {
    pub resources: Vec<McpResourceDescription>,
    pub next_cursor: Option<String>,
}

impl McpResourcePage {
    fn validate(&self) -> Result<(), McpError> {
        validate_page_len(self.resources.len())?;
        validate_cursor(self.next_cursor.as_deref())?;
        for resource in &self.resources {
            resource.validate()?;
        }
        Ok(())
    }
}

pub trait McpDiscoveryInvoker {
    fn list_tools(&mut self, cursor: Option<&str>, limit: u16) -> Result<McpToolPage, McpError>;

    fn list_resources(
        &mut self,
        cursor: Option<&str>,
        limit: u16,
    ) -> Result<McpResourcePage, McpError>;
}

/// Bounded, caller-driven MCP discovery. The host requests one page at a
/// time; server-provided cursors and descriptions remain untrusted metadata.
pub struct ProgressiveMcpDiscovery<I> {
    server: McpServerInfo,
    invoker: I,
    page_size: u16,
}

impl<I: McpDiscoveryInvoker> ProgressiveMcpDiscovery<I> {
    pub fn new(server: McpServerInfo, invoker: I, page_size: u16) -> Result<Self, McpError> {
        server.validate()?;
        if page_size == 0 || page_size as usize > MAX_DISCOVERY_PAGE_ITEMS {
            return Err(McpError::TooLarge(page_size as usize));
        }
        Ok(Self {
            server,
            invoker,
            page_size,
        })
    }

    pub fn server(&self) -> &McpServerInfo {
        &self.server
    }

    pub fn list_tools(&mut self, cursor: Option<&str>) -> Result<McpToolPage, McpError> {
        validate_cursor(cursor)?;
        let page = self.invoker.list_tools(cursor, self.page_size)?;
        page.validate()?;
        Ok(page)
    }

    pub fn list_resources(&mut self, cursor: Option<&str>) -> Result<McpResourcePage, McpError> {
        validate_cursor(cursor)?;
        let page = self.invoker.list_resources(cursor, self.page_size)?;
        page.validate()?;
        Ok(page)
    }
}

/// Host policy is deliberately separate from MCP metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpToolPolicy {
    pub required_fields: Vec<String>,
    pub capability: Option<CapabilityRequirement>,
    pub risk: RiskLevel,
    pub reversible: bool,
    pub trust_policy: TrustPolicy,
}

pub fn bind_tool(
    server: &McpServerInfo,
    description: &McpToolDescription,
    policy: &McpToolPolicy,
) -> Result<(ToolDefinition, TrustPolicy), McpError> {
    server.validate()?;
    description.validate()?;
    let definition = ToolDefinition {
        name: format!("mcp.{}.{}", server.name, description.name),
        version: server.version.clone(),
        required_fields: policy.required_fields.clone(),
        syntax_fields: Vec::new(),
        capability: policy.capability.clone(),
        ownership: None,
        risk: policy.risk,
        reversible: policy.reversible,
    };
    definition.validate().map_err(McpError::Tool)?;
    Ok((definition, policy.trust_policy))
}

pub trait McpInvoker {
    fn invoke(&mut self, request: &PluginRequest) -> Result<Vec<u8>, PluginError>;
}

/// MCP lifecycle variants are explicit because legacy servers use a stateful
/// initialize handshake while the modern revision carries negotiation per
/// request and has no protocol-level session handshake.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum McpProtocolMode {
    Legacy2025,
    Modern2026,
}

impl McpProtocolMode {
    pub const fn version(self) -> &'static str {
        match self {
            Self::Legacy2025 => MCP_LEGACY_PROTOCOL_VERSION,
            Self::Modern2026 => MCP_MODERN_PROTOCOL_VERSION,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpClientInfo {
    pub name: String,
    pub version: String,
}

impl McpClientInfo {
    fn validate(&self) -> Result<(), McpError> {
        if self.name.trim().is_empty() || self.version.trim().is_empty() {
            return Err(McpError::Invalid("client name and version are required"));
        }
        if self.name.len() > 256 || self.version.len() > 256 {
            return Err(McpError::TooLarge(self.name.len().max(self.version.len())));
        }
        Ok(())
    }
}

/// Transport-owned lifecycle hooks. The transport may be stdio, HTTP, or an
/// injected test double; this crate does not assume a wire framing mechanism.
pub trait McpSessionTransport {
    fn initialize(
        &mut self,
        mode: McpProtocolMode,
        client: &McpClientInfo,
    ) -> Result<McpServerInfo, McpError>;

    fn initialized(&mut self, mode: McpProtocolMode) -> Result<(), McpError>;

    fn request(
        &mut self,
        mode: McpProtocolMode,
        request: &PluginRequest,
    ) -> Result<Vec<u8>, McpError>;
}

pub struct McpSession<T> {
    mode: McpProtocolMode,
    client: McpClientInfo,
    server: McpServerInfo,
    transport: T,
    connected: bool,
    negotiated_protocol_version: Option<String>,
}

impl<T: McpSessionTransport> McpSession<T> {
    pub fn new(
        mode: McpProtocolMode,
        client: McpClientInfo,
        server: McpServerInfo,
        transport: T,
    ) -> Result<Self, McpError> {
        client.validate()?;
        server.validate()?;
        Ok(Self {
            mode,
            client,
            server,
            transport,
            connected: false,
            negotiated_protocol_version: None,
        })
    }

    pub fn connect(&mut self) -> Result<(), McpError> {
        if self.connected {
            return Err(McpError::Invalid("MCP session is already connected"));
        }
        if self.mode == McpProtocolMode::Legacy2025 {
            let server = self.transport.initialize(self.mode, &self.client)?;
            server.validate()?;
            let negotiated = server
                .wire_protocol_version
                .as_deref()
                .ok_or(McpError::Invalid(
                    "MCP initialize result omitted protocolVersion",
                ))?;
            validate_negotiated_protocol(self.mode, negotiated)?;
            self.negotiated_protocol_version = Some(negotiated.to_owned());
            self.server = server;
            self.transport.initialized(self.mode)?;
        } else {
            // Modern MCP carries negotiation on every request. The selected
            // mode is therefore the negotiated version for this session.
            self.negotiated_protocol_version = Some(self.mode.version().to_owned());
        }
        self.connected = true;
        Ok(())
    }

    pub fn mode(&self) -> McpProtocolMode {
        self.mode
    }

    pub fn client(&self) -> &McpClientInfo {
        &self.client
    }

    pub fn server(&self) -> &McpServerInfo {
        &self.server
    }

    pub fn is_connected(&self) -> bool {
        self.connected
    }

    pub fn negotiated_protocol_version(&self) -> Option<&str> {
        self.negotiated_protocol_version.as_deref()
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    fn request(&mut self, request: &PluginRequest) -> Result<Vec<u8>, PluginError> {
        if !self.connected {
            return Err(PluginError::Protocol(
                "MCP session is not connected".to_owned(),
            ));
        }
        request
            .validate_with(orynth_plugin_api::PluginResourceLimits::default())
            .map_err(|error| PluginError::Protocol(error.to_string()))?;
        self.transport
            .request(self.mode, request)
            .map_err(|error| PluginError::Protocol(error.to_string()))
    }
}

pub struct SessionMcpInvoker<T> {
    session: McpSession<T>,
}

impl<T: McpSessionTransport> SessionMcpInvoker<T> {
    pub fn new(session: McpSession<T>) -> Self {
        Self { session }
    }

    pub fn session(&self) -> &McpSession<T> {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut McpSession<T> {
        &mut self.session
    }
}

impl<T: McpSessionTransport> McpInvoker for SessionMcpInvoker<T> {
    fn invoke(&mut self, request: &PluginRequest) -> Result<Vec<u8>, PluginError> {
        self.session.request(request)
    }
}

/// A bounded newline-delimited JSON-RPC MCP stdio transport.
///
/// The process is spawned only by `connect`, after the caller supplies an
/// agent/task policy. Server-initiated messages are not silently accepted;
/// callers needing bidirectional requests must provide a transport with that
/// capability through `McpSessionTransport`.
pub struct StdioMcpTransport {
    command: ProcessCommand,
    manifest: PluginManifest,
    timeout: Duration,
    client: Option<McpClientInfo>,
    process: Option<ProcessSession>,
}

pub struct McpConnectionContext<'a> {
    pub policy: &'a orynth_security::CapabilityPolicy,
    pub ownership: &'a dyn orynth_security::ResourceOwnershipPolicy,
    pub agent_id: AgentId,
    pub task_id: Option<TaskId>,
    pub now_ms: u128,
}

impl StdioMcpTransport {
    pub fn new(
        command: ProcessCommand,
        manifest: PluginManifest,
        timeout: Duration,
    ) -> Result<Self, McpError> {
        manifest.validate().map_err(McpError::Plugin)?;
        if manifest.kind != PluginKind::Mcp {
            return Err(McpError::Invalid("MCP stdio requires an MCP manifest"));
        }
        if timeout.is_zero() {
            return Err(McpError::Invalid("MCP stdio timeout must be non-zero"));
        }
        Ok(Self {
            command,
            manifest,
            timeout,
            client: None,
            process: None,
        })
    }

    pub fn connect(
        mut self,
        mode: McpProtocolMode,
        client: McpClientInfo,
        server: McpServerInfo,
        context: McpConnectionContext<'_>,
    ) -> Result<McpSession<Self>, McpError> {
        self.client = Some(client.clone());
        self.process = Some(
            spawn_process_session(
                &self.command,
                &self.manifest,
                context.policy,
                context.ownership,
                context.agent_id,
                context.task_id,
                context.now_ms,
            )
            .map_err(McpError::Plugin)?,
        );
        let mut session = McpSession::new(mode, client, server, self)?;
        session.connect()?;
        Ok(session)
    }

    fn process_mut(&mut self) -> Result<&mut ProcessSession, McpError> {
        self.process
            .as_mut()
            .ok_or(McpError::Invalid("MCP stdio process is not connected"))
    }

    fn send_request_value(
        &mut self,
        mode: McpProtocolMode,
        request_id: u64,
        method: &str,
        params: Value,
    ) -> Result<Value, McpError> {
        let started = std::time::Instant::now();
        let message = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params,
        });
        let line = serde_json::to_vec(&message)
            .map_err(|_| McpError::Invalid("MCP request could not be encoded"))?;
        let timeout = self.timeout;
        let process = self.process_mut()?;
        let timeout = timeout.saturating_sub(started.elapsed());
        let response = process
            .request_line(&line, timeout)
            .map_err(McpError::Plugin)?;
        let response: Value = serde_json::from_slice(&response)
            .map_err(|_| McpError::Invalid("MCP response is not valid JSON"))?;
        if response.get("jsonrpc") != Some(&Value::String("2.0".to_owned())) {
            return Err(McpError::Invalid(
                "MCP response has an invalid JSON-RPC version",
            ));
        }
        if response.get("id") != Some(&Value::from(request_id)) {
            return Err(McpError::Invalid("MCP response ID does not match request"));
        }
        if response.get("error").is_some() {
            return Err(McpError::Invalid("MCP server returned a JSON-RPC error"));
        }
        let result = response
            .get("result")
            .cloned()
            .ok_or(McpError::Invalid("MCP response has no result"))?;
        if mode == McpProtocolMode::Modern2026 && self.client.is_none() {
            return Err(McpError::Invalid(
                "MCP modern request has no client identity",
            ));
        }
        Ok(result)
    }

    fn send_notification(&mut self, method: &str, params: Value) -> Result<(), McpError> {
        let message = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let line = serde_json::to_vec(&message)
            .map_err(|_| McpError::Invalid("MCP notification could not be encoded"))?;
        self.process_mut()?
            .write_line(&line)
            .map_err(McpError::Plugin)
    }
}

impl McpSessionTransport for StdioMcpTransport {
    fn initialize(
        &mut self,
        mode: McpProtocolMode,
        client: &McpClientInfo,
    ) -> Result<McpServerInfo, McpError> {
        self.client = Some(client.clone());
        let result = self.send_request_value(
            mode,
            1,
            "initialize",
            json!({
                "protocolVersion": mode.version(),
                "capabilities": {},
                "clientInfo": {
                    "name": client.name,
                    "version": client.version,
                },
            }),
        )?;
        let server = result
            .get("serverInfo")
            .and_then(Value::as_object)
            .ok_or(McpError::Invalid("MCP initialize result has no serverInfo"))?;
        let name = server
            .get("name")
            .and_then(Value::as_str)
            .ok_or(McpError::Invalid("MCP server name is missing"))?;
        let version = server
            .get("version")
            .and_then(Value::as_str)
            .ok_or(McpError::Invalid("MCP server version is missing"))?;
        let wire_protocol_version = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .ok_or(McpError::Invalid(
                "MCP initialize result protocolVersion is missing or malformed",
            ))?;
        validate_negotiated_protocol(mode, wire_protocol_version)?;
        Ok(McpServerInfo {
            name: name.to_owned(),
            version: version.to_owned(),
            protocol_version: MCP_PROTOCOL_VERSION,
            wire_protocol_version: Some(wire_protocol_version.to_owned()),
            metadata: [("wire_protocol".to_owned(), wire_protocol_version.to_owned())]
                .into_iter()
                .collect(),
        })
    }

    fn initialized(&mut self, _mode: McpProtocolMode) -> Result<(), McpError> {
        self.send_notification("notifications/initialized", json!({}))
    }

    fn request(
        &mut self,
        mode: McpProtocolMode,
        request: &PluginRequest,
    ) -> Result<Vec<u8>, McpError> {
        let payload: Value = serde_json::from_slice(&request.payload)
            .map_err(|_| McpError::Invalid("MCP request payload must be JSON"))?;
        let mut params = payload.as_object().cloned().ok_or(McpError::Invalid(
            "MCP request payload must be a JSON object",
        ))?;
        if mode == McpProtocolMode::Modern2026 {
            let mut metadata = Map::new();
            metadata.insert(
                "io.modelcontextprotocol/protocolVersion".to_owned(),
                Value::String(mode.version().to_owned()),
            );
            let client = self.client.as_ref().ok_or(McpError::Invalid(
                "MCP modern request has no client identity",
            ))?;
            metadata.insert(
                "io.modelcontextprotocol/clientInfo".to_owned(),
                json!({"name": client.name, "version": client.version}),
            );
            params.insert("_meta".to_owned(), Value::Object(metadata));
        }
        let result = self.send_request_value(
            mode,
            request.request_id,
            &request.method,
            Value::Object(params),
        )?;
        let payload = serde_json::to_vec(&result)
            .map_err(|_| McpError::Invalid("MCP result could not be encoded"))?;
        if payload.len() > self.manifest.limits.max_message_bytes as usize {
            return Err(McpError::TooLarge(payload.len()));
        }
        Ok(payload)
    }
}

/// A bounded request/response Streamable HTTP MCP transport.
///
/// Modern requests carry the protocol and client metadata on every request.
/// Legacy sessions additionally retain the server-provided session header.
/// Responses may be JSON or bounded server-sent events. Server requests found
/// in an active response stream are dispatched only when the host supplies an
/// explicit handler. A caller can also open a bounded GET stream and resume it
/// with `Last-Event-ID`; SSE retry hints trigger at most two bounded automatic
/// reconnects. Broader bidirectional session behavior remains outside this
/// transport.
pub struct HttpMcpTransport {
    endpoint: Url,
    endpoint_text: String,
    manifest: PluginManifest,
    client: Client,
    capability_policy: Option<orynth_security::CapabilityPolicy>,
    capability_agent: Option<AgentId>,
    capability_task: Option<TaskId>,
    ownership_authorized: bool,
    client_info: Option<McpClientInfo>,
    session_id: Option<String>,
    server_request_handler: Option<Box<McpServerRequestHandler>>,
}

/// Host-owned handler for a server request received in an active SSE response.
pub type McpServerRequestHandler = dyn FnMut(u64, &str, Value) -> Result<Value, McpError> + Send;

/// One bounded JSON-RPC message delivered by an MCP SSE stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpSseEvent {
    pub id: Option<String>,
    pub message: Value,
}

impl HttpMcpTransport {
    pub fn new(
        endpoint: impl Into<String>,
        manifest: PluginManifest,
        timeout: Duration,
    ) -> Result<Self, McpError> {
        manifest.validate().map_err(McpError::Plugin)?;
        if manifest.kind != PluginKind::Mcp {
            return Err(McpError::Invalid("MCP HTTP requires an MCP manifest"));
        }
        if timeout.is_zero() {
            return Err(McpError::Invalid("MCP HTTP timeout must be non-zero"));
        }
        let endpoint_text = endpoint.into();
        reject_ambiguous_url_text(&endpoint_text)?;
        let endpoint = Url::parse(&endpoint_text)
            .map_err(|_| McpError::Invalid("MCP HTTP endpoint is not a valid URL"))?;
        let endpoint = canonical_network_url(endpoint)?;
        let endpoint_text = endpoint.to_string();
        if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host_str().is_none() {
            return Err(McpError::Invalid(
                "MCP HTTP endpoint must use HTTP(S) with a host",
            ));
        }
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            // Installation is a process-wide compare-and-set. Another HTTP
            // transport may win the race after the read above; that benign
            // outcome still leaves the provider configured for this client.
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
        let client = Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                transport_error(format!("could not build MCP HTTP client: {error}"))
            })?;
        Ok(Self {
            endpoint,
            endpoint_text,
            manifest,
            client,
            capability_policy: None,
            capability_agent: None,
            capability_task: None,
            ownership_authorized: false,
            client_info: None,
            session_id: None,
            server_request_handler: None,
        })
    }

    /// Attach the explicit host handler used for active-stream server requests.
    pub fn with_server_request_handler<F>(mut self, handler: F) -> Self
    where
        F: FnMut(u64, &str, Value) -> Result<Value, McpError> + Send + 'static,
    {
        self.server_request_handler = Some(Box::new(handler));
        self
    }

    /// Open a bounded GET SSE stream.
    ///
    /// `last_event_id` enables caller-driven resumption. Supplying
    /// `request_id` marks the stream as a resumption of a prior request; an
    /// unsolicited stream rejects JSON-RPC responses.
    pub fn open_event_stream(
        &mut self,
        mode: McpProtocolMode,
        last_event_id: Option<&str>,
        request_id: Option<u64>,
    ) -> Result<HttpMcpEventStream<'_>, McpError> {
        if request_id == Some(0) {
            return Err(McpError::Invalid(
                "MCP HTTP event-stream request ID must be non-zero",
            ));
        }
        if let Some(last_event_id) = last_event_id {
            validate_sse_event_id(last_event_id)?;
        }
        let response = self.send_get_sse(mode, last_event_id)?;
        Ok(HttpMcpEventStream {
            transport: self,
            mode,
            request_id,
            response,
            buffer: Vec::new(),
            last_event_id: last_event_id.map(str::to_owned),
            ended: false,
            retry_after_ms: None,
            reconnects: 0,
        })
    }

    fn send_get_sse(
        &mut self,
        mode: McpProtocolMode,
        last_event_id: Option<&str>,
    ) -> Result<Response, McpError> {
        self.reauthorize()?;
        if let Some(last_event_id) = last_event_id {
            validate_sse_event_id(last_event_id)?;
        }
        let mut request = self
            .client
            .get(self.endpoint.clone())
            .header("accept", "text/event-stream")
            .header("MCP-Protocol-Version", mode.version());
        if let Some(session_id) = &self.session_id {
            request = request.header("Mcp-Session-Id", session_id);
        }
        if let Some(last_event_id) = last_event_id {
            request = request.header("Last-Event-ID", last_event_id);
        }
        let response = request
            .send()
            .map_err(|error| transport_error(format!("MCP HTTP SSE GET failed: {error}")))?;
        if !response.status().is_success() {
            return Err(transport_error(format!(
                "MCP HTTP SSE GET returned status {}",
                response.status()
            )));
        }
        if let Some(session_id) = response.headers().get("Mcp-Session-Id") {
            let session_id = session_id
                .to_str()
                .map_err(|_| McpError::Invalid("MCP session header is not valid text"))?;
            if session_id.is_empty() || session_id.len() > MAX_DISCOVERY_CURSOR_BYTES {
                return Err(McpError::TooLarge(session_id.len()));
            }
            self.session_id = Some(session_id.to_owned());
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim)
            .unwrap_or_default()
            .to_ascii_lowercase();
        if content_type != "text/event-stream" {
            return Err(McpError::Invalid(
                "MCP HTTP SSE GET response is not an event stream",
            ));
        }
        Ok(response)
    }

    pub fn connect(
        mut self,
        mode: McpProtocolMode,
        client: McpClientInfo,
        server: McpServerInfo,
        context: McpConnectionContext<'_>,
    ) -> Result<McpSession<Self>, McpError> {
        self.client_info = Some(client.clone());
        authorize_manifest_with_ownership(
            context.policy,
            context.ownership,
            context.agent_id,
            context.task_id,
            context.now_ms,
            &self.manifest,
        )
        .map_err(McpError::Plugin)?;
        if !self.manifest.capabilities.iter().any(|capability| {
            capability.domain == orynth_security::CapabilityDomain::Network
                && network_resource_matches(&capability.resource, &self.endpoint)
        }) {
            return Err(McpError::Plugin(PluginError::Invalid(
                "MCP HTTP network endpoint is missing from the manifest",
            )));
        }
        context
            .policy
            .authorize(
                context.agent_id,
                context.task_id,
                orynth_security::CapabilityDomain::Network,
                &self.endpoint_text,
                context.now_ms,
            )
            .map_err(|error| {
                transport_error(format!("MCP HTTP network capability denied: {error}"))
            })?;
        context
            .ownership
            .authorize(
                context.agent_id,
                &self.endpoint_text,
                orynth_security::OwnershipAccess::Write,
            )
            .map_err(|error| {
                transport_error(format!("MCP HTTP network ownership denied: {error}"))
            })?;
        self.capability_policy = Some(context.policy.clone());
        self.capability_agent = Some(context.agent_id);
        self.capability_task = context.task_id;
        self.ownership_authorized = true;
        let mut session = McpSession::new(mode, client, server, self)?;
        session.connect()?;
        Ok(session)
    }

    fn send_json(
        &mut self,
        mode: McpProtocolMode,
        request_id: Option<u64>,
        method: &str,
        params: Value,
    ) -> Result<Option<Value>, McpError> {
        self.reauthorize()?;
        let message = if let Some(request_id) = request_id {
            json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "method": method,
                "params": params,
            })
        } else {
            json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params,
            })
        };
        let body = serde_json::to_vec(&message)
            .map_err(|_| McpError::Invalid("MCP HTTP request could not be encoded"))?;
        if body.len() > self.manifest.limits.max_message_bytes as usize {
            return Err(McpError::TooLarge(body.len()));
        }
        let mut request = self
            .client
            .post(self.endpoint.clone())
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(body);
        request = request.header("MCP-Protocol-Version", mode.version());
        if mode == McpProtocolMode::Modern2026 {
            request = request.header("Mcp-Method", method);
            let name = params.get("name").and_then(Value::as_str).unwrap_or(method);
            request = request.header("Mcp-Name", name);
        }
        if let Some(session_id) = &self.session_id {
            request = request.header("Mcp-Session-Id", session_id);
        }
        let response = request
            .send()
            .map_err(|error| transport_error(format!("MCP HTTP request failed: {error}")))?;
        if !response.status().is_success() {
            return Err(transport_error(format!(
                "MCP HTTP server returned status {}",
                response.status()
            )));
        }
        if let Some(session_id) = response.headers().get("Mcp-Session-Id") {
            let session_id = session_id
                .to_str()
                .map_err(|_| McpError::Invalid("MCP session header is not valid text"))?;
            if session_id.is_empty() || session_id.len() > MAX_DISCOVERY_CURSOR_BYTES {
                return Err(McpError::TooLarge(session_id.len()));
            }
            self.session_id = Some(session_id.to_owned());
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim)
            .unwrap_or_default()
            .to_ascii_lowercase();
        if content_type == "text/event-stream" {
            return self.read_sse_response(mode, request_id, response);
        }
        if !content_type.is_empty() && content_type != "application/json" {
            return Err(McpError::Invalid(
                "MCP HTTP response content type is unsupported",
            ));
        }
        let max_message_bytes = self.manifest.limits.max_message_bytes as usize;
        let mut bytes = Vec::new();
        response
            .take(max_message_bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                transport_error(format!("could not read MCP HTTP response: {error}"))
            })?;
        if bytes.len() > max_message_bytes {
            return Err(McpError::TooLarge(bytes.len()));
        }
        if request_id.is_none() && bytes.is_empty() {
            return Ok(None);
        }
        let response: Value = serde_json::from_slice(&bytes)
            .map_err(|_| McpError::Invalid("MCP HTTP response is not valid JSON"))?;
        self.process_json_message(mode, request_id, response)
    }

    fn read_sse_response(
        &mut self,
        mode: McpProtocolMode,
        request_id: Option<u64>,
        response: Response,
    ) -> Result<Option<Value>, McpError> {
        let mut response = response;
        for reconnect in 0..=MAX_SSE_RECONNECTS {
            let outcome = self.consume_sse_response(mode, request_id, response)?;
            if let Some(result) = outcome.result {
                return Ok(Some(result));
            }
            if request_id.is_none() {
                return Ok(None);
            }
            let last_event_id = outcome.last_event_id.ok_or(McpError::Invalid(
                "MCP HTTP SSE stream ended without a resumable event ID",
            ))?;
            if reconnect == MAX_SSE_RECONNECTS {
                return Err(McpError::Invalid(
                    "MCP HTTP SSE reconnect limit reached without a matching response",
                ));
            }
            sleep_sse_retry(outcome.retry_after_ms);
            response = self.send_get_sse(mode, Some(&last_event_id))?;
        }
        unreachable!("bounded MCP SSE reconnect loop must return")
    }

    fn consume_sse_response(
        &mut self,
        mode: McpProtocolMode,
        request_id: Option<u64>,
        mut response: Response,
    ) -> Result<SseReadOutcome, McpError> {
        let max_message_bytes = self.manifest.limits.max_message_bytes as usize;
        let max_event_bytes = max_message_bytes.min(MAX_SSE_EVENT_BYTES);
        let mut buffer = Vec::new();
        let mut last_event_id = None;
        let mut retry_after_ms = None;
        let mut chunk = [0_u8; 8192];
        loop {
            let read = response.read(&mut chunk).map_err(|error| {
                transport_error(format!("could not read MCP HTTP SSE response: {error}"))
            })?;
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
            if buffer.len() > max_event_bytes {
                return Err(McpError::TooLarge(buffer.len()));
            }
            while let Some((event_end, delimiter_len)) = find_sse_event_end(&buffer) {
                let event = buffer.drain(..event_end).collect::<Vec<_>>();
                buffer.drain(..delimiter_len);
                if let Some(event) = parse_sse_event(&event)? {
                    update_sse_cursor(&mut last_event_id, &mut retry_after_ms, &event);
                    if let Some(message) = event.message
                        && let Some(result) =
                            self.process_json_message(mode, request_id, message)?
                    {
                        return Ok(SseReadOutcome {
                            result: Some(result),
                            last_event_id,
                            retry_after_ms,
                        });
                    }
                }
            }
        }
        if !buffer.is_empty()
            && let Some(event) = parse_sse_event(&buffer)?
        {
            update_sse_cursor(&mut last_event_id, &mut retry_after_ms, &event);
            if let Some(message) = event.message
                && let Some(result) = self.process_json_message(mode, request_id, message)?
            {
                return Ok(SseReadOutcome {
                    result: Some(result),
                    last_event_id,
                    retry_after_ms,
                });
            }
        }
        Ok(SseReadOutcome {
            result: None,
            last_event_id,
            retry_after_ms,
        })
    }

    fn process_json_message(
        &mut self,
        mode: McpProtocolMode,
        request_id: Option<u64>,
        response: Value,
    ) -> Result<Option<Value>, McpError> {
        if response.get("jsonrpc") != Some(&Value::String("2.0".to_owned())) {
            return Err(McpError::Invalid(
                "MCP HTTP message has an invalid JSON-RPC version",
            ));
        }
        if let Some(method) = response.get("method").and_then(Value::as_str) {
            if let Some(id) = response.get("id") {
                let server_request_id = id.as_u64().ok_or(McpError::Invalid(
                    "MCP HTTP server request ID must be numeric",
                ))?;
                self.handle_server_request(
                    mode,
                    server_request_id,
                    method,
                    response.get("params").cloned().unwrap_or_else(|| json!({})),
                )?;
            }
            return Ok(None);
        }
        let request_id = request_id.ok_or(McpError::Invalid(
            "MCP HTTP notification returned a response",
        ))?;
        if response.get("jsonrpc") != Some(&Value::String("2.0".to_owned()))
            || response.get("id") != Some(&Value::from(request_id))
        {
            return Err(McpError::Invalid("MCP HTTP response identity is invalid"));
        }
        if response.get("error").is_some() {
            return Err(McpError::Invalid(
                "MCP HTTP server returned a JSON-RPC error",
            ));
        }
        Ok(Some(response.get("result").cloned().ok_or(
            McpError::Invalid("MCP HTTP response has no result"),
        )?))
    }

    fn handle_server_request(
        &mut self,
        mode: McpProtocolMode,
        request_id: u64,
        method: &str,
        params: Value,
    ) -> Result<(), McpError> {
        let handler = self
            .server_request_handler
            .as_mut()
            .ok_or(McpError::Invalid(
                "MCP HTTP server request has no host handler",
            ))?;
        let response = handler(request_id, method, params);
        let message = match response {
            Ok(result) => json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "result": result,
            }),
            Err(_) => json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "error": {"code": -32603, "message": "MCP host handler failed"},
            }),
        };
        self.send_server_response(mode, message)
    }

    fn send_server_response(
        &mut self,
        mode: McpProtocolMode,
        message: Value,
    ) -> Result<(), McpError> {
        self.reauthorize()?;
        let body = serde_json::to_vec(&message)
            .map_err(|_| McpError::Invalid("MCP HTTP server response could not be encoded"))?;
        if body.len() > self.manifest.limits.max_message_bytes as usize {
            return Err(McpError::TooLarge(body.len()));
        }
        let mut request = self
            .client
            .post(self.endpoint.clone())
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", mode.version())
            .body(body);
        if let Some(session_id) = &self.session_id {
            request = request.header("Mcp-Session-Id", session_id);
        }
        let response = request.send().map_err(|error| {
            transport_error(format!("MCP HTTP server response failed: {error}"))
        })?;
        if !response.status().is_success() {
            return Err(transport_error(format!(
                "MCP HTTP server rejected the server response with status {}",
                response.status()
            )));
        }
        Ok(())
    }

    fn reauthorize(&self) -> Result<(), McpError> {
        if !self.ownership_authorized {
            return Err(McpError::Invalid(
                "MCP HTTP transport has no ownership authorization",
            ));
        }
        let policy = self.capability_policy.as_ref().ok_or(McpError::Invalid(
            "MCP HTTP transport has no capability context",
        ))?;
        policy
            .authorize(
                self.capability_agent
                    .ok_or(McpError::Invalid("MCP HTTP transport has no agent context"))?,
                self.capability_task,
                orynth_security::CapabilityDomain::Network,
                &self.endpoint_text,
                current_time_ms(),
            )
            .map_err(|error| transport_error(format!("MCP HTTP capability denied: {error}")))
    }
}

/// A caller-driven, bounded MCP GET SSE stream.
pub struct HttpMcpEventStream<'a> {
    transport: &'a mut HttpMcpTransport,
    mode: McpProtocolMode,
    request_id: Option<u64>,
    response: Response,
    buffer: Vec<u8>,
    last_event_id: Option<String>,
    ended: bool,
    retry_after_ms: Option<u64>,
    reconnects: usize,
}

impl HttpMcpEventStream<'_> {
    pub fn last_event_id(&self) -> Option<&str> {
        self.last_event_id.as_deref()
    }

    pub fn next_event(&mut self) -> Result<Option<McpSseEvent>, McpError> {
        loop {
            while let Some((event_end, delimiter_len)) = find_sse_event_end(&self.buffer) {
                let event = self.buffer.drain(..event_end).collect::<Vec<_>>();
                self.buffer.drain(..delimiter_len);
                if let Some(event) = parse_sse_event(&event)? {
                    update_sse_cursor(&mut self.last_event_id, &mut self.retry_after_ms, &event);
                    if let Some(message) = event.message {
                        return self.process_message(event.id, message).map(Some);
                    }
                }
            }
            if self.ended {
                if !self.buffer.is_empty() {
                    let event = parse_sse_event(&self.buffer)?;
                    self.buffer.clear();
                    if let Some(event) = event {
                        update_sse_cursor(
                            &mut self.last_event_id,
                            &mut self.retry_after_ms,
                            &event,
                        );
                        if let Some(message) = event.message {
                            return self.process_message(event.id, message).map(Some);
                        }
                    }
                }
                if self.retry_after_ms.is_some()
                    && self.last_event_id.is_some()
                    && self.reconnects < MAX_SSE_RECONNECTS
                {
                    let last_event_id = self.last_event_id.clone().expect("checked above");
                    sleep_sse_retry(self.retry_after_ms);
                    self.response = self
                        .transport
                        .send_get_sse(self.mode, Some(&last_event_id))?;
                    self.buffer.clear();
                    self.ended = false;
                    self.retry_after_ms = None;
                    self.reconnects += 1;
                    continue;
                }
                return Ok(None);
            }
            let mut chunk = [0_u8; 8192];
            let read = self.response.read(&mut chunk).map_err(|error| {
                transport_error(format!("could not read MCP HTTP SSE GET response: {error}"))
            })?;
            if read == 0 {
                self.ended = true;
            } else {
                self.buffer.extend_from_slice(&chunk[..read]);
                let max_event_bytes = self
                    .transport
                    .manifest
                    .limits
                    .max_message_bytes
                    .try_into()
                    .unwrap_or(MAX_SSE_EVENT_BYTES)
                    .min(MAX_SSE_EVENT_BYTES);
                if self.buffer.len() > max_event_bytes {
                    return Err(McpError::TooLarge(self.buffer.len()));
                }
            }
        }
    }

    fn process_message(
        &mut self,
        event_id: Option<String>,
        message: Value,
    ) -> Result<McpSseEvent, McpError> {
        if message.get("jsonrpc") != Some(&Value::String("2.0".to_owned())) {
            return Err(McpError::Invalid(
                "MCP HTTP SSE message has an invalid JSON-RPC version",
            ));
        }
        if let Some(method) = message.get("method").and_then(Value::as_str) {
            if let Some(id) = message.get("id") {
                let request_id = id.as_u64().ok_or(McpError::Invalid(
                    "MCP HTTP SSE server request ID must be numeric",
                ))?;
                self.transport.handle_server_request(
                    self.mode,
                    request_id,
                    method,
                    message.get("params").cloned().unwrap_or_else(|| json!({})),
                )?;
            }
        } else {
            let request_id = self.request_id.ok_or(McpError::Invalid(
                "MCP HTTP unsolicited SSE stream returned a response",
            ))?;
            if message.get("id") != Some(&Value::from(request_id)) {
                return Err(McpError::Invalid(
                    "MCP HTTP SSE response identity is invalid",
                ));
            }
        }
        Ok(McpSseEvent {
            id: event_id,
            message,
        })
    }
}

fn find_sse_event_end(buffer: &[u8]) -> Option<(usize, usize)> {
    if let Some(index) = buffer.windows(2).position(|window| window == b"\n\n") {
        return Some((index, 2));
    }
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| (index, 4))
}

struct ParsedSseEvent {
    id: Option<String>,
    message: Option<Value>,
    retry_after_ms: Option<u64>,
}

fn parse_sse_event(event: &[u8]) -> Result<Option<ParsedSseEvent>, McpError> {
    let text = std::str::from_utf8(event)
        .map_err(|_| McpError::Invalid("MCP HTTP SSE event is not UTF-8"))?;
    let mut data = String::new();
    let mut id = None;
    let mut retry_after_ms = None;
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(value) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(value.strip_prefix(' ').unwrap_or(value));
        } else if let Some(value) = line.strip_prefix("id:") {
            let value = value.strip_prefix(' ').unwrap_or(value);
            validate_sse_event_id(value)?;
            id = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("retry:")
            && let Ok(value) = value
                .strip_prefix(' ')
                .unwrap_or(value)
                .trim()
                .parse::<u64>()
        {
            retry_after_ms = Some(value.min(MAX_SSE_RETRY_DELAY_MS));
        }
    }
    if data.trim().is_empty() {
        return Ok(Some(ParsedSseEvent {
            id,
            message: None,
            retry_after_ms,
        }));
    }
    serde_json::from_str(&data)
        .map(|message| {
            Some(ParsedSseEvent {
                id,
                message: Some(message),
                retry_after_ms,
            })
        })
        .map_err(|_| McpError::Invalid("MCP HTTP SSE event data is not valid JSON"))
}

struct SseReadOutcome {
    result: Option<Value>,
    last_event_id: Option<String>,
    retry_after_ms: Option<u64>,
}

fn update_sse_cursor(
    last_event_id: &mut Option<String>,
    retry_after_ms: &mut Option<u64>,
    event: &ParsedSseEvent,
) {
    if let Some(id) = event.id.as_deref() {
        *last_event_id = if id.is_empty() {
            None
        } else {
            Some(id.to_owned())
        };
    }
    if event.retry_after_ms.is_some() {
        *retry_after_ms = event.retry_after_ms;
    }
}

fn sleep_sse_retry(retry_after_ms: Option<u64>) {
    if let Some(retry_after_ms) = retry_after_ms {
        thread::sleep(Duration::from_millis(retry_after_ms));
    }
}

fn validate_sse_event_id(event_id: &str) -> Result<(), McpError> {
    if event_id.len() > MAX_DISCOVERY_CURSOR_BYTES
        || !event_id.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(McpError::Invalid(
            "MCP SSE event ID is not valid visible ASCII",
        ));
    }
    Ok(())
}

impl McpSessionTransport for HttpMcpTransport {
    fn initialize(
        &mut self,
        mode: McpProtocolMode,
        client: &McpClientInfo,
    ) -> Result<McpServerInfo, McpError> {
        self.client_info = Some(client.clone());
        let result = self
            .send_json(
                mode,
                Some(1),
                "initialize",
                json!({
                    "protocolVersion": mode.version(),
                    "capabilities": {},
                    "clientInfo": {"name": client.name, "version": client.version},
                }),
            )?
            .ok_or(McpError::Invalid("MCP HTTP initialize returned no result"))?;
        let server =
            result
                .get("serverInfo")
                .and_then(Value::as_object)
                .ok_or(McpError::Invalid(
                    "MCP HTTP initialize result has no serverInfo",
                ))?;
        let wire_protocol_version = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .ok_or(McpError::Invalid(
                "MCP HTTP initialize result protocolVersion is missing or malformed",
            ))?;
        validate_negotiated_protocol(mode, wire_protocol_version)?;
        Ok(McpServerInfo {
            name: server
                .get("name")
                .and_then(Value::as_str)
                .ok_or(McpError::Invalid("MCP server name is missing"))?
                .to_owned(),
            version: server
                .get("version")
                .and_then(Value::as_str)
                .ok_or(McpError::Invalid("MCP server version is missing"))?
                .to_owned(),
            protocol_version: MCP_PROTOCOL_VERSION,
            wire_protocol_version: Some(wire_protocol_version.to_owned()),
            metadata: [("wire_protocol".to_owned(), wire_protocol_version.to_owned())]
                .into_iter()
                .collect(),
        })
    }

    fn initialized(&mut self, mode: McpProtocolMode) -> Result<(), McpError> {
        self.send_json(mode, None, "notifications/initialized", json!({}))?;
        Ok(())
    }

    fn request(
        &mut self,
        mode: McpProtocolMode,
        request: &PluginRequest,
    ) -> Result<Vec<u8>, McpError> {
        let payload: Value = serde_json::from_slice(&request.payload)
            .map_err(|_| McpError::Invalid("MCP HTTP request payload must be JSON"))?;
        let mut params = payload.as_object().cloned().ok_or(McpError::Invalid(
            "MCP HTTP request payload must be a JSON object",
        ))?;
        if mode == McpProtocolMode::Modern2026 {
            let client = self
                .client_info
                .as_ref()
                .ok_or(McpError::Invalid("MCP HTTP request has no client identity"))?;
            params.insert(
                "_meta".to_owned(),
                json!({
                    "io.modelcontextprotocol/protocolVersion": mode.version(),
                    "io.modelcontextprotocol/clientInfo": {"name": client.name, "version": client.version},
                }),
            );
        }
        let result = self
            .send_json(
                mode,
                Some(request.request_id),
                &request.method,
                Value::Object(params),
            )?
            .ok_or(McpError::Invalid("MCP HTTP request returned no result"))?;
        serde_json::to_vec(&result)
            .map_err(|_| McpError::Invalid("MCP HTTP result could not be encoded"))
    }
}

fn transport_error(message: String) -> McpError {
    McpError::Plugin(PluginError::Protocol(message))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NetworkTarget {
    scheme: String,
    host: String,
    port: u16,
    path: String,
    query: Option<String>,
}

fn canonical_network_url(mut url: Url) -> Result<Url, McpError> {
    if url.username() != "" || url.password().is_some() || url.fragment().is_some() {
        return Err(McpError::Invalid(
            "MCP HTTP endpoint must not contain credentials or a fragment",
        ));
    }
    let target = network_target(&url)?;
    url.set_path(&target.path);
    url.set_query(target.query.as_deref());
    Ok(url)
}

fn reject_ambiguous_url_text(endpoint: &str) -> Result<(), McpError> {
    let lower = endpoint.to_ascii_lowercase();
    if lower.contains("%2e") || lower.contains("%2f") || lower.contains("%5c") {
        return Err(McpError::Invalid(
            "MCP HTTP endpoint contains an encoded path ambiguity",
        ));
    }
    Ok(())
}

fn network_target(url: &Url) -> Result<NetworkTarget, McpError> {
    let host = url
        .host_str()
        .ok_or(McpError::Invalid("MCP HTTP endpoint must have a host"))?
        .to_ascii_lowercase();
    let port = url.port_or_known_default().ok_or(McpError::Invalid(
        "MCP HTTP endpoint must have a known port",
    ))?;
    let raw_path = url.path();
    let lower_path = raw_path.to_ascii_lowercase();
    if lower_path.contains("%2e")
        || lower_path.contains("%2f")
        || lower_path.contains("%5c")
        || raw_path.contains('\\')
    {
        return Err(McpError::Invalid(
            "MCP HTTP endpoint contains an encoded or alternate path separator",
        ));
    }
    let mut components = Vec::new();
    for component in raw_path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if components.pop().is_none() {
                    return Err(McpError::Invalid("MCP HTTP endpoint escapes its root"));
                }
            }
            value => components.push(value),
        }
    }
    let path = format!("/{}", components.join("/"));
    Ok(NetworkTarget {
        scheme: url.scheme().to_ascii_lowercase(),
        host,
        port,
        path: if path == "/" {
            path
        } else {
            path.trim_end_matches('/').to_owned()
        },
        query: url.query().map(str::to_owned),
    })
}

fn network_resource_matches(granted: &str, requested: &Url) -> bool {
    let Ok(granted_url) = Url::parse(granted) else {
        return false;
    };
    let Ok(granted) = network_target(&granted_url) else {
        return false;
    };
    let Ok(requested) = network_target(requested) else {
        return false;
    };
    if granted.scheme != requested.scheme
        || granted.host != requested.host
        || granted.port != requested.port
        || granted.query != requested.query
    {
        return false;
    }
    granted.path == requested.path
        || (requested.path.starts_with(&granted.path)
            && requested
                .path
                .as_bytes()
                .get(granted.path.len())
                .is_some_and(|separator| *separator == b'/'))
}

fn current_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

pub struct McpAdapter<I> {
    manifest: PluginManifest,
    server: McpServerInfo,
    invoker: I,
}

impl<I> McpAdapter<I> {
    pub fn new(
        manifest: PluginManifest,
        server: McpServerInfo,
        invoker: I,
    ) -> Result<Self, McpError> {
        manifest.validate().map_err(McpError::Plugin)?;
        if manifest.kind != PluginKind::Mcp {
            return Err(McpError::Invalid("MCP adapter requires an MCP manifest"));
        }
        server.validate()?;
        Ok(Self {
            manifest,
            server,
            invoker,
        })
    }

    pub fn server(&self) -> &McpServerInfo {
        &self.server
    }

    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }
}

impl<I: McpInvoker> PluginTransport for McpAdapter<I> {
    fn invoke(
        &mut self,
        policy: &orynth_security::CapabilityPolicy,
        ownership: &dyn orynth_security::ResourceOwnershipPolicy,
        agent_id: AgentId,
        task_id: Option<TaskId>,
        now_ms: u128,
        manifest: &PluginManifest,
        request: PluginRequest,
    ) -> Result<PluginResponse, PluginError> {
        if manifest != &self.manifest {
            return Err(PluginError::Protocol(
                "invocation manifest does not match the bound MCP server".to_owned(),
            ));
        }
        authorize_manifest_with_ownership(
            policy,
            ownership,
            agent_id,
            task_id,
            now_ms,
            &self.manifest,
        )?;
        request.validate_with(self.manifest.limits)?;
        let request_id = request.request_id;
        let payload = self.invoker.invoke(&request)?;
        let response = PluginResponse {
            request_id,
            payload,
            origin: TrustOrigin::McpResult,
        };
        response.validate_with(self.manifest.limits)?;
        Ok(response)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum McpError {
    Invalid(&'static str),
    UnsupportedVersion(u16),
    UnsupportedWireVersion(String),
    TooLarge(usize),
    Plugin(PluginError),
    Tool(ToolError),
}

impl std::fmt::Display for McpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => write!(formatter, "invalid MCP contract: {message}"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported MCP protocol version {version}")
            }
            Self::UnsupportedWireVersion(version) => {
                write!(formatter, "unsupported MCP wire protocol version {version}")
            }
            Self::TooLarge(size) => write!(formatter, "MCP value is too large: {size}"),
            Self::Plugin(error) => write!(formatter, "MCP plugin error: {error}"),
            Self::Tool(error) => write!(formatter, "MCP tool policy error: {error}"),
        }
    }
}

impl std::error::Error for McpError {}

fn validate_wire_protocol_version(version: &str) -> Result<(), McpError> {
    if version == MCP_LEGACY_PROTOCOL_VERSION || version == MCP_MODERN_PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(McpError::UnsupportedWireVersion(version.to_owned()))
    }
}

fn validate_negotiated_protocol(
    mode: McpProtocolMode,
    server_version: &str,
) -> Result<(), McpError> {
    validate_wire_protocol_version(server_version)?;
    if server_version != mode.version() {
        return Err(McpError::UnsupportedWireVersion(server_version.to_owned()));
    }
    Ok(())
}

fn validate_page_len(length: usize) -> Result<(), McpError> {
    if length > MAX_DISCOVERY_PAGE_ITEMS {
        Err(McpError::TooLarge(length))
    } else {
        Ok(())
    }
}

fn validate_cursor(cursor: Option<&str>) -> Result<(), McpError> {
    if cursor
        .is_some_and(|value| value.trim().is_empty() || value.len() > MAX_DISCOVERY_CURSOR_BYTES)
    {
        return Err(McpError::Invalid("MCP discovery cursor is invalid"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orynth_kernel::PluginId;
    use orynth_plugin_api::{PLUGIN_PROTOCOL_VERSION, PluginCapability, PluginResourceLimits};
    use orynth_security::{CapabilityDomain, CapabilityLease, CapabilityPolicy};

    #[derive(Default)]
    struct MockInvoker;

    impl McpInvoker for MockInvoker {
        fn invoke(&mut self, request: &PluginRequest) -> Result<Vec<u8>, PluginError> {
            Ok(request.payload.clone())
        }
    }

    #[derive(Default)]
    struct MockDiscovery {
        tool_calls: Vec<(Option<String>, u16)>,
        resource_calls: Vec<(Option<String>, u16)>,
    }

    impl McpDiscoveryInvoker for MockDiscovery {
        fn list_tools(
            &mut self,
            cursor: Option<&str>,
            limit: u16,
        ) -> Result<McpToolPage, McpError> {
            self.tool_calls.push((cursor.map(str::to_owned), limit));
            Ok(McpToolPage {
                tools: vec![McpToolDescription {
                    name: "read".to_owned(),
                    description: "reads a resource".to_owned(),
                    input_schema: b"{}".to_vec(),
                }],
                next_cursor: Some("next".to_owned()),
            })
        }

        fn list_resources(
            &mut self,
            cursor: Option<&str>,
            limit: u16,
        ) -> Result<McpResourcePage, McpError> {
            self.resource_calls.push((cursor.map(str::to_owned), limit));
            Ok(McpResourcePage {
                resources: vec![McpResourceDescription {
                    uri: "resource://one".to_owned(),
                    name: "one".to_owned(),
                    description: "first resource".to_owned(),
                }],
                next_cursor: None,
            })
        }
    }

    struct OversizedDiscovery;

    impl McpDiscoveryInvoker for OversizedDiscovery {
        fn list_tools(
            &mut self,
            _cursor: Option<&str>,
            _limit: u16,
        ) -> Result<McpToolPage, McpError> {
            Ok(McpToolPage {
                tools: (0..=MAX_DISCOVERY_PAGE_ITEMS)
                    .map(|index| McpToolDescription {
                        name: format!("tool-{index}"),
                        description: "tool".to_owned(),
                        input_schema: b"{}".to_vec(),
                    })
                    .collect(),
                next_cursor: None,
            })
        }

        fn list_resources(
            &mut self,
            _cursor: Option<&str>,
            _limit: u16,
        ) -> Result<McpResourcePage, McpError> {
            unreachable!("resource listing is not part of this test")
        }
    }

    #[derive(Default)]
    struct MockSession {
        initialize_calls: usize,
        initialized_calls: usize,
        requests: Vec<McpProtocolMode>,
    }

    impl McpSessionTransport for MockSession {
        fn initialize(
            &mut self,
            _mode: McpProtocolMode,
            _client: &McpClientInfo,
        ) -> Result<McpServerInfo, McpError> {
            self.initialize_calls += 1;
            Ok(server())
        }

        fn initialized(&mut self, _mode: McpProtocolMode) -> Result<(), McpError> {
            self.initialized_calls += 1;
            Ok(())
        }

        fn request(
            &mut self,
            mode: McpProtocolMode,
            request: &PluginRequest,
        ) -> Result<Vec<u8>, McpError> {
            self.requests.push(mode);
            Ok(request.payload.clone())
        }
    }

    struct NegotiationSession {
        response: McpServerInfo,
    }

    impl McpSessionTransport for NegotiationSession {
        fn initialize(
            &mut self,
            _mode: McpProtocolMode,
            _client: &McpClientInfo,
        ) -> Result<McpServerInfo, McpError> {
            Ok(self.response.clone())
        }

        fn initialized(&mut self, _mode: McpProtocolMode) -> Result<(), McpError> {
            Ok(())
        }

        fn request(
            &mut self,
            _mode: McpProtocolMode,
            _request: &PluginRequest,
        ) -> Result<Vec<u8>, McpError> {
            Ok(Vec::new())
        }
    }

    fn server() -> McpServerInfo {
        McpServerInfo {
            name: "files".to_owned(),
            version: "1".to_owned(),
            protocol_version: MCP_PROTOCOL_VERSION,
            wire_protocol_version: Some(MCP_LEGACY_PROTOCOL_VERSION.to_owned()),
            metadata: [("title".to_owned(), "Files".to_owned())]
                .into_iter()
                .collect(),
        }
    }

    fn client() -> McpClientInfo {
        McpClientInfo {
            name: "orynth".to_owned(),
            version: "0.1".to_owned(),
        }
    }

    fn manifest() -> PluginManifest {
        PluginManifest {
            id: PluginId::from_u64(3),
            protocol_version: PLUGIN_PROTOCOL_VERSION,
            name: "mcp.files".to_owned(),
            version: "1".to_owned(),
            kind: PluginKind::Mcp,
            capabilities: vec![PluginCapability {
                domain: CapabilityDomain::Filesystem,
                resource: "workspace".to_owned(),
            }],
            limits: PluginResourceLimits::default(),
        }
    }

    #[test]
    fn mcp_metadata_is_explicitly_untrusted() {
        assert_eq!(server().origin(), TrustOrigin::McpMetadata);
        assert!(!server().origin().is_trusted());
    }

    #[test]
    fn tool_binding_requires_host_policy_for_capabilities() {
        let description = McpToolDescription {
            name: "write".to_owned(),
            description: "writes a file".to_owned(),
            input_schema: b"{}".to_vec(),
        };
        let (definition, trust_policy) = bind_tool(
            &server(),
            &description,
            &McpToolPolicy {
                required_fields: vec!["path".to_owned()],
                capability: Some(CapabilityRequirement {
                    domain: CapabilityDomain::Filesystem,
                    resource: "workspace".to_owned(),
                    input_field: Some("path".to_owned()),
                    input_fields: Vec::new(),
                }),
                risk: RiskLevel::High,
                reversible: false,
                trust_policy: TrustPolicy::RequireApprovalForUntrusted,
            },
        )
        .unwrap();
        assert_eq!(definition.name, "mcp.files.write");
        assert_eq!(trust_policy, TrustPolicy::RequireApprovalForUntrusted);
    }

    #[test]
    fn mcp_results_are_marked_untrusted_at_the_adapter_boundary() {
        let manifest = manifest();
        let mut adapter = McpAdapter::new(manifest.clone(), server(), MockInvoker).unwrap();
        let agent_id = AgentId::from_u64(9);
        let mut policy = CapabilityPolicy::new();
        policy
            .grant(CapabilityLease {
                agent_id,
                task_id: None,
                domain: CapabilityDomain::Filesystem,
                resource: "workspace".to_owned(),
                expires_at_ms: u128::MAX,
            })
            .unwrap();
        let response = adapter
            .invoke(
                &policy,
                &orynth_security::AllowAllOwnership,
                agent_id,
                None,
                1,
                &manifest,
                PluginRequest {
                    request_id: 1,
                    method: "tools/call".to_owned(),
                    payload: vec![7],
                },
            )
            .unwrap();
        assert_eq!(response.origin, TrustOrigin::McpResult);
        assert!(!response.origin.is_trusted());
    }

    #[test]
    fn progressive_discovery_bounds_pages_and_preserves_untrusted_metadata() {
        let mut discovery =
            ProgressiveMcpDiscovery::new(server(), MockDiscovery::default(), 2).unwrap();
        let tools = discovery.list_tools(Some("cursor-1")).unwrap();
        assert_eq!(tools.tools.len(), 1);
        assert_eq!(tools.next_cursor.as_deref(), Some("next"));
        let resources = discovery.list_resources(None).unwrap();
        assert_eq!(resources.resources[0].uri, "resource://one");
        assert_eq!(discovery.server().origin(), TrustOrigin::McpMetadata);
    }

    #[test]
    fn progressive_discovery_rejects_invalid_cursors_and_oversized_pages() {
        assert!(matches!(
            ProgressiveMcpDiscovery::new(server(), MockDiscovery::default(), 0),
            Err(McpError::TooLarge(0))
        ));
        let mut discovery = ProgressiveMcpDiscovery::new(server(), OversizedDiscovery, 1).unwrap();
        assert!(matches!(
            discovery.list_tools(Some(&"x".repeat(MAX_DISCOVERY_CURSOR_BYTES + 1))),
            Err(McpError::Invalid(_))
        ));
        assert!(matches!(
            discovery.list_tools(None),
            Err(McpError::TooLarge(_))
        ));
    }

    #[test]
    fn network_authorization_uses_the_normalized_structured_destination() {
        let allowed = Url::parse("HTTP://Example.COM:80/allowed").unwrap();
        let normalized = canonical_network_url(allowed).unwrap();
        assert_eq!(normalized.as_str(), "http://example.com/allowed");
        let escaped =
            canonical_network_url(Url::parse("http://example.com/allowed/../private").unwrap())
                .unwrap();
        assert!(!network_resource_matches(
            "http://example.com/allowed",
            &escaped
        ));
        assert!(network_resource_matches(
            "http://EXAMPLE.com:80/allowed",
            &normalized
        ));
        assert!(!network_resource_matches(
            "http://example.com/allowed",
            &Url::parse("http://example.com/allowed-sibling").unwrap()
        ));
        assert!(canonical_network_url(Url::parse("http://[::1]/allowed").unwrap()).is_ok());
        assert!(reject_ambiguous_url_text("http://example.com/allowed/%2e%2e/private").is_err());
    }

    #[test]
    fn legacy_session_performs_handshake_before_requests() {
        let mut session = McpSession::new(
            McpProtocolMode::Legacy2025,
            client(),
            server(),
            MockSession::default(),
        )
        .unwrap();
        assert_eq!(session.mode().version(), MCP_LEGACY_PROTOCOL_VERSION);
        assert!(!session.is_connected());
        session.connect().unwrap();
        assert!(session.is_connected());
        assert_eq!(session.transport().initialize_calls, 1);
        assert_eq!(session.transport().initialized_calls, 1);
        let mut invoker = SessionMcpInvoker::new(session);
        let payload = invoker
            .invoke(&PluginRequest {
                request_id: 1,
                method: "tools/call".to_owned(),
                payload: vec![4],
            })
            .unwrap();
        assert_eq!(payload, vec![4]);
        assert_eq!(
            invoker.session().transport().requests,
            vec![McpProtocolMode::Legacy2025]
        );
        assert_eq!(
            invoker.session().negotiated_protocol_version(),
            Some(MCP_LEGACY_PROTOCOL_VERSION)
        );
    }

    #[test]
    fn legacy_handshake_rejects_missing_or_unsupported_wire_version() {
        for wire_protocol_version in [
            None,
            Some("2024-01-01".to_owned()),
            Some("2026-07-28".to_owned()),
        ] {
            let mut returned = server();
            returned.wire_protocol_version = wire_protocol_version;
            let mut session = McpSession::new(
                McpProtocolMode::Legacy2025,
                client(),
                server(),
                NegotiationSession { response: returned },
            )
            .unwrap();
            assert!(matches!(
                session.connect(),
                Err(McpError::Invalid(_)) | Err(McpError::UnsupportedWireVersion(_))
            ));
            assert!(!session.is_connected());
        }
    }

    #[test]
    fn modern_session_skips_legacy_handshake_and_requires_connect() {
        let session = McpSession::new(
            McpProtocolMode::Modern2026,
            client(),
            server(),
            MockSession::default(),
        )
        .unwrap();
        let mut invoker = SessionMcpInvoker::new(session);
        assert!(matches!(
            invoker.invoke(&PluginRequest {
                request_id: 1,
                method: "tools/list".to_owned(),
                payload: Vec::new(),
            }),
            Err(PluginError::Protocol(message)) if message.contains("not connected")
        ));
        invoker.session_mut().connect().unwrap();
        assert_eq!(invoker.session().transport().initialize_calls, 0);
        assert_eq!(invoker.session().transport().initialized_calls, 0);
        assert_eq!(
            invoker.session().mode().version(),
            MCP_MODERN_PROTOCOL_VERSION
        );
        assert!(
            invoker
                .invoke(&PluginRequest {
                    request_id: 2,
                    method: "tools/list".to_owned(),
                    payload: Vec::new(),
                })
                .is_ok()
        );
    }
}
