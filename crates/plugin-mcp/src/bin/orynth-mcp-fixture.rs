use std::io::{self, BufRead, Write};

use serde_json::{Value, json};

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::BufWriter::new(io::stdout());
    for line in stdin.lock().lines() {
        let Ok(line) = line else { return };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            return;
        };
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return;
        };
        if method == "notifications/initialized" {
            continue;
        }
        let Some(request_id) = message.get("id") else {
            return;
        };
        let result = if method == "initialize" {
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fixture", "version": "1"}
            })
        } else {
            json!({"echo": message.get("params").cloned().unwrap_or(Value::Null)})
        };
        let response = json!({"jsonrpc": "2.0", "id": request_id, "result": result});
        let Ok(bytes) = serde_json::to_vec(&response) else {
            return;
        };
        if stdout.write_all(&bytes).is_err() || stdout.write_all(b"\n").is_err() {
            return;
        }
        if stdout.flush().is_err() {
            return;
        }
    }
}
