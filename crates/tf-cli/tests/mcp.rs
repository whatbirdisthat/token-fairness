//! MCP server integration tests.
//!
//! These tests drive the real `tf mcp` subprocess over stdio JSON-RPC and assert BEHAVIOUR:
//! each test seeds a known state file (via the `I2P_*` env overrides the child inherits) and
//! asserts the response EQUALS the seeded/derived value — not merely that a field is present.
//! That density is what closes the stub-facade hole (M1).
//!
//! **Note:** Gated behind the `mcp` feature. Run with: `cargo test --test mcp --features mcp`.

#![cfg(feature = "mcp")]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

/// A fresh temp dir for one test's state files.
fn temp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let p = std::env::temp_dir().join(format!(
        "tf-mcp-it-{}-{}-{}",
        tag,
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// A spawned `tf mcp` subprocess that is always killed + reaped on drop (no zombies).
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Spawn `tf mcp` with the given `I2P_*` env overrides so the child reads OUR seeded state.
fn spawn_mcp_server_env(env: &[(&str, &Path)]) -> Server {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tf"));
    cmd.arg("mcp");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn `tf mcp`");
    Server(child)
}

fn spawn_mcp_server() -> Server {
    spawn_mcp_server_env(&[])
}

/// Send one JSON-RPC request, return `(full_response, result_or_error)`.
fn send_tool_call(
    server: &mut Server,
    method: &str,
    params: Option<Value>,
) -> (Value, Option<Value>) {
    let server = &mut server.0;
    let stdin = server.stdin.as_mut().expect("failed to get stdin");
    let stdout = server.stdout.as_mut().expect("failed to get stdout");

    let request = if let Some(p) = params {
        json!({ "jsonrpc": "2.0", "method": method, "params": p, "id": 1 })
    } else {
        json!({ "jsonrpc": "2.0", "method": method, "id": 1 })
    };

    let request_str = request.to_string() + "\n";
    stdin
        .write_all(request_str.as_bytes())
        .expect("failed to write request");
    stdin.flush().expect("failed to flush stdin");

    let mut reader = BufReader::new(stdout);
    let mut response_line = String::new();
    reader
        .read_line(&mut response_line)
        .expect("failed to read response");

    let response: Value = serde_json::from_str(&response_line)
        .unwrap_or_else(|_| panic!("failed to parse response: {}", response_line));

    let result = response.get("result").cloned();
    let error = response.get("error").cloned();
    (response, result.or(error))
}

// ============================================================================
// MCP HANDSHAKE (H6)
// ============================================================================

#[test]
fn test_initialize_handshake() {
    let mut server = spawn_mcp_server();
    let (_resp, result) = send_tool_call(&mut server, "initialize", Some(json!({})));
    let result = result.expect("initialize must return a result");
    assert!(
        result.get("protocolVersion").is_some(),
        "initialize advertises a protocol version"
    );
    assert_eq!(
        result
            .pointer("/serverInfo/name")
            .unwrap()
            .as_str()
            .unwrap(),
        "token-fairness"
    );
    assert!(result.pointer("/capabilities/tools").is_some());
    assert!(result.pointer("/capabilities/resources").is_some());
}

#[test]
fn test_tools_list_enumerates_tools() {
    let mut server = spawn_mcp_server();
    let (_resp, result) = send_tool_call(&mut server, "tools/list", None);
    let result = result.expect("tools/list must return a result");
    let tools = result.get("tools").unwrap().as_array().unwrap();
    assert_eq!(tools.len(), 10, "all ten tools enumerated");
    let names: Vec<&str> = tools
        .iter()
        .map(|t| t.get("name").unwrap().as_str().unwrap())
        .collect();
    assert!(names.contains(&"tf_gate"));
    assert!(names.contains(&"tf_schedule_toggle"));
}

#[test]
fn test_resources_list_enumerates_resources() {
    let mut server = spawn_mcp_server();
    let (_resp, result) = send_tool_call(&mut server, "resources/list", None);
    let result = result.expect("resources/list must return a result");
    let resources = result.get("resources").unwrap().as_array().unwrap();
    assert_eq!(resources.len(), 3);
}

#[test]
fn test_tools_call_invokes_named_tool() {
    // Standard MCP form: tools/call with { name, arguments }.
    let mut server = spawn_mcp_server();
    let params = json!({
        "name": "tf_gate",
        "arguments": { "ceiling": { "used_pct": 90, "headroom": 15 } }
    });
    let (_resp, result) = send_tool_call(&mut server, "tools/call", Some(params));
    let result = result.expect("tools/call must return a result");
    assert_eq!(result.get("verdict").unwrap().as_str().unwrap(), "deny");
}

// ============================================================================
// TOOL BEHAVIOUR — verdicts must equal the real gate (C1)
// ============================================================================

#[test]
fn test_tf_gate_allow_below_ceiling() {
    let mut server = spawn_mcp_server();
    // used_pct 80, headroom 15 → ceiling 85 → below → allow.
    let params = json!({ "ceiling": { "used_pct": 80, "headroom": 15 } });
    let (_resp, result) = send_tool_call(&mut server, "tf_gate", Some(params));
    let result = result.expect("expected a result");
    assert_eq!(result.get("verdict").unwrap().as_str().unwrap(), "allow");
    assert!(result.get("reason").is_some());
    // The echoed ceiling carries the real used_pct the scheduler evaluated.
    assert_eq!(
        result
            .pointer("/ceiling/used_pct")
            .unwrap()
            .as_f64()
            .unwrap(),
        80.0
    );
}

#[test]
fn test_tf_gate_deny_at_ceiling() {
    let mut server = spawn_mcp_server();
    // used_pct 90, headroom 15 → ceiling 85 → breach → deny.
    let params = json!({ "ceiling": { "used_pct": 90, "headroom": 15 } });
    let (_resp, result) = send_tool_call(&mut server, "tf_gate", Some(params));
    let result = result.expect("expected a result");
    assert_eq!(result.get("verdict").unwrap().as_str().unwrap(), "deny");
}

// ============================================================================
// TOOL BEHAVIOUR — budget read folds REAL seeded state (H1, H7)
// ============================================================================

#[test]
fn test_tf_budget_read_reflects_seeded_caps_and_spend() {
    let dir = temp_dir("budget");
    let events = dir.join("events.jsonl");
    std::fs::write(
        &events,
        r#"{"ts":1000,"session":"S","kind":"spend","by_model":[{"model":"opus","tokens":120000,"cost_usd":1.0},{"model":"sonnet","tokens":30000,"cost_usd":0.2}]}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("budget.json"),
        r#"{"session_cap_tokens":900000,"per_fanout_cap_tokens":80000}"#,
    )
    .unwrap();

    let mut server = spawn_mcp_server_env(&[
        ("I2P_COST_STATE_DIR", dir.as_path()),
        ("I2P_HONESTY_EVENTS", events.as_path()),
    ]);
    let (_resp, result) = send_tool_call(&mut server, "tf_budget_read", None);
    let result = result.expect("expected a result");

    assert_eq!(result.get("session_cap").unwrap().as_i64().unwrap(), 900000);
    assert_eq!(
        result.get("per_fanout_cap").unwrap().as_i64().unwrap(),
        80000
    );
    assert_eq!(
        result.get("current_spend").unwrap().as_i64().unwrap(),
        150000
    );
    assert_eq!(
        result.get("fanout_spend").unwrap().as_i64().unwrap(),
        120000
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_tf_budget_set_then_read_round_trip() {
    let dir = temp_dir("budget-set");
    let mut server = spawn_mcp_server_env(&[("I2P_COST_STATE_DIR", dir.as_path())]);

    let (_resp, result) = send_tool_call(
        &mut server,
        "tf_budget_set",
        Some(json!({"key": "session_cap", "value": 60000})),
    );
    let result = result.expect("tf_budget_set should succeed");
    assert!(result.get("success").unwrap().as_bool().unwrap());

    let (_resp, result) = send_tool_call(&mut server, "tf_budget_read", None);
    let result = result.expect("tf_budget_read should succeed");
    assert_eq!(result.get("session_cap").unwrap().as_i64().unwrap(), 60000);

    std::fs::remove_dir_all(&dir).ok();
}

// ============================================================================
// TOOL BEHAVIOUR — report folds the window (H2)
// ============================================================================

#[test]
fn test_tf_report_folds_window_totals() {
    let dir = temp_dir("report");
    let events = dir.join("events.jsonl");
    std::fs::write(
        &events,
        concat!(
            "{\"ts\":100,\"session\":\"S\",\"kind\":\"spend\",\"by_model\":[{\"model\":\"opus\",\"tokens\":7000,\"cost_usd\":0.1}]}\n",
            "{\"ts\":100,\"kind\":\"gate\",\"class\":\"save\",\"reason\":\"ceiling\",\"est\":1}\n",
            "{\"ts\":100,\"kind\":\"gate\",\"class\":\"procedural-deny\",\"reason\":\"no budget\"}\n"
        ),
    )
    .unwrap();

    let mut server = spawn_mcp_server_env(&[("I2P_HONESTY_EVENTS", events.as_path())]);
    let (_resp, result) =
        send_tool_call(&mut server, "tf_report", Some(json!({"window": "month"})));
    let result = result.expect("expected a result");

    assert_eq!(result.get("window").unwrap().as_str().unwrap(), "month");
    let open = result.get("window_open").unwrap().as_i64().unwrap();
    let close = result.get("window_close").unwrap().as_i64().unwrap();
    assert_eq!(close - open, 30 * 24 * 3600, "month window spans 30d");
    assert_eq!(result.get("spend_total").unwrap().as_i64().unwrap(), 7000);
    assert_eq!(result.get("gate_denials").unwrap().as_i64().unwrap(), 2);

    std::fs::remove_dir_all(&dir).ok();
}

// ============================================================================
// TOOL BEHAVIOUR — observe returns deduped real spans (H3)
// ============================================================================

#[test]
fn test_tf_observe_returns_seeded_spans() {
    let dir = temp_dir("observe");
    let events = dir.join("events.jsonl");
    std::fs::write(
        &events,
        concat!(
            "{\"ts\":1000,\"session\":\"A\",\"kind\":\"spend\",\"by_model\":[{\"model\":\"opus\",\"tokens\":100,\"cost_usd\":1.0}]}\n",
            "{\"ts\":2000,\"session\":\"A\",\"kind\":\"spend\",\"by_model\":[{\"model\":\"opus\",\"tokens\":200,\"cost_usd\":2.0}]}\n"
        ),
    )
    .unwrap();

    let mut server = spawn_mcp_server_env(&[("I2P_HONESTY_EVENTS", events.as_path())]);
    let (_resp, result) = send_tool_call(&mut server, "tf_observe", Some(json!({"window": "day"})));
    let result = result.expect("expected a result");

    let arr = result.as_array().unwrap();
    assert_eq!(arr.len(), 1, "deduped to latest per session");
    assert_eq!(arr[0].get("span_id").unwrap().as_str().unwrap(), "A");
    assert_eq!(arr[0].get("cost_tokens").unwrap().as_i64().unwrap(), 200);
    assert_eq!(arr[0].get("model").unwrap().as_str().unwrap(), "opus");

    std::fs::remove_dir_all(&dir).ok();
}

// ============================================================================
// TOOL BEHAVIOUR — spend actually writes (C2)
// ============================================================================

#[test]
fn test_tf_spend_writes_event_that_folds_back() {
    let dir = temp_dir("spend");
    let events = dir.join("events.jsonl");

    let mut server = spawn_mcp_server_env(&[("I2P_HONESTY_EVENTS", events.as_path())]);
    let (_resp, result) = send_tool_call(
        &mut server,
        "tf_spend",
        Some(json!({"span_id": "span-1", "cost": 500, "model": "opus", "role": "researcher"})),
    );
    let result = result.expect("expected a result");
    assert!(result.get("success").unwrap().as_bool().unwrap());

    // Read the spend back through tf_observe — proves the write was real.
    let (_resp, observed) =
        send_tool_call(&mut server, "tf_observe", Some(json!({"window": "day"})));
    let observed = observed.expect("observe ok");
    let arr = observed.as_array().unwrap();
    assert_eq!(arr.len(), 1, "the spend we just recorded is observable");
    assert_eq!(arr[0].get("cost_tokens").unwrap().as_i64().unwrap(), 500);

    std::fs::remove_dir_all(&dir).ok();
}

// ============================================================================
// TOOL BEHAVIOUR — signal persists and status reflects it (H4, H5)
// ============================================================================

#[test]
fn test_tf_signal_persists_and_status_reflects() {
    let dir = temp_dir("signal");
    let signals = dir.join("signals.json");
    let events = dir.join("events.jsonl");

    let mut server = spawn_mcp_server_env(&[
        ("I2P_MCP_SIGNALS", signals.as_path()),
        ("I2P_COST_STATE_DIR", dir.as_path()),
        ("I2P_HONESTY_EVENTS", events.as_path()),
    ]);

    let (_resp, result) = send_tool_call(
        &mut server,
        "tf_signal",
        Some(json!({"name": "gate", "status": "ERROR"})),
    );
    assert!(result
        .expect("ok")
        .get("success")
        .unwrap()
        .as_bool()
        .unwrap());

    let (_resp, status) = send_tool_call(
        &mut server,
        "resources/read",
        Some(json!({"uri": "tf://status"})),
    );
    let status = status.expect("status ok");
    // The status resource reflects the REAL signal, not unconditional OK (H5).
    assert_eq!(
        status.pointer("/signals/gate").unwrap().as_str().unwrap(),
        "ERROR"
    );
    assert_eq!(
        status.pointer("/signals/budget").unwrap().as_str().unwrap(),
        "OK"
    );

    std::fs::remove_dir_all(&dir).ok();
}

// ============================================================================
// TOOL BEHAVIOUR — plan open/close round trip via the real scheduler (H4)
// ============================================================================

#[test]
fn test_tf_plan_open_close_round_trip_persists_plan_id() {
    let dir = temp_dir("plan");
    let popen = dir.join("plan-open.json");
    let session = dir.join("session.json");
    let cal = dir.join("cal.json");

    let mut server = spawn_mcp_server_env(&[
        ("I2P_PLANOPEN_FILE", popen.as_path()),
        ("I2P_SESSION_FILE", session.as_path()),
        ("I2P_CALIBRATION_FILE", cal.as_path()),
    ]);

    let (_resp, opened) = send_tool_call(
        &mut server,
        "tf_plan_open",
        Some(json!({"title": "Q3 research", "budget_tokens": 100000})),
    );
    let opened = opened.expect("plan_open ok");
    let plan_id = opened.get("plan_id").unwrap().as_str().unwrap().to_string();
    assert!(!plan_id.is_empty());
    // The scheduler really wrote plan-open.json with our budget as est.
    assert!(popen.exists());
    let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(&popen).unwrap()).unwrap();
    assert_eq!(on_disk.get("est").unwrap().as_i64().unwrap(), 100000);
    assert_eq!(on_disk.get("plan_id").unwrap().as_str().unwrap(), plan_id);

    let (_resp, closed) = send_tool_call(
        &mut server,
        "tf_plan_close",
        Some(json!({"plan_id": plan_id})),
    );
    let closed = closed.expect("plan_close ok");
    assert!(closed.get("success").unwrap().as_bool().unwrap());
    // plan_close removed the open-plan file.
    assert!(!popen.exists());

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_tf_plan_close_unknown_plan_errors() {
    let dir = temp_dir("plan-none");
    let popen = dir.join("nope.json");
    let mut server = spawn_mcp_server_env(&[("I2P_PLANOPEN_FILE", popen.as_path())]);
    let (response, _) = send_tool_call(
        &mut server,
        "tf_plan_close",
        Some(json!({"plan_id": "plan-x"})),
    );
    assert!(
        response.get("error").is_some(),
        "closing with no open plan errors"
    );
    std::fs::remove_dir_all(&dir).ok();
}

// ============================================================================
// TOOL BEHAVIOUR — schedule toggle persists (H4)
// ============================================================================

#[test]
fn test_tf_schedule_toggle_persists() {
    let dir = temp_dir("sched");
    let sf = dir.join("sched.json");
    let mut server = spawn_mcp_server_env(&[("I2P_SCHEDULE_ENABLED", sf.as_path())]);

    let (_resp, result) = send_tool_call(
        &mut server,
        "tf_schedule_toggle",
        Some(json!({"enabled": true})),
    );
    let result = result.expect("expected a result");
    assert!(result.get("enabled").unwrap().as_bool().unwrap());

    // The flag is on disk.
    let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(&sf).unwrap()).unwrap();
    assert!(on_disk.get("enabled").unwrap().as_bool().unwrap());

    std::fs::remove_dir_all(&dir).ok();
}

// ============================================================================
// RESOURCES — events reflects the real ledger (H5)
// ============================================================================

#[test]
fn test_resource_events_returns_seeded_ledger() {
    let dir = temp_dir("events");
    let events = dir.join("events.jsonl");
    std::fs::write(
        &events,
        concat!(
            "{\"ts\":1,\"kind\":\"blown\",\"reason\":\"a\"}\n",
            "{\"ts\":2,\"kind\":\"blown\",\"reason\":\"b\"}\n"
        ),
    )
    .unwrap();

    let mut server = spawn_mcp_server_env(&[("I2P_HONESTY_EVENTS", events.as_path())]);
    let (_resp, result) = send_tool_call(
        &mut server,
        "resources/read",
        Some(json!({"uri": "tf://events"})),
    );
    let result = result.expect("expected a result");
    let arr = result.as_array().unwrap();
    assert_eq!(arr.len(), 2, "both seeded events returned");
    assert_eq!(arr[1].get("ts").unwrap().as_i64().unwrap(), 2);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_resource_tf_calibration_returns_windows() {
    let mut server = spawn_mcp_server();
    let (_resp, result) = send_tool_call(
        &mut server,
        "resources/read",
        Some(json!({"uri": "tf://calibration"})),
    );
    let result = result.expect("expected a result");
    assert_eq!(
        result
            .get("rolling_windows")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert!(result.get("next_boundary").is_some());
}

// ============================================================================
// ERROR HANDLING
// ============================================================================

#[test]
fn test_unknown_method_returns_error() {
    let mut server = spawn_mcp_server();
    let (response, _) = send_tool_call(&mut server, "tf_unknown_tool", Some(json!({})));
    let code = response
        .get("error")
        .unwrap()
        .get("code")
        .unwrap()
        .as_i64()
        .unwrap();
    assert_eq!(code, -32601, "method not found");
}

#[test]
fn test_invalid_json_returns_parse_error() {
    let mut server = spawn_mcp_server();
    let stdin = server.0.stdin.as_mut().unwrap();
    let stdout = server.0.stdout.as_mut().unwrap();
    stdin.write_all(b"{ this is not json }\n").unwrap();
    stdin.flush().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let response: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        response.pointer("/error/code").unwrap().as_i64().unwrap(),
        -32700
    );
}

#[test]
fn test_invalid_budget_set_key_errors() {
    let mut server = spawn_mcp_server();
    let (response, _) = send_tool_call(
        &mut server,
        "tf_budget_set",
        Some(json!({"key": "invalid_key", "value": 1000})),
    );
    assert!(response.get("error").is_some());
}

#[test]
fn test_tf_spend_missing_field_errors() {
    let mut server = spawn_mcp_server();
    // No 'cost' field — must surface a JSON-RPC error, not a fake success.
    let (response, _) = send_tool_call(
        &mut server,
        "tf_spend",
        Some(json!({"span_id": "x", "model": "opus", "role": "r"})),
    );
    assert!(response.get("error").is_some());
}

#[test]
fn test_stdio_eof_graceful_shutdown() {
    let mut server = spawn_mcp_server();
    drop(server.0.stdin.take());
    let status = server.0.wait().expect("failed to wait for process");
    assert!(status.success() || status.code().unwrap_or(-1) == 0);
}
