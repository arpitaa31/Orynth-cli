//! Host-owned plugin discovery, activation, and transport routing.
//!
//! Discovery is still metadata-only. `PluginHost` performs automatic startup
//! binding only for candidates admitted by an explicit allowlist policy, and
//! every later invocation crosses the adapter's capability boundary.

use std::{collections::BTreeMap, fmt, path::PathBuf, time::Duration};

use orynth_kernel::{AgentId, PluginId, TaskId};
use orynth_plugin_api::{
    MAX_PLUGINS, PluginError, PluginKind, PluginManifest, PluginRequest, PluginResponse,
    PluginTransport,
};
use orynth_plugin_discovery::{
    DiscoveredPlugin, DiscoveryError, DiscoveryPolicy, discover_directories,
};
use orynth_plugin_mcp::{
    HttpMcpTransport, McpAdapter, McpClientInfo, McpConnectionContext, McpError, McpProtocolMode,
    McpServerInfo, SessionMcpInvoker,
};
use orynth_plugin_process::{CommandProcessInvoker, ProcessPlugin, activate_discovered_process};
use orynth_plugin_wasm::{WasmPlugin, WasmiInvoker, activate_discovered_wasm};
use orynth_security::CapabilityPolicy;

const DEFAULT_ACTIVATED_KINDS: &[PluginKind] = &[PluginKind::Process, PluginKind::Wasm];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivationPolicy {
    pub max_plugins: usize,
    pub allowed_ids: Option<Vec<PluginId>>,
    pub allowed_kinds: Option<Vec<PluginKind>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpActivationSpec {
    pub endpoint: String,
    pub mode: McpProtocolMode,
    pub client: McpClientInfo,
    pub server: McpServerInfo,
    pub timeout: Duration,
}

impl McpActivationSpec {
    fn validate(&self) -> Result<(), PluginHostError> {
        if self.endpoint.trim().is_empty() {
            return Err(PluginHostError::Invalid("MCP activation endpoint is empty"));
        }
        if self.timeout.is_zero() {
            return Err(PluginHostError::Invalid("MCP activation timeout is zero"));
        }
        self.server.validate().map_err(PluginHostError::Mcp)
    }
}

#[derive(Clone, Copy)]
pub struct McpActivationContext<'a> {
    pub policy: &'a CapabilityPolicy,
    pub agent_id: AgentId,
    pub task_id: Option<TaskId>,
    pub now_ms: u128,
}

impl Default for ActivationPolicy {
    fn default() -> Self {
        Self {
            max_plugins: MAX_PLUGINS,
            allowed_ids: None,
            allowed_kinds: Some(DEFAULT_ACTIVATED_KINDS.to_vec()),
        }
    }
}

impl ActivationPolicy {
    fn validate(&self) -> Result<(), PluginHostError> {
        if self.max_plugins == 0 || self.max_plugins > MAX_PLUGINS {
            return Err(PluginHostError::Invalid(
                "activation plugin limit is invalid",
            ));
        }
        if self
            .allowed_ids
            .as_ref()
            .is_some_and(|ids| ids.is_empty() || ids.len() > MAX_PLUGINS)
        {
            return Err(PluginHostError::Invalid(
                "activation ID allowlist is invalid",
            ));
        }
        if self
            .allowed_kinds
            .as_ref()
            .is_some_and(|kinds| kinds.is_empty())
        {
            return Err(PluginHostError::Invalid(
                "activation kind allowlist is invalid",
            ));
        }
        Ok(())
    }

    fn permits(&self, candidate: &DiscoveredPlugin) -> bool {
        self.allowed_ids
            .as_ref()
            .is_none_or(|ids| ids.contains(&candidate.manifest.id))
            && self
                .allowed_kinds
                .as_ref()
                .is_none_or(|kinds| kinds.contains(&candidate.manifest.kind))
    }
}

pub enum ActivatedPlugin {
    Process(ProcessPlugin<CommandProcessInvoker>),
    Mcp(Box<McpAdapter<SessionMcpInvoker<HttpMcpTransport>>>),
    Wasm(WasmPlugin<WasmiInvoker>),
}

