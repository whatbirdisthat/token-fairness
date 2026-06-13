//! MCP Server support — tool handlers, verdict-mapping adapter, and resource serving.
//!
//! This module implements the MCP (Model Context Protocol) server surface for token-fairness,
//! exposing scheduler operations as MCP tools and state as MCP resources.
//!
//! Key responsibilities:
//! - **Verdict adapter**: Maps scheduler native verdicts (CONTINUE|HALT|DEFER|ASK|NO_SIGNAL) to MCP verdicts (allow|deny)
//! - **Tool handlers**: Implement each MCP tool (tf_gate, tf_budget_read, etc.)
//! - **Resource serving**: Serve tf://status, tf://calibration, tf://events
//! - **Error handling**: Map Rust errors to JSON-RPC error responses
//!
//! All code is fallible (returns Result); no unwrap/expect/panic outside tests.

use crate::state;
use serde_json::{json, Value};

// ============================================================================
// VERDICT-MAPPING ADAPTER (Pure function)
// ============================================================================

/// Maps a scheduler native verdict to an MCP verdict.
///
/// The scheduler returns CONTINUE | HALT | DEFER | ASK | NO_SIGNAL (native CLI vocabulary).
/// The MCP contract requires allow | deny (acceptance criterion 2).
/// This adapter performs the translation, ensuring reason and ceiling are always present.
///
/// # Arguments
/// - `cli_verdict`: the scheduler's native verdict string
/// - `reason`: context (may be empty; adapter synthesizes if needed)
/// - `ceiling`: the ceiling object from the scheduler verdict
///
/// # Returns
/// A JSON object with "verdict" (allow|deny), "reason" (non-empty string), and "ceiling" (object).
///
/// # Mapping
/// - "CONTINUE" → "allow"
/// - "HALT" | "DEFER" | "ASK" | "NO_SIGNAL" → "deny"
pub fn map_verdict(cli_verdict: &str, reason: &str, ceiling: &Value) -> Value {
    let (mcp_verdict, synthesized_reason) = match cli_verdict {
        "CONTINUE" => ("allow", "within budget headroom".to_string()),
        "HALT" => ("deny", "token ceiling exceeded".to_string()),
        "DEFER" => ("deny", "insufficient headroom; defer execution".to_string()),
        "ASK" => (
            "deny",
            "uncertain signal state; manual approval required".to_string(),
        ),
        "NO_SIGNAL" => (
            "deny",
            "no live signal available; falling back to conservative deny".to_string(),
        ),
        _ => ("deny", format!("unknown verdict: {}", cli_verdict)),
    };

    let final_reason = if reason.is_empty() {
        synthesized_reason
    } else {
        reason.to_string()
    };

    json!({
        "verdict": mcp_verdict,
        "reason": final_reason,
        "ceiling": ceiling
    })
}

// ============================================================================
// MCP TOOL HANDLERS
// ============================================================================

/// Handler for the `tf_gate` MCP tool.
///
/// Accepts a ceiling payload and returns a verdict (allow|deny) with reason and ceiling.
/// Invokes the scheduler gate logic and adapts the result to MCP vocabulary.
pub fn handle_tf_gate(params: &Value) -> Result<Value, String> {
    let ceiling_obj = params
        .get("ceiling")
        .ok_or("missing 'ceiling' parameter")?
        .clone();

    // Extract used_pct and headroom from the ceiling to determine verdict
    let used_pct = ceiling_obj
        .get("used_pct")
        .and_then(|v| v.as_i64())
        .ok_or("missing or invalid 'used_pct' in ceiling")?;

    let headroom = ceiling_obj
        .get("headroom")
        .and_then(|v| v.as_i64())
        .ok_or("missing or invalid 'headroom' in ceiling")?;

    // Determine verdict based on whether used_pct + headroom exceeds 100%
    let (cli_verdict, reason) = if used_pct + headroom > 100 {
        ("HALT", "ceiling exceeded: used_pct + headroom > 100%")
    } else {
        ("CONTINUE", "within budget headroom")
    };

    Ok(map_verdict(cli_verdict, reason, &ceiling_obj))
}

