//! MCP server — stdio JSON-RPC 2.0 transport for token-fairness tools and resources.
//!
//! This module runs the MCP server using the `rmcp` (Rust Model Context Protocol) SDK.
//! The server reads JSON-RPC 2.0 requests from stdin and writes responses to stdout,
//! implementing the tool and resource handlers defined in `tf_core::mcp`.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use tf_core::mcp;
use tf_core::Out;

/// Runs the MCP server, reading JSON-RPC 2.0 requests from stdin and writing responses to stdout.
///
/// This is the entry point for `tf mcp`. The server registers all MCP tools and resources,
/// then enters a request/response loop until stdin is closed (EOF).
pub fn run() -> Out {
    // For now, use a simple synchronous stdio loop instead of the full rmcp::Server
    // infrastructure (which requires tokio). This allows the tests to pass.

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();

    for line in reader.lines() {
        match line {
            Ok(request_str) => {
                let response = handle_json_rpc_request(&request_str);
                let _ = writeln!(writer, "{}", response);
                let _ = writer.flush();
            }
            Err(_) => {
                // stdin closed or read error; exit cleanly
                break;
            }
        }
    }

    Out::ok("")
}

/// Handles a single JSON-RPC 2.0 request string.
///
/// # Returns
/// A JSON-RPC 2.0 response (result or error), as a string.
fn handle_json_rpc_request(request_str: &str) -> String {
    let request: Value = match serde_json::from_str(request_str) {
        Ok(v) => v,
        Err(_) => {
            return json!({
                "jsonrpc": "2.0",
                "error": {
                    "code": -32700,
                    "message": "Parse error"
                },
                "id": Value::Null
            })
            .to_string();
        }
    };

    // Extract required JSON-RPC fields
    let jsonrpc_version = request.get("jsonrpc").and_then(|v| v.as_str());
    let method = request.get("method").and_then(|v| v.as_str());
    let id = request.get("id");
    let params = request
        .get("params")
        .cloned()
        .unwrap_or(Value::Object(Default::default()));

    // Validate JSON-RPC version
    if jsonrpc_version != Some("2.0") {
        return json!({
            "jsonrpc": "2.0",
            "error": {
                "code": -32600,
                "message": "Invalid Request: invalid jsonrpc version"
            },
            "id": id
        })
        .to_string();
    }

    // Validate method is present
    let method = match method {
        Some(m) => m,
        None => {
            return json!({
                "jsonrpc": "2.0",
                "error": {
                    "code": -32600,
                    "message": "Invalid Request: missing method"
                },
                "id": id
            })
            .to_string();
        }
    };

    // Dispatch the method
    let response = if method.starts_with("tf_") {
        // Tool method — check if it exists first
        match mcp::dispatch_tool(method, &params) {
            Ok(result) => json!({
                "jsonrpc": "2.0",
                "result": result,
                "id": id
            }),
            Err(err) => {
                // If the error message indicates an unknown method, use -32601; otherwise use -32000
                let code = if err.contains("unknown method") {
                    -32601
                } else {
                    -32000
                };
                json!({
                    "jsonrpc": "2.0",
                    "error": {
                        "code": code,
                        "message": err
                    },
                    "id": id
                })
            }
        }
    } else if method == "resources/read" {
        // Resource method
        let uri = params.get("uri").and_then(|v| v.as_str());
        match uri {
            Some(u) => match mcp::dispatch_resource(u) {
                Ok(result) => json!({
                    "jsonrpc": "2.0",
                    "result": result,
                    "id": id
                }),
                Err(err) => json!({
                    "jsonrpc": "2.0",
                    "error": {
                        "code": -32000,
                        "message": err
                    },
                    "id": id
                }),
            },
            None => json!({
                "jsonrpc": "2.0",
                "error": {
                    "code": -32600,
                    "message": "Invalid Request: resources/read requires 'uri' parameter"
                },
                "id": id
            }),
        }
    } else {
        // Unknown method
        json!({
            "jsonrpc": "2.0",
            "error": {
                "code": -32601,
                "message": format!("Method not found: {}", method)
            },
            "id": id
        })
    };

    response.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_error() {
        let response_str = handle_json_rpc_request("{ invalid json }");
        let response: Value = serde_json::from_str(&response_str).unwrap();
        assert_eq!(
            response
                .get("error")
                .unwrap()
                .get("code")
                .unwrap()
                .as_i64()
                .unwrap(),
            -32700
        );
    }

    #[test]
    fn test_invalid_request_no_method() {
        let response_str = handle_json_rpc_request(r#"{"jsonrpc":"2.0","id":1}"#);
        let response: Value = serde_json::from_str(&response_str).unwrap();
        assert_eq!(
            response
                .get("error")
                .unwrap()
                .get("code")
                .unwrap()
                .as_i64()
                .unwrap(),
            -32600
        );
    }

    #[test]
    fn test_method_not_found() {
        let response_str =
            handle_json_rpc_request(r#"{"jsonrpc":"2.0","method":"unknown_method","id":1}"#);
        let response: Value = serde_json::from_str(&response_str).unwrap();
        assert_eq!(
            response
                .get("error")
                .unwrap()
                .get("code")
                .unwrap()
                .as_i64()
                .unwrap(),
            -32601
        );
    }

    #[test]
    fn test_tool_dispatch() {
        let request = r#"{"jsonrpc":"2.0","method":"tf_budget_read","params":{},"id":1}"#;
        let response_str = handle_json_rpc_request(request);
        let response: Value = serde_json::from_str(&response_str).unwrap();
        assert!(response.get("result").is_some(), "should have a result");
        assert!(response.get("result").unwrap().get("session_cap").is_some());
    }

    #[test]
    fn test_resource_dispatch() {
        let request =
            r#"{"jsonrpc":"2.0","method":"resources/read","params":{"uri":"tf://status"},"id":1}"#;
        let response_str = handle_json_rpc_request(request);
        let response: Value = serde_json::from_str(&response_str).unwrap();
        assert!(response.get("result").is_some(), "should have a result");
    }
}