impl ActivatedPlugin {
    pub fn manifest(&self) -> &PluginManifest {
        match self {
            Self::Process(plugin) => plugin.manifest(),
            Self::Mcp(plugin) => plugin.manifest(),
            Self::Wasm(plugin) => plugin.manifest(),
        }
    }
}

impl PluginTransport for ActivatedPlugin {
    fn invoke(
        &mut self,
        policy: &CapabilityPolicy,
        agent_id: AgentId,
        task_id: Option<TaskId>,
        now_ms: u128,
        manifest: &PluginManifest,
        request: PluginRequest,
    ) -> Result<PluginResponse, PluginError> {
        match self {
            Self::Process(plugin) => {
                plugin.invoke(policy, agent_id, task_id, now_ms, manifest, request)
            }
            Self::Mcp(plugin) => {
                plugin.invoke(policy, agent_id, task_id, now_ms, manifest, request)
            }
            Self::Wasm(plugin) => {
                plugin.invoke(policy, agent_id, task_id, now_ms, manifest, request)
            }
        }
    }
}

#[derive(Default)]
pub struct PluginHost {
    plugins: BTreeMap<PluginId, ActivatedPlugin>,
}

impl PluginHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// Discover and bind all candidates admitted by `activation_policy`.
    ///
    /// Binding is staged in a temporary map and committed only after every
    /// selected candidate succeeds, so a malformed later plugin cannot leave a
    /// partially activated startup state. Process binding does not launch a
    /// child; invocation remains a separate policy-checked effect.
    pub fn activate_directories(
        &mut self,
        roots: &[PathBuf],
        discovery_policy: &DiscoveryPolicy,
        activation_policy: &ActivationPolicy,
    ) -> Result<usize, PluginHostError> {
        self.activate_directories_inner(roots, discovery_policy, activation_policy, None, None)
    }

    /// Discover and bind process/WASM plugins plus explicitly configured MCP
    /// candidates. MCP activation requires endpoint configuration and an
    /// agent-scoped capability context; no MCP candidate is implicitly given
    /// network access.
    pub fn activate_directories_with_mcp(
        &mut self,
        roots: &[PathBuf],
        discovery_policy: &DiscoveryPolicy,
        activation_policy: &ActivationPolicy,
        mcp_specs: &BTreeMap<PluginId, McpActivationSpec>,
        context: McpActivationContext<'_>,
    ) -> Result<usize, PluginHostError> {
        self.activate_directories_inner(
            roots,
            discovery_policy,
            activation_policy,
            Some(mcp_specs),
            Some(context),
        )
    }

    fn activate_directories_inner(
        &mut self,
        roots: &[PathBuf],
        discovery_policy: &DiscoveryPolicy,
        activation_policy: &ActivationPolicy,
        mcp_specs: Option<&BTreeMap<PluginId, McpActivationSpec>>,
        mcp_context: Option<McpActivationContext<'_>>,
    ) -> Result<usize, PluginHostError> {
        activation_policy.validate()?;
        let candidates = discover_directories(roots, discovery_policy)?;
        let selected = candidates
            .iter()
            .filter(|candidate| activation_policy.permits(candidate))
            .collect::<Vec<_>>();
        if selected.len() > activation_policy.max_plugins {
            return Err(PluginHostError::TooMany(selected.len()));
        }
        if let Some(mcp_specs) = mcp_specs {
            if mcp_specs.len() > MAX_PLUGINS {
                return Err(PluginHostError::TooMany(mcp_specs.len()));
            }
            for (id, spec) in mcp_specs {
                spec.validate()?;
                if !selected.iter().any(|candidate| {
                    candidate.manifest.id == *id && candidate.manifest.kind == PluginKind::Mcp
                }) {
                    return Err(PluginHostError::Invalid(
                        "MCP activation spec targets a non-selected MCP plugin",
                    ));
                }
            }
        }

        let mut staged = BTreeMap::new();
        for candidate in selected {
            if self.plugins.contains_key(&candidate.manifest.id) {
                return Err(PluginHostError::Plugin(PluginError::Duplicate(
                    candidate.manifest.id,
                )));
            }
            let plugin = activate_candidate(
                candidate,
                mcp_specs.and_then(|specs| specs.get(&candidate.manifest.id)),
                mcp_context,
            )?;
            staged.insert(candidate.manifest.id, plugin);
        }
        let activated = staged.len();
        self.plugins.extend(staged);
        Ok(activated)
    }

    pub fn get(&self, id: PluginId) -> Option<&ActivatedPlugin> {
        self.plugins.get(&id)
    }

    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    pub fn invoke(
        &mut self,
        id: PluginId,
        policy: &CapabilityPolicy,
        agent_id: AgentId,
        task_id: Option<TaskId>,
        now_ms: u128,
        request: PluginRequest,
    ) -> Result<PluginResponse, PluginHostError> {
        let plugin = self
            .plugins
            .get_mut(&id)
            .ok_or(PluginHostError::NotActivated(id))?;
        let manifest = plugin.manifest().clone();
        plugin
            .invoke(policy, agent_id, task_id, now_ms, &manifest, request)
            .map_err(PluginHostError::Plugin)
    }
}

