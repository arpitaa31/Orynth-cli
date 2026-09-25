use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    thread,
    time::Duration,
};

use orynth_kernel::{AgentId, PluginId, TrustOrigin};
use orynth_plugin_api::{
    PLUGIN_PROTOCOL_VERSION, PluginCapability, PluginError, PluginKind, PluginManifest,
    PluginRequest, PluginResourceLimits, PluginTransport,
};
use orynth_plugin_mcp::{
    HttpMcpTransport, McpAdapter, McpClientInfo, McpConnectionContext, McpProtocolMode,
    McpServerInfo, SessionMcpInvoker,
};
use orynth_security::{
    AllowAllOwnership, CapabilityDomain, CapabilityLease, CapabilityPolicy, OwnershipAccess,
    OwnershipError, ResourceOwnershipPolicy,
};

struct DenyOwnership;

impl ResourceOwnershipPolicy for DenyOwnership {
    fn authorize(
        &self,
        agent_id: AgentId,
        resource: &str,
        _access: OwnershipAccess,
    ) -> Result<(), OwnershipError> {
        Err(OwnershipError::Unowned {
            agent_id,
            resource: resource.to_owned(),
        })
    }
}

fn server() -> McpServerInfo {
    McpServerInfo {
        name: "http-fixture".to_owned(),
        version: "1".to_owned(),
        protocol_version: 1,
        wire_protocol_version: Some(orynth_plugin_mcp::MCP_MODERN_PROTOCOL_VERSION.to_owned()),
        metadata: Default::default(),
    }
}

fn client() -> McpClientInfo {
    McpClientInfo {
        name: "orynth-http-test".to_owned(),
        version: "1".to_owned(),
    }
}

fn manifest(endpoint: &str) -> PluginManifest {
    PluginManifest {
        id: PluginId::from_u64(201),
        protocol_version: PLUGIN_PROTOCOL_VERSION,
        name: "mcp.http.fixture".to_owned(),
        version: "1".to_owned(),
        kind: PluginKind::Mcp,
        capabilities: vec![PluginCapability {
            domain: CapabilityDomain::Network,
            resource: endpoint.to_owned(),
        }],
        limits: PluginResourceLimits::default(),
    }
}

fn policy(agent_id: AgentId, endpoint: &str) -> CapabilityPolicy {
    let mut policy = CapabilityPolicy::new();
    policy
        .grant(CapabilityLease {
            agent_id,
            task_id: None,
            domain: CapabilityDomain::Network,
            resource: endpoint.to_owned(),
            expires_at_ms: u128::MAX,
        })
        .unwrap();
    policy
}

#[test]
fn http_connection_rejects_missing_resource_ownership_before_network_io() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let manifest = manifest(&endpoint);
    let agent_id = AgentId::from_u64(299);
    let result = HttpMcpTransport::new(&endpoint, manifest.clone(), Duration::from_secs(2))
        .unwrap()
        .connect(
            McpProtocolMode::Modern2026,
            client(),
            server(),
            McpConnectionContext {
                policy: &policy(agent_id, &endpoint),
                ownership: &DenyOwnership,
                agent_id,
                task_id: None,
                now_ms: 1,
            },
        );
    assert!(matches!(
        result,
        Err(orynth_plugin_mcp::McpError::Plugin(
            PluginError::CapabilityDenied(_)
        ))
    ));
    drop(listener);
}

fn read_headers(stream: &mut TcpStream) -> String {
    let mut headers = Vec::new();
    let mut byte = [0_u8; 1];
    while !headers.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).unwrap();
        headers.push(byte[0]);
        assert!(
            headers.len() <= 16 * 1024,
            "fixture request headers are bounded"
        );
    }
    String::from_utf8(headers).unwrap()
}

fn read_request(stream: &mut TcpStream) -> serde_json::Value {
    let header_text = read_headers(stream);
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
    assert!(content_length <= 64 * 1024);
    let mut body = vec![0_u8; content_length];
    stream.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[test]
fn modern_http_session_round_trips_through_mcp_adapter() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let server_thread = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut stream);
        assert_eq!(request["jsonrpc"], "2.0");
        assert_eq!(request["id"], 1);
        assert_eq!(request["method"], "tools/call");
        assert_eq!(request["params"]["name"], "echo");
        assert_eq!(
            request["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
            "2026-07-28"
        );

        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"echo": request["params"]},
        });
        let body = serde_json::to_vec(&response).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(&body).unwrap();
    });

    let manifest = manifest(&endpoint);
    let agent_id = AgentId::from_u64(202);
    let transport =
        HttpMcpTransport::new(&endpoint, manifest.clone(), Duration::from_secs(2)).unwrap();
    let session = transport
        .connect(
            McpProtocolMode::Modern2026,
            client(),
            server(),
            McpConnectionContext {
                policy: &policy(agent_id, &endpoint),
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
            &policy(agent_id, &endpoint),
            &AllowAllOwnership,
            agent_id,
            None,
            1,
            &manifest,
            PluginRequest {
                request_id: 1,
                method: "tools/call".to_owned(),
                payload: br#"{"name":"echo","arguments":{"value":7}}"#.to_vec(),
            },
        )
        .unwrap();

    server_thread.join().unwrap();
    assert_eq!(response.origin, TrustOrigin::McpResult);
    let result: serde_json::Value = serde_json::from_slice(&response.payload).unwrap();
    assert_eq!(result["echo"]["arguments"]["value"], 7);
}

