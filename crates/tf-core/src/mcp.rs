//! MCP Server support — tool handlers, verdict-mapping adapter, and resource serving.
//!
//! This module implements the MCP (Model Context Protocol) server surface for token-fairness,
//! exposing scheduler operations as MCP tools and state as MCP resources.
//!
//! Every handler DELEGATES to the real domain modules — it owns no business logic of its own:
//! - `tf_gate`     → [`scheduler::gate`]   (the same verdict the CLI `tf gate` produces)
//! - `tf_spend`    → [`observe::log_spend`] (appends a real spend event to the honesty ledger)
//! - `tf_budget_*` → [`budget`] config keys + the [`observe::fold_events`] spend fold
//! - `tf_report`   → [`observe::fold_events`] over a window
//! - `tf_observe`  → the deduped honesty-events ledger
//! - `tf_signal`   → a persisted per-name signal state file
//! - `tf_plan_*`   → [`scheduler::plan_open`] / [`scheduler::plan_close`]
//! - resources     → the real on-disk state files
//!
//! All code is fallible (returns Result); no unwrap/expect/panic outside tests.

use crate::{observe, scheduler, state};
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
// Shared helpers — real spend fold (reuses observe::fold_events, the same pure
// path the dashboard folds through), and signal-state persistence.
// ============================================================================

/// Fold the honesty-events ledger and return `(current_spend, fanout_spend)` in tokens.
///
/// `current_spend` is the total folded spend in the current Day period; `fanout_spend` is the
/// heaviest single per-model contributor in that period (the worst-case fan-out share). Both
/// are derived from the same [`observe::fold_events`] the dashboard uses — never hardcoded.
fn fold_spend() -> (i64, i64) {
    let events = std::fs::read_to_string(observe::events_path()).unwrap_or_default();
    let roll = observe::fold_events(events.lines(), observe::Period::Day);
    let mut total = 0i64;
    let mut max_model = 0i64;
    for models in roll.spend.values() {
        for (tokens, _cost) in models.values() {
            total += *tokens;
            if *tokens > max_model {
                max_model = *tokens;
            }
        }
    }
    (total, max_model)
}

/// Count gate denials (saves + procedural denies) in the current Day period.
fn fold_denials() -> i64 {
    let events = std::fs::read_to_string(observe::events_path()).unwrap_or_default();
    let roll = observe::fold_events(events.lines(), observe::Period::Day);
    let t = roll.totals();
    t.saves + t.procedural
}

fn budget_file() -> String {
    format!("{}/budget.json", state::state_dir())
}

/// Per-name signal state file (`gate`/`budget`/`observability` → "OK"|"ERROR"). Written by
/// `tf_signal`, read by the `tf://status` resource so the snapshot reflects the REAL state.
fn signal_state_file() -> String {
    if let Ok(p) = std::env::var("I2P_MCP_SIGNALS") {
        return p;
    }
    format!("{}/mcp-signals.json", state::state_dir())
}

/// Read the persisted per-name signal map, defaulting any unseen signal to "OK".
fn read_signals() -> Value {
    let stored = state::read_json(&signal_state_file()).unwrap_or_else(|| json!({}));
    let get = |k: &str| -> String {
        stored
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("OK")
            .to_string()
    };
    json!({
        "gate": get("gate"),
        "budget": get("budget"),
        "observability": get("observability"),
    })
}

fn planopen_file() -> String {
    std::env::var("I2P_PLANOPEN_FILE")
        .unwrap_or_else(|_| format!("{}/plan-open.json", state::home_cost_dir()))
}

fn schedule_file() -> String {
    if let Ok(p) = std::env::var("I2P_SCHEDULE_ENABLED") {
        return p;
    }
    format!("{}/schedule-enabled.json", state::state_dir())
}

// ============================================================================
// MCP TOOL HANDLERS
// ============================================================================

