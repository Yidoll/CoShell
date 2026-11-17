use coshell_core::RuntimeService;
use serde_json::{json, Value};

pub fn handle_request(request: Value) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let result = match method {
        "initialize" => json!({
            "protocolVersion": "2025-06-18",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "coshell", "version": env!("CARGO_PKG_VERSION") }
        }),
        "tools/list" => json!({
            "tools": [{
                "name": "coshell_health",
                "description": "Report CoShell runtime health",
                "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
            }]
        }),
        "tools/call"
            if request.pointer("/params/name").and_then(Value::as_str)
                == Some("coshell_health") =>
        {
            json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string_pretty(&RuntimeService::new().health())
                        .expect("health serializes")
                }]
            })
        }
        _ => {
            return json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": "Method not found" }
            })
        }
    };
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

#[cfg(test)]
mod tests {
    use super::handle_request;
    use serde_json::json;

    #[test]
    fn lists_health_tool() {
        let response = handle_request(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }));
        assert_eq!(
            response
                .pointer("/result/tools/0/name")
                .and_then(|value| value.as_str()),
            Some("coshell_health")
        );
    }
}