#[test]
fn modern_http_sse_handles_server_request_before_response() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let server_thread = thread::spawn(move || {
        {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            assert_eq!(request["method"], "tools/call");
            let server_request = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 77,
                "method": "sampling/createMessage",
                "params": {"prompt": "approve"},
            });
            let response = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"ok": true},
            });
            let body = format!(
                "data: {}\n\ndata: {}\n\n",
                serde_json::to_string(&server_request).unwrap(),
                serde_json::to_string(&response).unwrap(),
            );
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body.as_bytes()).unwrap();
        }

        let (mut stream, _) = listener.accept().unwrap();
        let response = read_request(&mut stream);
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 77);
        assert_eq!(response["result"]["accepted"], true);
        write!(
            stream,
            "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
    });

    let manifest = manifest(&endpoint);
    let agent_id = AgentId::from_u64(203);
    let transport = HttpMcpTransport::new(&endpoint, manifest.clone(), Duration::from_secs(2))
        .unwrap()
        .with_server_request_handler(|request_id, method, params| {
            assert_eq!(request_id, 77);
            assert_eq!(method, "sampling/createMessage");
            assert_eq!(params["prompt"], "approve");
            Ok(serde_json::json!({"accepted": true}))
        });
    let session = transport
        .connect(
            McpProtocolMode::Modern2026,
            client(),
            server(),
            McpConnectionContext {
                policy: &policy(agent_id, &endpoint),
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
            &policy(agent_id, &endpoint),
            &AllowAllOwnership,
            agent_id,
            None,
            1,
            &manifest,
            PluginRequest {
                request_id: 1,
                method: "tools/call".to_owned(),
                payload: br#"{"name":"echo","arguments":{}}"#.to_vec(),
            },
        )
        .unwrap();

    server_thread.join().unwrap();
    let result: serde_json::Value = serde_json::from_slice(&response.payload).unwrap();
    assert_eq!(result["ok"], true);
}

#[test]
fn modern_http_sse_reconnects_active_request_from_event_cursor() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let server_thread = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut stream);
        assert_eq!(request["method"], "tools/call");
        let first_body = "id: event-1\nretry: 0\n\n";
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            first_body.len()
        )
        .unwrap();
        stream.write_all(first_body.as_bytes()).unwrap();
        drop(stream);

        let (mut stream, _) = listener.accept().unwrap();
        let headers = read_headers(&mut stream);
        let headers_lower = headers.to_ascii_lowercase();
        assert!(headers.starts_with("GET /mcp HTTP/1.1"));
        assert!(headers_lower.contains("last-event-id: event-1"));
        let second_body = format!(
            "id: event-2\ndata: {}\n\n",
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"resumed": true},
            })
        );
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            second_body.len()
        )
        .unwrap();
        stream.write_all(second_body.as_bytes()).unwrap();
    });

    let manifest = manifest(&endpoint);
    let agent_id = AgentId::from_u64(205);
    let transport =
        HttpMcpTransport::new(&endpoint, manifest.clone(), Duration::from_secs(2)).unwrap();
    let session = transport
        .connect(
            McpProtocolMode::Modern2026,
            client(),
            server(),
            McpConnectionContext {
                policy: &policy(agent_id, &endpoint),
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
            &policy(agent_id, &endpoint),
            &AllowAllOwnership,
            agent_id,
            None,
            1,
            &manifest,
            PluginRequest {
                request_id: 1,
                method: "tools/call".to_owned(),
                payload: br#"{"name":"echo","arguments":{}}"#.to_vec(),
            },
        )
        .unwrap();

    server_thread.join().unwrap();
    let result: serde_json::Value = serde_json::from_slice(&response.payload).unwrap();
    assert_eq!(result["resumed"], true);
}