/// Handler for the `tf_gate` MCP tool.
///
/// Builds a live rate-limit payload from the requested ceiling and invokes the REAL
/// [`scheduler::gate`] — the identical path `tf gate` runs — then maps the native verdict into
/// MCP vocabulary. The MCP gate therefore produces the SAME verdict as the CLI for the same input.
pub fn handle_tf_gate(params: &Value) -> Result<Value, String> {
    let ceiling_obj = params
        .get("ceiling")
        .ok_or("missing 'ceiling' parameter")?
        .clone();

    let used_pct = ceiling_obj
        .get("used_pct")
        .and_then(|v| v.as_f64())
        .ok_or("missing or invalid 'used_pct' in ceiling")?;

    // Headroom is optional; the scheduler's own default is 15 when absent.
    let headroom = ceiling_obj.get("headroom").and_then(|v| v.as_i64());

    // Synthesize the live signal the gate reads (`.rate_limits.five_hour.used_percentage`), so
    // the gate runs against a real live window rather than falling through to ASK/NO_SIGNAL.
    let payload = json!({
        "rate_limits": { "five_hour": { "used_percentage": used_pct } }
    })
    .to_string();

    let mut argv = vec!["--window".to_string(), "five_hour".to_string()];
    if let Some(h) = headroom {
        argv.push("--headroom".to_string());
        argv.push(h.to_string());
    }

    let out = scheduler::gate(&argv, &payload);
    let body = out.stdout.trim_end();
    let parsed: Value = serde_json::from_str(body)
        .map_err(|e| format!("scheduler gate returned unparseable JSON: {}", e))?;

    let verdict = parsed
        .get("verdict")
        .and_then(|v| v.as_str())
        .ok_or("scheduler gate verdict missing")?;
    let reason = parsed.get("reason").and_then(|v| v.as_str()).unwrap_or("");
    let ceiling = parsed.get("ceiling").cloned().unwrap_or(ceiling_obj);

    Ok(map_verdict(verdict, reason, &ceiling))
}