fn activate_candidate(
    candidate: &DiscoveredPlugin,
    mcp_spec: Option<&McpActivationSpec>,
    mcp_context: Option<McpActivationContext<'_>>,
) -> Result<ActivatedPlugin, PluginHostError> {
    match candidate.manifest.kind {
        PluginKind::Process => Ok(ActivatedPlugin::Process(activate_discovered_process(
            candidate,
        )?)),
        PluginKind::Wasm => Ok(ActivatedPlugin::Wasm(activate_discovered_wasm(candidate)?)),
        PluginKind::Mcp => activate_mcp_candidate(candidate, mcp_spec, mcp_context),
        PluginKind::Builtin | PluginKind::Native => Err(PluginHostError::Invalid(
            "plugin kind has no host activation transport",
        )),
    }
}

fn activate_mcp_candidate(
    candidate: &DiscoveredPlugin,
    spec: Option<&McpActivationSpec>,
    context: Option<McpActivationContext<'_>>,
) -> Result<ActivatedPlugin, PluginHostError> {
    let spec = spec.ok_or(PluginHostError::Invalid(
        "MCP activation requires an explicit endpoint configuration",
    ))?;
    let context = context.ok_or(PluginHostError::Invalid(
        "MCP activation requires an agent capability context",
    ))?;
    let (manifest, _) = orynth_plugin_discovery::revalidate_discovered_plugin(candidate)?;
    let transport = HttpMcpTransport::new(&spec.endpoint, manifest.clone(), spec.timeout)
        .map_err(PluginHostError::Mcp)?;
    let session = transport
        .connect(
            spec.mode,
            spec.client.clone(),
            spec.server.clone(),
            McpConnectionContext {
                policy: context.policy,
                agent_id: context.agent_id,
                task_id: context.task_id,
                now_ms: context.now_ms,
            },
        )
        .map_err(PluginHostError::Mcp)?;
    let server = session.server().clone();
    let adapter = McpAdapter::new(manifest, server, SessionMcpInvoker::new(session))
        .map_err(PluginHostError::Mcp)?;
    Ok(ActivatedPlugin::Mcp(Box::new(adapter)))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginHostError {
    Invalid(&'static str),
    TooMany(usize),
    NotActivated(PluginId),
    Discovery(DiscoveryError),
    Plugin(PluginError),
    Mcp(McpError),
}

impl fmt::Display for PluginHostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(formatter, "invalid plugin host policy: {message}"),
            Self::TooMany(count) => write!(
                formatter,
                "too many plugins selected for activation: {count}"
            ),
            Self::NotActivated(id) => write!(formatter, "plugin {id} is not activated"),
            Self::Discovery(error) => write!(formatter, "plugin discovery failed: {error}"),
            Self::Plugin(error) => write!(formatter, "plugin activation failed: {error}"),
            Self::Mcp(error) => write!(formatter, "MCP activation failed: {error}"),
        }
    }
}