#[test]
fn http_get_sse_stream_handles_requests_and_resumption_cursor() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let server_thread = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let headers = read_headers(&mut stream);
        let headers_lower = headers.to_ascii_lowercase();
        assert!(headers.starts_with("GET /mcp HTTP/1.1"));
        assert!(headers_lower.contains("accept: text/event-stream"));
        assert!(headers_lower.contains("last-event-id: event-40"));
        assert!(headers_lower.contains("mcp-protocol-version: 2026-07-28"));

        let server_request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 88,
            "method": "sampling/createMessage",
            "params": {"prompt": "resume"},
        });
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/progress",
            "params": {"progress": 1},
        });
        let body = format!(
            "id: event-41\ndata: {}\n\nid: event-42\ndata: {}\n\n",
            serde_json::to_string(&server_request).unwrap(),
            serde_json::to_string(&notification).unwrap(),
        );
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(body.as_bytes()).unwrap();
        drop(stream);

        let (mut stream, _) = listener.accept().unwrap();
        let response = read_request(&mut stream);
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 88);
        assert_eq!(response["result"]["accepted"], true);
        write!(
            stream,
            "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
    });

    let manifest = manifest(&endpoint);
    let agent_id = AgentId::from_u64(204);
    let transport = HttpMcpTransport::new(&endpoint, manifest, Duration::from_secs(2))
        .unwrap()
        .with_server_request_handler(|request_id, method, params| {
            assert_eq!(request_id, 88);
            assert_eq!(method, "sampling/createMessage");
            assert_eq!(params["prompt"], "resume");
            Ok(serde_json::json!({"accepted": true}))
        });
    let mut session = transport
        .connect(
            McpProtocolMode::Modern2026,
            client(),
            server(),
            McpConnectionContext {
                policy: &policy(agent_id, &endpoint),
                ownership: &AllowAllOwnership,
                agent_id,
                task_id: None,
                now_ms: 1,
            },
        )
        .unwrap();
    let mut stream = session
        .transport_mut()
        .open_event_stream(McpProtocolMode::Modern2026, Some("event-40"), None)
        .unwrap();

    let first = stream.next_event().unwrap().unwrap();
    assert_eq!(first.id.as_deref(), Some("event-41"));
    assert_eq!(first.message["method"], "sampling/createMessage");
    let second = stream.next_event().unwrap().unwrap();
    assert_eq!(second.id.as_deref(), Some("event-42"));
    assert_eq!(second.message["method"], "notifications/progress");
    assert_eq!(stream.last_event_id(), Some("event-42"));
    assert!(stream.next_event().unwrap().is_none());

    server_thread.join().unwrap();
}

#[test]
fn http_get_sse_stream_reconnects_when_retry_is_advertised() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let server_thread = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let headers = read_headers(&mut stream);
        assert!(headers.starts_with("GET /mcp HTTP/1.1"));
        let first_body = "id: event-1\nretry: 0\n\n";
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            first_body.len()
        )
        .unwrap();
        stream.write_all(first_body.as_bytes()).unwrap();
        drop(stream);

        let (mut stream, _) = listener.accept().unwrap();
        let headers = read_headers(&mut stream);
        let headers_lower = headers.to_ascii_lowercase();
        assert!(headers.starts_with("GET /mcp HTTP/1.1"));
        assert!(headers_lower.contains("last-event-id: event-1"));
        let second_body = format!(
            "id: event-2\ndata: {}\n\n",
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/progress",
                "params": {"progress": 2},
            })
        );
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            second_body.len()
        )
        .unwrap();
        stream.write_all(second_body.as_bytes()).unwrap();
    });

    let manifest = manifest(&endpoint);
    let agent_id = AgentId::from_u64(206);
    let transport = HttpMcpTransport::new(&endpoint, manifest, Duration::from_secs(2)).unwrap();
    let mut session = transport
        .connect(
            McpProtocolMode::Modern2026,
            client(),
            server(),
            McpConnectionContext {
                policy: &policy(agent_id, &endpoint),
                ownership: &AllowAllOwnership,
                agent_id,
                task_id: None,
                now_ms: 1,
            },
        )
        .unwrap();
    let mut stream = session
        .transport_mut()
        .open_event_stream(McpProtocolMode::Modern2026, None, None)
        .unwrap();
    let event = stream.next_event().unwrap().unwrap();
    assert_eq!(event.id.as_deref(), Some("event-2"));
    assert_eq!(event.message["method"], "notifications/progress");
    assert_eq!(stream.last_event_id(), Some("event-2"));
    assert!(stream.next_event().unwrap().is_none());

    server_thread.join().unwrap();
}