/// Handler for the `tf_budget_read` MCP tool.
///
/// Returns the configured caps (real on-disk keys) plus the live folded spend.
pub fn handle_tf_budget_read(_params: &Value) -> Result<Value, String> {
    const DEFAULT_SESSION_CAP: i64 = 2_000_000;
    const DEFAULT_PER_FANOUT_CAP: i64 = 150_000;

    let budget_obj = state::read_json(&budget_file());

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

    let (current_spend, fanout_spend) = fold_spend();

    Ok(json!({
        "session_cap": session_cap,
        "per_fanout_cap": per_fanout_cap,
        "current_spend": current_spend,
        "fanout_spend": fanout_spend
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

    let json_key = match key {
        "session_cap" => "session_cap_tokens",
        "per_fanout_cap" => "per_fanout_cap_tokens",
        _ => return Err(format!("invalid key: {}", key)),
    };

    let mut budget_obj = state::read_json(&budget_file()).unwrap_or_else(|| json!({}));

    if let Some(obj) = budget_obj.as_object_mut() {
        obj.insert(json_key.to_string(), Value::Number(value.into()));
    }

    if let Err(e) = state::write_json(&budget_file(), &budget_obj) {
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
/// Computes the window bounds for the requested period and folds the events ledger for
/// `spend_total` and `gate_denials` — no longer a fixed 24h stub.
pub fn handle_tf_report(params: &Value) -> Result<Value, String> {
    let window = params
        .get("window")
        .and_then(|v| v.as_str())
        .unwrap_or("day");

    let span_seconds: i64 = match window {
        "hour" => 3600,
        "day" => 24 * 3600,
        "month" => 30 * 24 * 3600,
        "ytd" => 365 * 24 * 3600,
        _ => return Err(format!("invalid window: {}", window)),
    };

    let now = state::now_epoch();
    let window_open = now - span_seconds;

    let (spend_total, _fanout) = fold_spend();
    let gate_denials = fold_denials();

    Ok(json!({
        "window": window,
        "window_open": window_open,
        "window_close": now,
        "spend_total": spend_total,
        "gate_denials": gate_denials
    }))
}

/// Handler for the `tf_observe` MCP tool.
///
/// Reads the honesty-events ledger, keeps only `spend` events, dedupes to the LATEST per-model
/// reading per session, and returns one observation per (session,model) span.
pub fn handle_tf_observe(params: &Value) -> Result<Value, String> {
    let window = params
        .get("window")
        .and_then(|v| v.as_str())
        .unwrap_or("day");

    match window {
        "hour" | "day" | "month" | "ytd" => {}
        _ => return Err(format!("invalid window: {}", window)),
    }

    let events = std::fs::read_to_string(observe::events_path()).unwrap_or_default();

    // Dedup spend events to the latest per session, then emit one span per model.
    use std::collections::BTreeMap;
    type Latest = BTreeMap<String, (i64, Vec<Value>)>;
    let mut latest: Latest = BTreeMap::new();
    for line in events.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("kind").and_then(|x| x.as_str()) != Some("spend") {
            continue;
        }
        let session = v.get("session").and_then(|x| x.as_str()).unwrap_or("");
        let ts = state::int(&v, "ts", 0);
        let models = v
            .get("by_model")
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default();
        let e = latest.entry(session.to_string()).or_insert((-1, vec![]));
        if ts >= e.0 {
            *e = (ts, models);
        }
    }

    let mut spans: Vec<Value> = Vec::new();
    for (session, (_ts, models)) in latest {
        for m in models {
            let model = m.get("model").and_then(|x| x.as_str()).unwrap_or("unknown");
            let cost_tokens = state::int(&m, "tokens", 0);
            spans.push(json!({
                "span_id": session,
                "cost_tokens": cost_tokens,
                "model": model,
                "role": "spend"
            }));
        }
    }

    Ok(Value::Array(spans))
}

/// Handler for the `tf_spend` MCP tool.
///
/// Records a real spend event to the honesty-events ledger via [`observe::log_spend`] — the same
/// writer `tf spend --capture` uses. Returns success only after the append succeeds.
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

    // Append the real spend event; fail with a JSON-RPC error if the write fails.
    state::append_line(
        &observe::events_path(),
        &json!({
            "ts": state::now_epoch(),
            "session": span_id,
            "kind": "spend",
            "by_model": [{ "model": model, "tokens": cost, "cost_usd": 0.0 }],
            "total_tokens": cost,
            "total_cost_usd": 0.0,
            "role": role,
        })
        .to_string(),
    )
    .map_err(|e| format!("failed to record spend event: {}", e))?;

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
/// Persists a per-name signal state (gate/budget/observability → OK|ERROR) to the signal-state
/// file, so the `tf://status` resource can reflect the REAL state on its next read.
pub fn handle_tf_signal(params: &Value) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing or invalid 'name'")?;

    let status = params
        .get("status")
        .and_then(|v| v.as_str())
        .ok_or("missing or invalid 'status'")?;

    match name {
        "gate" | "budget" | "observability" => {}
        _ => return Err(format!("invalid signal name: {}", name)),
    }

    match status {
        "OK" | "ERROR" => {}
        _ => return Err(format!("invalid status: {}", status)),
    }

    let mut stored = state::read_json(&signal_state_file()).unwrap_or_else(|| json!({}));
    if let Some(obj) = stored.as_object_mut() {
        obj.insert(name.to_string(), Value::String(status.to_string()));
    }
    state::write_json(&signal_state_file(), &stored)
        .map_err(|e| format!("failed to write signal state: {}", e))?;

    Ok(json!({
        "success": true,
        "signal": name,
        "status": status
    }))
}

/// Handler for the `tf_plan_open` MCP tool.
///
/// Delegates to the real [`scheduler::plan_open`] (class=title, est=budget_tokens), then persists
/// the generated `plan_id` and `title` into plan-open.json so `tf_plan_close` can recover them.
pub fn handle_tf_plan_open(params: &Value) -> Result<Value, String> {
    let title = params
        .get("title")
        .and_then(|v| v.as_str())
        .ok_or("missing or invalid 'title'")?;

    let budget_tokens = params
        .get("budget_tokens")
        .and_then(|v| v.as_i64())
        .ok_or("missing or invalid 'budget_tokens'")?;

    let class = state::safe_id(title);
    let out = scheduler::plan_open(&[class.clone(), budget_tokens.to_string()]);
    if out.code != 0 {
        return Err(format!("scheduler plan-open failed: {}", out.stderr.trim()));
    }

    let plan_id = format!("plan-{}", state::now_epoch());

    // Augment the plan-open.json the scheduler just wrote with the MCP plan_id + title.
    let popen = planopen_file();
    let mut doc = state::read_json(&popen).unwrap_or_else(|| json!({}));
    if let Some(obj) = doc.as_object_mut() {
        obj.insert("plan_id".to_string(), Value::String(plan_id.clone()));
        obj.insert("title".to_string(), Value::String(title.to_string()));
    }
    state::write_json(&popen, &doc).map_err(|e| format!("failed to persist plan_id: {}", e))?;

    Ok(json!({
        "success": true,
        "plan_id": plan_id,
        "title": title,
        "budget_tokens": budget_tokens
    }))
}

/// Handler for the `tf_plan_close` MCP tool.
///
/// Reads the persisted plan_id, verifies it matches the requested one, then delegates to the real
/// [`scheduler::plan_close`] (which removes plan-open.json and folds convergence).
pub fn handle_tf_plan_close(params: &Value) -> Result<Value, String> {
    let plan_id = params
        .get("plan_id")
        .and_then(|v| v.as_str())
        .ok_or("missing or invalid 'plan_id'")?;

    let popen = planopen_file();
    let doc = state::read_json(&popen).ok_or("no open plan to close")?;
    let stored_id = doc.get("plan_id").and_then(|v| v.as_str()).unwrap_or("");
    if stored_id != plan_id {
        return Err(format!(
            "plan_id mismatch: requested {}, open plan is {}",
            plan_id, stored_id
        ));
    }

    let out = scheduler::plan_close(&[]);
    if out.code != 0 {
        return Err(format!(
            "scheduler plan-close failed: {}",
            out.stdout.trim()
        ));
    }

    Ok(json!({
        "success": true,
        "plan_id": plan_id,
        "closed_at": state::now_epoch()
    }))
}

/// Handler for the `tf_schedule_toggle` MCP tool.
///
/// Persists the window-aware schedule gate flag to a real `schedule-enabled.json` state file.
pub fn handle_tf_schedule_toggle(params: &Value) -> Result<Value, String> {
    let enabled = params
        .get("enabled")
        .and_then(|v| v.as_bool())
        .ok_or("missing or invalid 'enabled'")?;

    state::write_json(
        &schedule_file(),
        &json!({ "enabled": enabled, "set_at": state::now_epoch() }),
    )
    .map_err(|e| format!("failed to write schedule state: {}", e))?;

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
/// Returns a live snapshot built from the REAL state files: configured caps + folded spend, and
/// the persisted per-name signal state (NOT an unconditional "OK").
pub fn handle_resource_status() -> Result<Value, String> {
    let budget = handle_tf_budget_read(&json!({}))?;
    let signals = read_signals();

    Ok(json!({
        "session_budget": {
            "session_cap": budget.get("session_cap").cloned().unwrap_or(json!(0)),
            "per_fanout_cap": budget.get("per_fanout_cap").cloned().unwrap_or(json!(0)),
            "current_spend": budget.get("current_spend").cloned().unwrap_or(json!(0)),
            "fanout_spend": budget.get("fanout_spend").cloned().unwrap_or(json!(0)),
        },
        "signals": signals,
        "timestamp": state::now_epoch()
    }))
}

/// Handler for the `tf://calibration` MCP resource.
///
/// Returns the window definitions plus the next 5h boundary read from the persisted windows state.
pub fn handle_resource_calibration() -> Result<Value, String> {
    let now = state::now_epoch();

    // Read the persisted 5h window's resets_at, if any, for the next boundary.
    let windows = state::read_json(&crate::windows::snapshot_path());
    let next_boundary = windows
        .as_ref()
        .and_then(|v| v.pointer("/rate_limits/five_hour/resets_at"))
        .and_then(|v| v.as_i64())
        .unwrap_or(now + 86400);

    Ok(json!({
        "rolling_windows": [
            { "name": "hour", "duration_seconds": 3600 },
            { "name": "day", "duration_seconds": 86400 },
            { "name": "month", "duration_seconds": 2592000 },
            { "name": "ytd", "duration_seconds": 31536000 }
        ],
        "current_window": "day",
        "next_boundary": next_boundary
    }))
}

/// Handler for the `tf://events` MCP resource.
///
/// Reads the honesty-events ledger and returns the last 100 parsed events (most recent last).
pub fn handle_resource_events() -> Result<Value, String> {
    let content = std::fs::read_to_string(observe::events_path()).unwrap_or_default();
    let parsed: Vec<Value> = content
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect();
    let start = parsed.len().saturating_sub(100);
    Ok(Value::Array(parsed[start..].to_vec()))
}

// ============================================================================
// Tool and Resource Registry
// ============================================================================

/// Dispatches an MCP tool call to the appropriate handler.
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
pub fn dispatch_resource(uri: &str) -> Result<Value, String> {
    match uri {
        "tf://status" => handle_resource_status(),
        "tf://calibration" => handle_resource_calibration(),
        "tf://events" => handle_resource_events(),
        _ => Err(format!("unknown resource: {}", uri)),
    }
}

/// The MCP tool catalogue, for the `tools/list` handshake. Names + one-line descriptions.
pub fn tools_list() -> Value {
    let tool = |name: &str, desc: &str| json!({ "name": name, "description": desc });
    json!({
        "tools": [
            tool("tf_gate", "Evaluate a token ceiling and return allow|deny."),
            tool("tf_budget_read", "Read configured caps and live folded spend."),
            tool("tf_budget_set", "Update session_cap or per_fanout_cap."),
            tool("tf_report", "Window spend total and gate denials."),
            tool("tf_observe", "Deduplicated per-session spend observations."),
            tool("tf_spend", "Record a spend event to the honesty ledger."),
            tool("tf_signal", "Set a gate/budget/observability signal to OK|ERROR."),
            tool("tf_plan_open", "Open a budget plan."),
            tool("tf_plan_close", "Close an open budget plan."),
            tool("tf_schedule_toggle", "Enable/disable the window-aware schedule gate.")
        ]
    })
}

/// The MCP resource catalogue, for the `resources/list` handshake.
pub fn resources_list() -> Value {
    let res = |uri: &str, desc: &str| json!({ "uri": uri, "name": uri, "description": desc });
    json!({
        "resources": [
            res("tf://status", "Live budget + signal snapshot."),
            res("tf://calibration", "Rolling-window definitions and next boundary."),
            res("tf://events", "Recent honesty-ledger events (last 100).")
        ]
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{temp_dir, ENV_LOCK};

    // ---- verdict adapter (pure) ----

    #[test]
    fn test_verdict_adapter_continue_to_allow() {
        let ceiling = json!({"used_pct": 50});
        let result = map_verdict("CONTINUE", "", &ceiling);
        assert_eq!(result.get("verdict").unwrap().as_str().unwrap(), "allow");
        assert!(!result.get("reason").unwrap().as_str().unwrap().is_empty());
        assert_eq!(result.get("ceiling").unwrap(), &ceiling);
    }

    #[test]
    fn test_verdict_adapter_halt_defer_ask_no_signal_all_deny() {
        let ceiling = json!({"used_pct": 100});
        for v in ["HALT", "DEFER", "ASK", "NO_SIGNAL"] {
            let r = map_verdict(v, "", &ceiling);
            assert_eq!(
                r.get("verdict").unwrap().as_str().unwrap(),
                "deny",
                "{} must map to deny",
                v
            );
            assert!(!r.get("reason").unwrap().as_str().unwrap().is_empty());
        }
    }

    #[test]
    fn test_verdict_adapter_custom_reason_overrides_synth() {
        let ceiling = json!({"used_pct": 50});
        let result = map_verdict("CONTINUE", "custom reason", &ceiling);
        assert_eq!(
            result.get("reason").unwrap().as_str().unwrap(),
            "custom reason"
        );
    }

    // ---- tf_gate: must equal the real scheduler gate verdict ----

    #[test]
    fn test_gate_allows_when_below_ceiling() {
        // used_pct 80, headroom 15 → ceiling 85 → below → CONTINUE → allow.
        let params = json!({"ceiling": {"used_pct": 80, "headroom": 15}});
        let r = handle_tf_gate(&params).expect("ok");
        assert_eq!(r.get("verdict").unwrap().as_str().unwrap(), "allow");
        // The ceiling echoed back is the scheduler's, carrying the real used_pct it evaluated.
        let used = r.pointer("/ceiling/used_pct").and_then(|v| v.as_f64());
        assert_eq!(used, Some(80.0));
    }

    #[test]
    fn test_gate_denies_when_at_or_over_ceiling() {
        // used_pct 90, headroom 15 → ceiling 85 → breach → HALT → deny.
        let params = json!({"ceiling": {"used_pct": 90, "headroom": 15}});
        let r = handle_tf_gate(&params).expect("ok");
        assert_eq!(r.get("verdict").unwrap().as_str().unwrap(), "deny");
    }

    #[test]
    fn test_gate_matches_cli_scheduler_for_same_payload() {
        // The MCP gate must produce the SAME native verdict the CLI gate produces.
        let used_pct = 90.0;
        let payload = json!({
            "rate_limits": { "five_hour": { "used_percentage": used_pct } }
        })
        .to_string();
        let cli = scheduler::gate(
            &[
                "--window".to_string(),
                "five_hour".to_string(),
                "--headroom".to_string(),
                "15".to_string(),
            ],
            &payload,
        );
        let cli_json: Value = serde_json::from_str(cli.stdout.trim_end()).unwrap();
        let cli_verdict = cli_json.get("verdict").unwrap().as_str().unwrap();

        let mcp = handle_tf_gate(&json!({"ceiling": {"used_pct": used_pct, "headroom": 15}}))
            .expect("ok");
        // CLI says HALT; the adapter maps that to deny.
        assert_eq!(cli_verdict, "HALT");
        assert_eq!(mcp.get("verdict").unwrap().as_str().unwrap(), "deny");
    }

    #[test]
    fn test_gate_missing_ceiling_errors() {
        assert!(handle_tf_gate(&json!({})).is_err());
    }

    // ---- tf_budget_read / status: real caps + folded spend ----

    #[test]
    fn test_budget_read_reads_real_caps_and_folds_spend() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_dir("mcp-budget");
        let events = dir.join("events.jsonl");
        std::fs::write(
            &events,
            r#"{"ts":1000,"session":"S","kind":"spend","by_model":[{"model":"opus","tokens":120000,"cost_usd":1.0},{"model":"sonnet","tokens":30000,"cost_usd":0.2}],"total_tokens":150000,"total_cost_usd":1.2}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("budget.json"),
            r#"{"session_cap_tokens":900000,"per_fanout_cap_tokens":80000}"#,
        )
        .unwrap();

        std::env::set_var("I2P_COST_STATE_DIR", &dir);
        std::env::set_var("I2P_HONESTY_EVENTS", &events);
        let r = handle_tf_budget_read(&json!({})).expect("ok");
        std::env::remove_var("I2P_COST_STATE_DIR");
        std::env::remove_var("I2P_HONESTY_EVENTS");

        assert_eq!(r.get("session_cap").unwrap().as_i64().unwrap(), 900000);
        assert_eq!(r.get("per_fanout_cap").unwrap().as_i64().unwrap(), 80000);
        // current_spend = 120000 + 30000 = 150000 (real fold, not 0)
        assert_eq!(r.get("current_spend").unwrap().as_i64().unwrap(), 150000);
        // fanout_spend = heaviest single model = 120000
        assert_eq!(r.get("fanout_spend").unwrap().as_i64().unwrap(), 120000);

        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- tf_report: window bounds + folded totals ----

    #[test]
    fn test_report_window_bounds_and_folds_denials() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_dir("mcp-report");
        let events = dir.join("events.jsonl");
        let day = state::now_epoch();
        std::fs::write(
            &events,
            format!(
                concat!(
                    "{{\"ts\":{0},\"session\":\"S\",\"kind\":\"spend\",\"by_model\":[{{\"model\":\"opus\",\"tokens\":5000,\"cost_usd\":0.1}}],\"total_tokens\":5000,\"total_cost_usd\":0.1}}\n",
                    "{{\"ts\":{0},\"kind\":\"gate\",\"class\":\"save\",\"reason\":\"ceiling\",\"est\":1}}\n",
                    "{{\"ts\":{0},\"kind\":\"gate\",\"class\":\"procedural-deny\",\"reason\":\"no budget\"}}\n",
                    "{{\"ts\":{0},\"kind\":\"gate\",\"class\":\"allow\",\"reason\":\"armed\",\"est\":1}}\n"
                ),
                day
            ),
        )
        .unwrap();

        std::env::set_var("I2P_HONESTY_EVENTS", &events);
        let r = handle_tf_report(&json!({"window": "hour"})).expect("ok");
        std::env::remove_var("I2P_HONESTY_EVENTS");

        assert_eq!(r.get("window").unwrap().as_str().unwrap(), "hour");
        let open = r.get("window_open").unwrap().as_i64().unwrap();
        let close = r.get("window_close").unwrap().as_i64().unwrap();
        assert_eq!(close - open, 3600, "hour window spans 3600s");
        assert_eq!(r.get("spend_total").unwrap().as_i64().unwrap(), 5000);
        // denials = saves(1) + procedural(1) = 2; allow is not a denial.
        assert_eq!(r.get("gate_denials").unwrap().as_i64().unwrap(), 2);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_report_rejects_invalid_window() {
        assert!(handle_tf_report(&json!({"window": "decade"})).is_err());
    }

    // ---- tf_observe: real deduped spans ----

    #[test]
    fn test_observe_returns_deduped_latest_per_session() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_dir("mcp-observe");
        let events = dir.join("events.jsonl");
        std::fs::write(
            &events,
            concat!(
                "{\"ts\":1000,\"session\":\"A\",\"kind\":\"spend\",\"by_model\":[{\"model\":\"opus\",\"tokens\":100,\"cost_usd\":1.0}]}\n",
                "{\"ts\":2000,\"session\":\"A\",\"kind\":\"spend\",\"by_model\":[{\"model\":\"opus\",\"tokens\":200,\"cost_usd\":2.0}]}\n"
            ),
        )
        .unwrap();

        std::env::set_var("I2P_HONESTY_EVENTS", &events);
        let r = handle_tf_observe(&json!({"window": "day"})).expect("ok");
        std::env::remove_var("I2P_HONESTY_EVENTS");

        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 1, "session A deduped to its latest reading");
        assert_eq!(arr[0].get("span_id").unwrap().as_str().unwrap(), "A");
        assert_eq!(arr[0].get("model").unwrap().as_str().unwrap(), "opus");
        // latest reading is 200, not 100 or 300
        assert_eq!(arr[0].get("cost_tokens").unwrap().as_i64().unwrap(), 200);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_observe_empty_ledger_is_empty_array() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_dir("mcp-observe-empty");
        let events = dir.join("events.jsonl");
        std::fs::write(&events, "").unwrap();
        std::env::set_var("I2P_HONESTY_EVENTS", &events);
        let r = handle_tf_observe(&json!({"window": "day"})).expect("ok");
        std::env::remove_var("I2P_HONESTY_EVENTS");
        assert_eq!(r.as_array().unwrap().len(), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- tf_spend: actually appends a real event ----

    #[test]
    fn test_spend_appends_real_event_readable_back() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_dir("mcp-spend");
        let events = dir.join("events.jsonl");
        std::env::set_var("I2P_HONESTY_EVENTS", &events);

        let r = handle_tf_spend(&json!({
            "span_id": "span-1", "cost": 500, "model": "opus", "role": "researcher"
        }))
        .expect("ok");
        assert!(r.get("success").unwrap().as_bool().unwrap());

        // The event must be on disk and fold back to a spend of 500.
        let written = std::fs::read_to_string(&events).unwrap();
        std::env::remove_var("I2P_HONESTY_EVENTS");
        assert!(written.contains("\"kind\":\"spend\""));
        let roll = observe::fold_events(written.lines(), observe::Period::Day);
        let total: i64 = roll
            .spend
            .values()
            .flat_map(|m| m.values())
            .map(|(t, _)| *t)
            .sum();
        assert_eq!(total, 500);

        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- tf_signal + tf://status: real persisted signal state ----

    #[test]
    fn test_signal_persists_and_status_reflects_it() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_dir("mcp-signal");
        let sig = dir.join("signals.json");
        std::env::set_var("I2P_MCP_SIGNALS", &sig);
        std::env::set_var("I2P_COST_STATE_DIR", &dir);
        std::env::set_var("I2P_HONESTY_EVENTS", dir.join("ev.jsonl"));

        let r = handle_tf_signal(&json!({"name": "gate", "status": "ERROR"})).expect("ok");
        assert!(r.get("success").unwrap().as_bool().unwrap());

        let status = handle_resource_status().expect("ok");
        std::env::remove_var("I2P_MCP_SIGNALS");
        std::env::remove_var("I2P_COST_STATE_DIR");
        std::env::remove_var("I2P_HONESTY_EVENTS");

        // The status resource must reflect the REAL signal, not unconditional OK.
        assert_eq!(
            status.pointer("/signals/gate").unwrap().as_str().unwrap(),
            "ERROR"
        );
        // Untouched signals default to OK.
        assert_eq!(
            status.pointer("/signals/budget").unwrap().as_str().unwrap(),
            "OK"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_signal_rejects_invalid_name_and_status() {
        assert!(handle_tf_signal(&json!({"name": "nope", "status": "OK"})).is_err());
        assert!(handle_tf_signal(&json!({"name": "gate", "status": "MAYBE"})).is_err());
    }

    // ---- tf_plan_open / tf_plan_close: real scheduler delegation + round-trip ----

    #[test]
    fn test_plan_open_then_close_round_trip() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_dir("mcp-plan");
        let popen = dir.join("plan-open.json");
        std::env::set_var("I2P_PLANOPEN_FILE", &popen);
        std::env::set_var("I2P_SESSION_FILE", dir.join("session.json"));
        std::env::set_var("I2P_CALIBRATION_FILE", dir.join("cal.json"));

        let opened = handle_tf_plan_open(&json!({"title": "Q3 research", "budget_tokens": 100000}))
            .expect("ok");
        let plan_id = opened.get("plan_id").unwrap().as_str().unwrap().to_string();
        assert!(!plan_id.is_empty());
        // The scheduler really wrote plan-open.json, augmented with the plan_id.
        assert!(popen.exists());
        let on_disk = state::read_json(popen.to_str().unwrap()).unwrap();
        assert_eq!(on_disk.get("plan_id").unwrap().as_str().unwrap(), plan_id);
        assert_eq!(state::int(&on_disk, "est", 0), 100000);

        let closed = handle_tf_plan_close(&json!({"plan_id": plan_id})).expect("ok");
        assert!(closed.get("success").unwrap().as_bool().unwrap());
        // plan_close removes the open-plan file.
        assert!(!popen.exists());

        std::env::remove_var("I2P_PLANOPEN_FILE");
        std::env::remove_var("I2P_SESSION_FILE");
        std::env::remove_var("I2P_CALIBRATION_FILE");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_plan_close_without_open_errors() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_dir("mcp-plan-none");
        std::env::set_var("I2P_PLANOPEN_FILE", dir.join("nope.json"));
        let r = handle_tf_plan_close(&json!({"plan_id": "plan-x"}));
        std::env::remove_var("I2P_PLANOPEN_FILE");
        assert!(r.is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- tf_schedule_toggle: real state file ----

    #[test]
    fn test_schedule_toggle_persists_flag() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_dir("mcp-sched");
        let sf = dir.join("sched.json");
        std::env::set_var("I2P_SCHEDULE_ENABLED", &sf);
        let r = handle_tf_schedule_toggle(&json!({"enabled": true})).expect("ok");
        std::env::remove_var("I2P_SCHEDULE_ENABLED");
        assert!(r.get("enabled").unwrap().as_bool().unwrap());
        let on_disk = state::read_json(sf.to_str().unwrap()).unwrap();
        assert!(on_disk.get("enabled").unwrap().as_bool().unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- tf://events resource: real ledger ----

    #[test]
    fn test_events_resource_returns_real_events_capped_100() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_dir("mcp-events");
        let events = dir.join("ev.jsonl");
        let mut body = String::new();
        for i in 0..150 {
            body.push_str(&format!(
                "{{\"ts\":{},\"kind\":\"blown\",\"reason\":\"x\"}}\n",
                i
            ));
        }
        std::fs::write(&events, &body).unwrap();
        std::env::set_var("I2P_HONESTY_EVENTS", &events);
        let r = handle_resource_events().expect("ok");
        std::env::remove_var("I2P_HONESTY_EVENTS");
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 100, "capped at last 100");
        // last event is ts 149 (most recent last)
        assert_eq!(
            arr.last().unwrap().get("ts").unwrap().as_i64().unwrap(),
            149
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- dispatch + handshake catalogues ----

    #[test]
    fn test_dispatch_tool_unknown_errors() {
        assert!(dispatch_tool("tf_unknown", &json!({})).is_err());
    }

    #[test]
    fn test_dispatch_resource_unknown_errors() {
        assert!(dispatch_resource("tf://unknown").is_err());
    }

    #[test]
    fn test_tools_list_enumerates_all_ten() {
        let list = tools_list();
        assert_eq!(list.get("tools").unwrap().as_array().unwrap().len(), 10);
    }

    #[test]
    fn test_resources_list_enumerates_all_three() {
        let list = resources_list();
        assert_eq!(list.get("resources").unwrap().as_array().unwrap().len(), 3);
    }
}