impl std::error::Error for PluginHostError {}

impl From<DiscoveryError> for PluginHostError {
    fn from(error: DiscoveryError) -> Self {
        Self::Discovery(error)
    }
}

impl From<PluginError> for PluginHostError {
    fn from(error: PluginError) -> Self {
        Self::Plugin(error)
    }
}

impl From<McpError> for PluginHostError {
    fn from(error: McpError) -> Self {
        Self::Mcp(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::BTreeMap,
        fs,
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        thread,
    };

    use orynth_kernel::{PluginId, TrustOrigin};
    use orynth_plugin_api::PluginRequest;
    use orynth_security::{CapabilityDomain, CapabilityLease};

    fn wasm_manifest_text(module: &std::path::Path) -> String {
        format!(
            "protocol_version=1\nid=91\nname=host.wasm\nversion=1\nkind=wasm\nentrypoint={}\ncapability=filesystem:workspace\nmax_message_bytes=128\nmax_memory_bytes=65536\nmax_fuel=10000\nmax_wall_time_ms=1000\n",
            module.display()
        )
    }

    fn mcp_manifest_text() -> &'static str {
        "protocol_version=1\nid=92\nname=host.mcp\nversion=1\nkind=mcp\nmax_message_bytes=128\nmax_memory_bytes=65536\nmax_fuel=10000\nmax_wall_time_ms=1000\n"
    }

    fn read_http_request(stream: &mut TcpStream) -> serde_json::Value {
        let mut headers = Vec::new();
        let mut byte = [0_u8; 1];
        while !headers.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
            assert!(headers.len() <= 16 * 1024);
        }
        let header_text = String::from_utf8(headers).unwrap();
        let content_length = header_text
            .lines()
            .find_map(|line| {
                line.strip_prefix("Content-Length:")
                    .or_else(|| line.strip_prefix("content-length:"))
                    .map(str::trim)
            })
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let mut body = vec![0_u8; content_length];
        stream.read_exact(&mut body).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn policy(agent_id: AgentId) -> CapabilityPolicy {
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
        policy
    }