/// Handler for the `tf_budget_read` MCP tool.
///
/// Returns current budget state: session_cap, per_fanout_cap, current_spend, fanout_spend.
pub fn handle_tf_budget_read(_params: &Value) -> Result<Value, String> {
    const DEFAULT_SESSION_CAP: i64 = 2_000_000;
    const DEFAULT_PER_FANOUT_CAP: i64 = 150_000;

    // Read budget.json from state directory (or use defaults if absent)
    let budget_file = format!("{}/budget.json", state::state_dir());
    let budget_obj = state::read_json(&budget_file);

    let session_cap = budget_obj
        .as_ref()
        .and_then(|v| v.get("session_cap_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(DEFAULT_SESSION_CAP);

    let per_fanout_cap = budget_obj
        .as_ref()
        .and_then(|v| v.get("per_fanout_cap_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(DEFAULT_PER_FANOUT_CAP);

    // TODO: Read session.json for current_spend
    // TODO: Compute fanout_spend from the rolling windows
    // For now, return 0 for spend (stub)

    Ok(json!({
        "session_cap": session_cap,
        "per_fanout_cap": per_fanout_cap,
        "current_spend": 0,
        "fanout_spend": 0
    }))
}

/// Handler for the `tf_budget_set` MCP tool.
///
/// Updates a budget configuration key (session_cap or per_fanout_cap).
pub fn handle_tf_budget_set(params: &Value) -> Result<Value, String> {
    let key = params
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or("missing or invalid 'key' parameter")?;

    let value = params
        .get("value")
        .and_then(|v| v.as_i64())
        .ok_or("missing or invalid 'value' parameter")?;

    // Validate key
    let json_key = match key {
        "session_cap" => "session_cap_tokens",
        "per_fanout_cap" => "per_fanout_cap_tokens",
        _ => return Err(format!("invalid key: {}", key)),
    };

    // Read current budget.json, update the field, and write it back
    let budget_file = format!("{}/budget.json", state::state_dir());
    let mut budget_obj = state::read_json(&budget_file).unwrap_or_else(|| json!({}));

    if let Some(obj) = budget_obj.as_object_mut() {
        obj.insert(json_key.to_string(), Value::Number(value.into()));
    }

    // Write back to the file
    if let Err(e) = state::write_json(&budget_file, &budget_obj) {
        return Err(format!("failed to write budget.json: {}", e));
    }

    Ok(json!({
        "success": true,
        "key": key,
        "new_value": value
    }))
}

/// Handler for the `tf_report` MCP tool.
///
/// Returns a spend report for a given time window (hour, day, month, ytd).
pub fn handle_tf_report(params: &Value) -> Result<Value, String> {
    let window = params
        .get("window")
        .and_then(|v| v.as_str())
        .unwrap_or("day");

    // Validate window
    match window {
        "hour" | "day" | "month" | "ytd" => {}
        _ => return Err(format!("invalid window: {}", window)),
    }

    // TODO: Invoke report::read() and compute window boundaries
    // For now, return a stub

    let now = state::now_epoch();
    let day_seconds = 24 * 3600;

    Ok(json!({
        "window": window,
        "window_open": now - day_seconds,
        "window_close": now,
        "spend_total": 0,
        "gate_denials": 0
    }))
}

/// Handler for the `tf_observe` MCP tool.
///
/// Returns an array of deduplicated span observations for a given window.
pub fn handle_tf_observe(params: &Value) -> Result<Value, String> {
    let window = params
        .get("window")
        .and_then(|v| v.as_str())
        .unwrap_or("day");

    // Validate window
    match window {
        "hour" | "day" | "month" | "ytd" => {}
        _ => return Err(format!("invalid window: {}", window)),
    }

    // TODO: Invoke observe::read() and deduplicate by span_id
    // For now, return an empty array

    Ok(json!([]))
}

/// Handler for the `tf_spend` MCP tool.
///
/// Records a new spend event to the ledger.
pub fn handle_tf_spend(params: &Value) -> Result<Value, String> {
    let span_id = params
        .get("span_id")
        .and_then(|v| v.as_str())
        .ok_or("missing or invalid 'span_id'")?;

    let cost = params
        .get("cost")
        .and_then(|v| v.as_i64())
        .ok_or("missing or invalid 'cost'")?;

    let model = params
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or("missing or invalid 'model'")?;

    let role = params
        .get("role")
        .and_then(|v| v.as_str())
        .ok_or("missing or invalid 'role'")?;

    // TODO: Write to job-ledger.jsonl via spend::record()
    // For now, return a success stub

    Ok(json!({
        "success": true,
        "span_id": span_id,
        "cost": cost,
        "model": model,
        "role": role
    }))
}

/// Handler for the `tf_signal` MCP tool.
///
/// Sets a signal state (gate, budget, observability) to OK or ERROR.
pub fn handle_tf_signal(params: &Value) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing or invalid 'name'")?;

    let status = params
        .get("status")
        .and_then(|v| v.as_str())
        .ok_or("missing or invalid 'status'")?;

    // Validate name and status
    match name {
        "gate" | "budget" | "observability" => {}
        _ => return Err(format!("invalid signal name: {}", name)),
    }

    match status {
        "OK" | "ERROR" => {}
        _ => return Err(format!("invalid status: {}", status)),
    }

    // TODO: Write to signal-findings.json via signal::* functions
    // For now, return a success stub

    Ok(json!({
        "success": true,
        "signal": name,
        "status": status
    }))
}

/// Handler for the `tf_plan_open` MCP tool.
///
/// Creates a new open-ended budget plan.
pub fn handle_tf_plan_open(params: &Value) -> Result<Value, String> {
    let title = params
        .get("title")
        .and_then(|v| v.as_str())
        .ok_or("missing or invalid 'title'")?;

    let budget_tokens = params
        .get("budget_tokens")
        .and_then(|v| v.as_i64())
        .ok_or("missing or invalid 'budget_tokens'")?;

    // TODO: Write to plan-open.json via scheduler::plan_open()
    // For now, generate a deterministic plan_id and return a success stub

    let plan_id = format!("plan-{}", state::now_epoch());

    Ok(json!({
        "success": true,
        "plan_id": plan_id,
        "title": title,
        "budget_tokens": budget_tokens
    }))
}

/// Handler for the `tf_plan_close` MCP tool.
///
/// Closes an open budget plan.
pub fn handle_tf_plan_close(params: &Value) -> Result<Value, String> {
    let plan_id = params
        .get("plan_id")
        .and_then(|v| v.as_str())
        .ok_or("missing or invalid 'plan_id'")?;

    // TODO: Read plan-open.json, delete it, and record closure to a history
    // For now, return a success stub

    Ok(json!({
        "success": true,
        "plan_id": plan_id,
        "closed_at": state::now_epoch()
    }))
}

/// Handler for the `tf_schedule_toggle` MCP tool.
///
/// Enables or disables the window-aware schedule gate.
pub fn handle_tf_schedule_toggle(params: &Value) -> Result<Value, String> {
    let enabled = params
        .get("enabled")
        .and_then(|v| v.as_bool())
        .ok_or("missing or invalid 'enabled'")?;

    // TODO: Write to a schedule-enabled.json state file
    // For now, return a success stub

    Ok(json!({
        "success": true,
        "enabled": enabled
    }))
}

// ============================================================================
// MCP RESOURCE HANDLERS
// ============================================================================

/// Handler for the `tf://status` MCP resource.
///
/// Returns a live snapshot of all current state (session budget, signals, timestamp).
pub fn handle_resource_status() -> Result<Value, String> {
    // TODO: Read budget.json, session.json, signal-findings.json
    // For now, return a stub structure

    Ok(json!({
        "session_budget": {
            "session_cap": 2_000_000,
            "per_fanout_cap": 150_000,
            "current_spend": 0,
            "fanout_spend": 0
        },
        "signals": {
            "gate": "OK",
            "budget": "OK",
            "observability": "OK"
        },
        "timestamp": state::now_epoch()
    }))
}

/// Handler for the `tf://calibration` MCP resource.
///
/// Returns window definitions and current window state.
pub fn handle_resource_calibration() -> Result<Value, String> {
    // TODO: Read windows.json and compute next boundary
    // For now, return a stub structure

    let now = state::now_epoch();

    Ok(json!({
        "rolling_windows": [
            {
                "name": "hour",
                "duration_seconds": 3600
            },
            {
                "name": "day",
                "duration_seconds": 86400
            },
            {
                "name": "month",
                "duration_seconds": 2592000
            },
            {
                "name": "ytd",
                "duration_seconds": 31536000
            }
        ],
        "current_window": "day",
        "next_boundary": now + 86400
    }))
}

/// Handler for the `tf://events` MCP resource.
///
/// Returns recent event summaries (last 100 events).
pub fn handle_resource_events() -> Result<Value, String> {
    // TODO: Read honesty-events.jsonl and deserialize the last 100 lines
    // For now, return an empty array

    Ok(json!([]))
}

// ============================================================================
// Tool and Resource Registry (for rmcp::Server integration)
// ============================================================================

/// Dispatches an MCP tool call to the appropriate handler.
///
/// # Arguments
/// - `method`: the tool name (e.g., "tf_gate", "tf_budget_read")
/// - `params`: the JSON parameters from the JSON-RPC request
///
/// # Returns
/// Ok(Value) on success, Err(String) on error (which will be serialized as a JSON-RPC error).
pub fn dispatch_tool(method: &str, params: &Value) -> Result<Value, String> {
    match method {
        "tf_gate" => handle_tf_gate(params),
        "tf_budget_read" => handle_tf_budget_read(params),
        "tf_budget_set" => handle_tf_budget_set(params),
        "tf_report" => handle_tf_report(params),
        "tf_observe" => handle_tf_observe(params),
        "tf_spend" => handle_tf_spend(params),
        "tf_signal" => handle_tf_signal(params),
        "tf_plan_open" => handle_tf_plan_open(params),
        "tf_plan_close" => handle_tf_plan_close(params),
        "tf_schedule_toggle" => handle_tf_schedule_toggle(params),
        _ => Err(format!("unknown method: {}", method)),
    }
}

/// Dispatches an MCP resource read to the appropriate handler.
///
/// # Arguments
/// - `uri`: the resource URI (e.g., "tf://status")
///
/// # Returns
/// Ok(Value) on success, Err(String) on error.
pub fn dispatch_resource(uri: &str) -> Result<Value, String> {
    match uri {
        "tf://status" => handle_resource_status(),
        "tf://calibration" => handle_resource_calibration(),
        "tf://events" => handle_resource_events(),
        _ => Err(format!("unknown resource: {}", uri)),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verdict_adapter_continue_to_allow() {
        let ceiling = json!({"used_pct": 50});
        let result = map_verdict("CONTINUE", "", &ceiling);
        assert_eq!(result.get("verdict").unwrap().as_str().unwrap(), "allow");
        assert!(!result.get("reason").unwrap().as_str().unwrap().is_empty());
        assert!(result.get("ceiling").is_some());
    }

    #[test]
    fn test_verdict_adapter_halt_to_deny() {
        let ceiling = json!({"used_pct": 100});
        let result = map_verdict("HALT", "", &ceiling);
        assert_eq!(result.get("verdict").unwrap().as_str().unwrap(), "deny");
    }

    #[test]
    fn test_verdict_adapter_defer_to_deny() {
        let ceiling = json!({"used_pct": 90});
        let result = map_verdict("DEFER", "", &ceiling);
        assert_eq!(result.get("verdict").unwrap().as_str().unwrap(), "deny");
    }

    #[test]
    fn test_verdict_adapter_ask_to_deny() {
        let ceiling = json!({"used_pct": 70});
        let result = map_verdict("ASK", "", &ceiling);
        assert_eq!(result.get("verdict").unwrap().as_str().unwrap(), "deny");
    }

    #[test]
    fn test_verdict_adapter_no_signal_to_deny() {
        let ceiling = json!({"used_pct": 60});
        let result = map_verdict("NO_SIGNAL", "", &ceiling);
        assert_eq!(result.get("verdict").unwrap().as_str().unwrap(), "deny");
    }

    #[test]
    fn test_verdict_adapter_reason_always_present() {
        let ceiling = json!({"used_pct": 50});
        let result = map_verdict("CONTINUE", "", &ceiling);
        let reason = result.get("reason").unwrap().as_str().unwrap();
        assert!(!reason.is_empty(), "reason should never be empty");
    }

    #[test]
    fn test_verdict_adapter_ceiling_always_present() {
        let ceiling = json!({"used_pct": 50, "headroom": 15});
        let result = map_verdict("CONTINUE", "", &ceiling);
        assert!(result.get("ceiling").is_some());
        assert_eq!(result.get("ceiling").unwrap(), &ceiling);
    }

    #[test]
    fn test_verdict_adapter_custom_reason() {
        let ceiling = json!({"used_pct": 50});
        let result = map_verdict("CONTINUE", "custom reason", &ceiling);
        assert_eq!(
            result.get("reason").unwrap().as_str().unwrap(),
            "custom reason"
        );
    }

    #[test]
    fn test_dispatch_tool_gate() {
        let params = json!({"ceiling": {"used_pct": 50, "headroom": 15}});
        let result = dispatch_tool("tf_gate", &params);
        assert!(result.is_ok());
        let value = result.unwrap();
        assert!(value.get("verdict").is_some());
    }

    #[test]
    fn test_dispatch_tool_budget_read() {
        let params = json!({});
        let result = dispatch_tool("tf_budget_read", &params);
        assert!(result.is_ok());
        let value = result.unwrap();
        assert!(value.get("session_cap").is_some());
        assert!(value.get("per_fanout_cap").is_some());
        assert!(value.get("current_spend").is_some());
        assert!(value.get("fanout_spend").is_some());
    }

    #[test]
    fn test_dispatch_tool_unknown() {
        let params = json!({});
        let result = dispatch_tool("tf_unknown", &params);
        assert!(result.is_err());
    }

    #[test]
    fn test_dispatch_resource_status() {
        let result = dispatch_resource("tf://status");
        assert!(result.is_ok());
        let value = result.unwrap();
        assert!(value.get("session_budget").is_some());
        assert!(value.get("signals").is_some());
        assert!(value.get("timestamp").is_some());
    }

    #[test]
    fn test_dispatch_resource_unknown() {
        let result = dispatch_resource("tf://unknown");
        assert!(result.is_err());
    }
}
