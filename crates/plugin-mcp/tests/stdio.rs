use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use orynth_kernel::{AgentId, PluginId, TrustOrigin};
use orynth_plugin_api::{
    PLUGIN_PROTOCOL_VERSION, PluginCapability, PluginKind, PluginManifest, PluginRequest,
    PluginResourceLimits, PluginTransport,
};
use orynth_plugin_mcp::{
    McpAdapter, McpClientInfo, McpConnectionContext, McpProtocolMode, McpServerInfo,
    SessionMcpInvoker, StdioMcpTransport,
};
use orynth_plugin_process::ProcessCommand;
use orynth_security::{AllowAllOwnership, CapabilityDomain, CapabilityLease, CapabilityPolicy};

fn fixture_path() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_orynth-mcp-fixture")
        .or_else(|| std::env::var_os("CARGO_BIN_EXE_orynth_mcp_fixture"))
        .map(PathBuf::from)
        .expect("cargo must provide the MCP fixture binary")
}

fn manifest(program: &Path) -> PluginManifest {
    PluginManifest {
        id: PluginId::from_u64(101),
        protocol_version: PLUGIN_PROTOCOL_VERSION,
        name: "mcp.fixture".to_owned(),
        version: "1".to_owned(),
        kind: PluginKind::Mcp,
        capabilities: vec![PluginCapability {
            domain: CapabilityDomain::Process,
            resource: program.to_string_lossy().into_owned(),
        }],
        limits: PluginResourceLimits::default(),
    }
}

fn policy(agent_id: AgentId, program: &Path) -> CapabilityPolicy {
    let mut policy = CapabilityPolicy::new();
    policy
        .grant(CapabilityLease {
            agent_id,
            task_id: None,
            domain: CapabilityDomain::Process,
            resource: program.to_string_lossy().into_owned(),
            expires_at_ms: u128::MAX,
        })
        .unwrap();
    policy
}

fn server() -> McpServerInfo {
    McpServerInfo {
        name: "fixture".to_owned(),
        version: "1".to_owned(),
        protocol_version: 1,
        wire_protocol_version: Some(orynth_plugin_mcp::MCP_LEGACY_PROTOCOL_VERSION.to_owned()),
        metadata: Default::default(),
    }
}

fn client() -> McpClientInfo {
    McpClientInfo {
        name: "orynth-test".to_owned(),
        version: "1".to_owned(),
    }
}

fn request(id: u64) -> PluginRequest {
    PluginRequest {
        request_id: id,
        method: "tools/call".to_owned(),
        payload: br#"{"name":"echo","arguments":{"value":7}}"#.to_vec(),
    }
}

#[test]
fn legacy_stdio_session_round_trips_through_mcp_adapter() {
    let program = fixture_path();
    let manifest = manifest(&program);
    let agent_id = AgentId::from_u64(102);
    let transport = StdioMcpTransport::new(
        ProcessCommand::new(&program).unwrap(),
        manifest.clone(),
        Duration::from_secs(2),
    )
    .unwrap();
    let session = transport
        .connect(
            McpProtocolMode::Legacy2025,
            client(),
            server(),
            McpConnectionContext {
                policy: &policy(agent_id, &program),
                ownership: &AllowAllOwnership,
                agent_id,
                task_id: None,
                now_ms: 1,
            },
        )
        .unwrap();
    let mut adapter =
        McpAdapter::new(manifest.clone(), server(), SessionMcpInvoker::new(session)).unwrap();
    let response = adapter
        .invoke(
            &policy(agent_id, &program),
            &AllowAllOwnership,
            agent_id,
            None,
            1,
            &manifest,
            request(1),
        )
        .unwrap();
    assert_eq!(response.origin, TrustOrigin::McpResult);
    let result: serde_json::Value = serde_json::from_slice(&response.payload).unwrap();
    assert_eq!(result["echo"]["arguments"]["value"], 7);
}

#[test]
fn modern_stdio_session_adds_per_request_metadata_without_legacy_handshake() {
    let program = fixture_path();
    let manifest = manifest(&program);
    let agent_id = AgentId::from_u64(103);
    let transport = StdioMcpTransport::new(
        ProcessCommand::new(&program).unwrap(),
        manifest.clone(),
        Duration::from_secs(2),
    )
    .unwrap();
    let session = transport
        .connect(
            McpProtocolMode::Modern2026,
            client(),
            server(),
            McpConnectionContext {
                policy: &policy(agent_id, &program),
                ownership: &AllowAllOwnership,
                agent_id,
                task_id: None,
                now_ms: 1,
            },
        )
        .unwrap();
    let mut adapter =
        McpAdapter::new(manifest.clone(), server(), SessionMcpInvoker::new(session)).unwrap();
    let response = adapter
        .invoke(
            &policy(agent_id, &program),
            &AllowAllOwnership,
            agent_id,
            None,
            1,
            &manifest,
            request(2),
        )
        .unwrap();
    let result: serde_json::Value = serde_json::from_slice(&response.payload).unwrap();
    assert_eq!(
        result["echo"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
        "2026-07-28"
    );
}