    #[test]
    fn automatic_activation_is_allowlisted_and_routes_wasm_invocation() {
        let root = std::env::temp_dir().join(format!("orynth-plugin-host-{}", PluginId::new()));
        fs::create_dir_all(&root).unwrap();
        let module_path = root.join("module.wasm");
        fs::write(
            &module_path,
            wat::parse_str(
                r#"
                (module
                    (memory (export "memory") 1)
                    (data (i32.const 8) "ok")
                    (func (export "orynth_run") (param i32 i32) (result i64)
                        (i64.const 34359738370)
                    )
                )
                "#,
            )
            .unwrap(),
        )
        .unwrap();
        fs::write(
            root.join("orynth-plugin.manifest"),
            wasm_manifest_text(&module_path),
        )
        .unwrap();

        let mut host = PluginHost::new();
        let activated = host
            .activate_directories(
                std::slice::from_ref(&root),
                &DiscoveryPolicy::default(),
                &ActivationPolicy::default(),
            )
            .unwrap();
        assert_eq!(activated, 1);
        assert_eq!(host.len(), 1);
        let agent_id = AgentId::from_u64(92);
        let response = host
            .invoke(
                PluginId::from_u64(91),
                &policy(agent_id),
                agent_id,
                None,
                1,
                PluginRequest {
                    request_id: 1,
                    method: "run".to_owned(),
                    payload: vec![1],
                },
            )
            .unwrap();
        assert_eq!(response.payload, b"ok");
        assert_eq!(response.origin, TrustOrigin::External);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn activation_policy_can_exclude_candidates_without_granting_access() {
        let root =
            std::env::temp_dir().join(format!("orynth-plugin-host-filter-{}", PluginId::new()));
        fs::create_dir_all(&root).unwrap();
        let module = root.join("module.wasm");
        fs::write(&module, b"not wasm").unwrap();
        fs::write(
            root.join("orynth-plugin.manifest"),
            wasm_manifest_text(&module),
        )
        .unwrap();
        let mut host = PluginHost::new();
        let policy = ActivationPolicy {
            allowed_ids: Some(vec![PluginId::from_u64(999)]),
            ..ActivationPolicy::default()
        };
        assert_eq!(
            host.activate_directories(
                std::slice::from_ref(&root),
                &DiscoveryPolicy::default(),
                &policy
            )
            .unwrap(),
            0
        );
        assert!(host.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn activation_is_atomic_when_a_selected_kind_is_unsupported() {
        let root =
            std::env::temp_dir().join(format!("orynth-plugin-host-atomic-{}", PluginId::new()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("orynth-plugin.manifest"), mcp_manifest_text()).unwrap();
        let mut host = PluginHost::new();
        let policy = ActivationPolicy {
            allowed_kinds: Some(vec![PluginKind::Mcp]),
            ..ActivationPolicy::default()
        };
        assert!(matches!(
            host.activate_directories(
                std::slice::from_ref(&root),
                &DiscoveryPolicy::default(),
                &policy
            ),
            Err(PluginHostError::Invalid(_))
        ));
        assert!(host.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn configured_mcp_activation_binds_discovered_http_candidate() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
        let server_thread = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            assert_eq!(request["method"], "tools/call");
            assert_eq!(request["params"]["name"], "echo");
            let body = br#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        });

        let root = std::env::temp_dir().join(format!("orynth-plugin-host-mcp-{}", PluginId::new()));
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("orynth-plugin.manifest"),
            format!(
                "protocol_version=1\nid=93\nname=host.mcp.http\nversion=1\nkind=mcp\ncapability=network:{endpoint}\nmax_message_bytes=1024\nmax_memory_bytes=65536\nmax_fuel=10000\nmax_wall_time_ms=1000\n"
            ),
        )
        .unwrap();

        let agent_id = AgentId::from_u64(93);
        let mut capability_policy = CapabilityPolicy::new();
        capability_policy
            .grant(CapabilityLease {
                agent_id,
                task_id: None,
                domain: CapabilityDomain::Network,
                resource: endpoint.clone(),
                expires_at_ms: u128::MAX,
            })
            .unwrap();
        let mut mcp_specs = BTreeMap::new();
        mcp_specs.insert(
            PluginId::from_u64(93),
            McpActivationSpec {
                endpoint,
                mode: McpProtocolMode::Modern2026,
                client: McpClientInfo {
                    name: "orynth-host-test".to_owned(),
                    version: "1".to_owned(),
                },
                server: McpServerInfo {
                    name: "host.mcp.http".to_owned(),
                    version: "1".to_owned(),
                    protocol_version: 1,
                    metadata: BTreeMap::new(),
                },
                timeout: Duration::from_secs(2),
            },
        );
        let activation_policy = ActivationPolicy {
            allowed_ids: Some(vec![PluginId::from_u64(93)]),
            allowed_kinds: Some(vec![PluginKind::Mcp]),
            ..ActivationPolicy::default()
        };
        let mut host = PluginHost::new();
        assert_eq!(
            host.activate_directories_with_mcp(
                std::slice::from_ref(&root),
                &DiscoveryPolicy::default(),
                &activation_policy,
                &mcp_specs,
                McpActivationContext {
                    policy: &capability_policy,
                    agent_id,
                    task_id: None,
                    now_ms: 1,
                },
            )
            .unwrap(),
            1
        );
        let response = host
            .invoke(
                PluginId::from_u64(93),
                &capability_policy,
                agent_id,
                None,
                1,
                PluginRequest {
                    request_id: 1,
                    method: "tools/call".to_owned(),
                    payload: br#"{"name":"echo","arguments":{}}"#.to_vec(),
                },
            )
            .unwrap();
        assert_eq!(response.origin, TrustOrigin::McpResult);
        let result: serde_json::Value = serde_json::from_slice(&response.payload).unwrap();
        assert_eq!(result["ok"], true);
        server_thread.join().unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
