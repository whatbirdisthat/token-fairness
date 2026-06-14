//! Dashboard HTTP server with REST endpoints, WebSocket streaming, and Prometheus metrics.
//!
//! Features:
//! - Embedded Chart.js HTML at GET /
//! - REST JSON endpoints for live state snapshots (session budget, spend by model, guard efficacy, MAPE)
//! - WebSocket broadcaster at /ws for real-time event streaming
//! - Optional Prometheus metrics at GET /metrics

use crate::state;
use crate::telemetry::{fold_events, FoldState};
use serde_json::json;
use std::path::Path;

/// Errors in the dashboard.
#[derive(Debug, Clone)]
pub enum DashboardError {
    BindError(String),
    IoError(String),
    FoldError(String),
}

impl std::fmt::Display for DashboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DashboardError::BindError(e) => write!(f, "bind error: {}", e),
            DashboardError::IoError(e) => write!(f, "IO error: {}", e),
            DashboardError::FoldError(e) => write!(f, "fold error: {}", e),
        }
    }
}

impl std::error::Error for DashboardError {}

// ============================================================================
// REST Endpoint Handlers
// ============================================================================

/// Compute current fold state from JSONL files.
pub fn compute_fold() -> Result<FoldState, DashboardError> {
    let events_path = crate::observe::events_path();

    // Read events, defaulting to empty if file doesn't exist
    let events_content = std::fs::read_to_string(&events_path).unwrap_or_default();
    let lines: Vec<&str> = events_content.lines().collect();

    let mut state = fold_events(&lines, crate::telemetry::Period::Day);

    // Optionally fold accuracy ledger (if it exists)
    let accuracy_path = state::accuracy_ledger();
    if Path::new(&accuracy_path).exists() {
        let accuracy_content = std::fs::read_to_string(&accuracy_path).unwrap_or_default();
        let acc_lines: Vec<&str> = accuracy_content.lines().collect();
        crate::telemetry::fold_accuracy(&mut state, &acc_lines, crate::telemetry::Period::Day);
    }

    Ok(state)
}

/// GET /api/session-budget — current session budget state.
pub fn endpoint_session_budget() -> Result<serde_json::Value, DashboardError> {
    let state = compute_fold()?;

    // Read session budget from state files
    let budget_path = format!("{}/budget.json", state::state_dir());
    let budget_json = state::read_json(&budget_path).unwrap_or(json!({}));

    let session_cap = state::int(&budget_json, "session_cap_tokens", 2_000_000) as u64;
    let per_fanout_cap = state::int(&budget_json, "per_fanout_cap_tokens", 100_000) as u64;

    let ceiling_pct = if session_cap > 0 {
        (state.session_spend as f64 / session_cap as f64) * 100.0
    } else {
        0.0
    };

    Ok(json!({
        "session_cap": session_cap,
        "per_fanout_cap": per_fanout_cap,
        "current_spend": state.session_spend,
        "ceiling_pct": ceiling_pct.clamp(0.0, 100.0),
    }))
}

/// GET /api/spend-by-model — breakdown of spend by model.
pub fn endpoint_spend_by_model() -> Result<serde_json::Value, DashboardError> {
    let state = compute_fold()?;

    let models: Vec<serde_json::Value> = state
        .spend_by_model
        .iter()
        .map(|(model, (tokens, count))| {
            json!({
                "model": model,
                "tokens": tokens,
                "count": count,
            })
        })
        .collect();

    Ok(serde_json::Value::Array(models))
}

/// GET /api/guard-efficacy — save/blown counts and save rate.
pub fn endpoint_guard_efficacy() -> Result<serde_json::Value, DashboardError> {
    let state = compute_fold()?;

    let total_verdicts = state.saves_count as u64 + state.blown_count as u64;
    let save_rate_pct = if total_verdicts > 0 {
        (state.saves_count as f64 / total_verdicts as f64) * 100.0
    } else {
        0.0
    };

    Ok(json!({
        "saves_count": state.saves_count,
        "blown_count": state.blown_count,
        "save_rate_pct": save_rate_pct,
    }))
}

/// GET /api/estimator-accuracy — MAPE and error bounds.
pub fn endpoint_estimator_accuracy() -> Result<serde_json::Value, DashboardError> {
    let state = compute_fold()?;

    // For now, simple MAPE; min/max error would require tracking per-sample
    Ok(json!({
        "mean_absolute_percentage_error": state.mape,
        "min_error_pct": 0.0, // Would require per-sample tracking
        "max_error_pct": 100.0, // Would require per-sample tracking
    }))
}

/// GET /api/windows — the ACCOUNT-WIDE rolling-window lockout signal (5-hour + 7-day).
///
/// This is the real lockout ceiling: the provider's `used_percentage` shared across ALL
/// concurrent agents/sessions on the account — distinct from `/api/session-budget`, which is
/// this session's tokens ÷ a manual cap. It reuses the EXACT machinery the gate and the
/// statusline use ([`crate::windows::live_windows`] with [`crate::budget::SNAPSHOT_MAX_AGE`],
/// [`crate::windows::load`], [`crate::budget::win_disp`], [`crate::windows::snapshot_captured_at`])
/// so the dashboard cannot drift from `tf budget status`.
///
/// When no FRESH snapshot is available the account-wide signal is BLIND: `fresh:false`,
/// `blind:true`, and each window's `used_pct` is `null` (we will not fabricate a percentage we
/// cannot see — the bug this whole endpoint exists to fix).
pub fn endpoint_windows() -> Result<serde_json::Value, DashboardError> {
    let max_age = crate::budget::SNAPSHOT_MAX_AGE;
    let (l5, l7) = crate::windows::live_windows(max_age);
    let (five, seven) = crate::windows::load();
    let cur = crate::budget::session_tokens();

    // Caps + headroom from the same budget config the gate reads.
    let budget_path = format!("{}/budget.json", state::state_dir());
    let budget_json = state::read_json(&budget_path).unwrap_or(json!({}));
    let five_cap = state::int(
        &budget_json,
        "session_cap_tokens",
        crate::budget::DEFAULT_SESSION_CAP,
    );
    let weekly_cap = state::int(
        &budget_json,
        "weekly_cap_tokens",
        crate::budget::DEFAULT_WEEKLY_CAP,
    );
    let headroom = state::int(
        &budget_json,
        "headroom_pct",
        crate::windows::DEFAULT_HEADROOM,
    );

    let fresh = l5.is_some() || l7.is_some();
    let captured_at = crate::windows::snapshot_captured_at();
    let snapshot_age_seconds = captured_at.map(|c| (state::now_epoch() - c).max(0));

    // headroom_pct: how much room remains below the ceiling on the MOST-CONSTRAINED fresh window
    // (the binding lockout risk). Null when blind.
    let ceiling = (100 - headroom) as f64;
    let headroom_pct = [l5, l7]
        .into_iter()
        .flatten()
        .map(|used| (ceiling - used).max(0.0))
        .reduce(f64::min);

    Ok(json!({
        "captured_at": captured_at,
        "snapshot_age_seconds": snapshot_age_seconds,
        "max_age_seconds": max_age,
        "fresh": fresh,
        "blind": !fresh,
        "headroom_pct": headroom_pct,
        "note": "Account-wide rolling windows — shared across ALL agents/sessions on this provider account.",
        "five_hour": crate::budget::win_disp(&five, l5, five_cap, headroom, cur),
        "seven_day": crate::budget::win_disp(&seven, l7, weekly_cap, headroom, cur),
    }))
}

/// GET /api/mcp-invocations — visible proof the MCP surface is being hit.
///
/// Reads the shared, ungated audit log ([`crate::observe::mcp_invocations_path`]) the `mcp`-feature
/// writer appends to, returning the last ~100 parsed records (most recent last) plus a total
/// `count`. The records are metadata-only (method + sorted param key names) — no argument values
/// are ever present, by construction at the write side.
pub fn endpoint_mcp_invocations() -> Result<serde_json::Value, DashboardError> {
    let content =
        std::fs::read_to_string(crate::observe::mcp_invocations_path()).unwrap_or_default();
    let parsed: Vec<serde_json::Value> = content
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .collect();
    let count = parsed.len();
    let start = count.saturating_sub(100);
    Ok(json!({
        "count": count,
        "recent": parsed[start..].to_vec(),
    }))
}

// ============================================================================
// Prometheus Metrics
// ============================================================================

/// Prometheus gauge/counter metrics.
///
/// The session ceiling is PER-SESSION (this session's spend ÷ a manual cap). The window gauges
/// are ACCOUNT-WIDE, sourced from the live rolling-window snapshot via
/// [`crate::windows::live_windows`] — the same signal the gate enforces. They are emitted only
/// when the snapshot is FRESH (otherwise we would publish a stale or fabricated percentage); a
/// `tf_window_snapshot_fresh` 0/1 gauge plus `tf_window_snapshot_age_seconds` make freshness
/// explicit for any alerting rule. This replaces the previously-faked `tf_weekly_ceiling_percent`,
/// which merely copied the per-session number.
#[derive(Debug, Clone)]
pub struct PrometheusMetrics {
    pub session_spend_tokens: u64,
    pub session_ceiling_percent: f64,
    /// Account-wide 5-hour window `used_percentage`; `None` when the snapshot is stale/absent.
    pub five_hour_used_percent: Option<f64>,
    /// Account-wide 7-day window `used_percentage`; `None` when the snapshot is stale/absent.
    pub seven_day_used_percent: Option<f64>,
    /// 1 when a fresh account-wide snapshot drives the window gauges, else 0.
    pub window_snapshot_fresh: bool,
    /// Age of the account-wide snapshot in seconds; `None` when no snapshot exists.
    pub window_snapshot_age_seconds: Option<i64>,
    pub guard_saves_total: u64,
    pub guard_blown_total: u64,
    pub guard_procedural_denies_total: u64,
}

impl PrometheusMetrics {
    /// Generate from folded event state. Reads the account-wide window snapshot directly so the
    /// exported window gauges match `tf budget status` / `/api/windows`.
    pub fn from_fold(state: &FoldState, budget_json: &serde_json::Value) -> Self {
        let session_cap = state::int(budget_json, "session_cap_tokens", 2_000_000) as u64;
        let session_ceiling_percent = if session_cap > 0 {
            (state.session_spend as f64 / session_cap as f64) * 100.0
        } else {
            0.0
        };

        let (l5, l7) = crate::windows::live_windows(crate::budget::SNAPSHOT_MAX_AGE);
        let captured_at = crate::windows::snapshot_captured_at();
        let window_snapshot_age_seconds = captured_at.map(|c| (state::now_epoch() - c).max(0));

        PrometheusMetrics {
            session_spend_tokens: state.session_spend,
            session_ceiling_percent,
            five_hour_used_percent: l5,
            seven_day_used_percent: l7,
            window_snapshot_fresh: l5.is_some() || l7.is_some(),
            window_snapshot_age_seconds,
            guard_saves_total: state.saves_count as u64,
            guard_blown_total: state.blown_count as u64,
            guard_procedural_denies_total: state.procedural_denies_count as u64,
        }
    }
}

impl std::fmt::Display for PrometheusMetrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "# HELP tf_session_spend_tokens Current session cumulative spend"
        )?;
        writeln!(f, "# TYPE tf_session_spend_tokens gauge")?;
        writeln!(f, "tf_session_spend_tokens {}", self.session_spend_tokens)?;
        writeln!(f)?;

        writeln!(
            f,
            "# HELP tf_session_ceiling_percent PER-SESSION spend as percentage of the manual session cap (this session only, NOT account-wide — see tf_five_hour_used_percent / tf_seven_day_used_percent for the account-wide lockout signal)"
        )?;
        writeln!(f, "# TYPE tf_session_ceiling_percent gauge")?;
        writeln!(
            f,
            "tf_session_ceiling_percent {:.2}",
            self.session_ceiling_percent
        )?;
        writeln!(f)?;

        // Account-wide rolling-window gauges — emitted ONLY when the snapshot is fresh, so a stale
        // signal is never published as if it were live (the freshness gauge below disambiguates).
        writeln!(
            f,
            "# HELP tf_five_hour_used_percent ACCOUNT-WIDE 5-hour rolling window used_percentage (shared across all agents/sessions); emitted only when the snapshot is fresh"
        )?;
        writeln!(f, "# TYPE tf_five_hour_used_percent gauge")?;
        if let Some(p) = self.five_hour_used_percent {
            writeln!(f, "tf_five_hour_used_percent {:.2}", p)?;
        }
        writeln!(f)?;

        writeln!(
            f,
            "# HELP tf_seven_day_used_percent ACCOUNT-WIDE 7-day rolling window used_percentage (shared across all agents/sessions); emitted only when the snapshot is fresh"
        )?;
        writeln!(f, "# TYPE tf_seven_day_used_percent gauge")?;
        if let Some(p) = self.seven_day_used_percent {
            writeln!(f, "tf_seven_day_used_percent {:.2}", p)?;
        }
        writeln!(f)?;

        writeln!(
            f,
            "# HELP tf_window_snapshot_fresh 1 if a fresh account-wide window snapshot is driving the window gauges, else 0 (BLIND)"
        )?;
        writeln!(f, "# TYPE tf_window_snapshot_fresh gauge")?;
        writeln!(
            f,
            "tf_window_snapshot_fresh {}",
            if self.window_snapshot_fresh { 1 } else { 0 }
        )?;
        writeln!(f)?;

        writeln!(
            f,
            "# HELP tf_window_snapshot_age_seconds Age in seconds of the account-wide window snapshot"
        )?;
        writeln!(f, "# TYPE tf_window_snapshot_age_seconds gauge")?;
        if let Some(age) = self.window_snapshot_age_seconds {
            writeln!(f, "tf_window_snapshot_age_seconds {}", age)?;
        }
        writeln!(f)?;

        writeln!(
            f,
            "# HELP tf_guard_saves_total Total count of guard SAVE verdicts"
        )?;
        writeln!(f, "# TYPE tf_guard_saves_total counter")?;
        writeln!(f, "tf_guard_saves_total {}", self.guard_saves_total)?;
        writeln!(f)?;

        writeln!(f, "# HELP tf_guard_blown_total Total count of BLOWN events")?;
        writeln!(f, "# TYPE tf_guard_blown_total counter")?;
        writeln!(f, "tf_guard_blown_total {}", self.guard_blown_total)?;
        writeln!(f)?;

        writeln!(
            f,
            "# HELP tf_guard_procedural_denies_total Total procedural denials"
        )?;
        writeln!(f, "# TYPE tf_guard_procedural_denies_total counter")?;
        writeln!(
            f,
            "tf_guard_procedural_denies_total {}",
            self.guard_procedural_denies_total
        )?;

        Ok(())
    }
}

// ============================================================================
// Embedded HTML Asset
// ============================================================================

/// The embedded Chart.js dashboard HTML, relocated to `assets/dashboard.html` so the frontend
/// agent can own that file without colliding with Rust edits here. Embedded at compile time.
pub const DASHBOARD_HTML: &str = include_str!("../../../assets/dashboard.html");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{temp_dir, ENV_LOCK};

    #[test]
    fn test_endpoint_session_budget_returns_json() {
        let result = endpoint_session_budget();
        assert!(result.is_ok(), "endpoint should return Ok");
        let json = result.unwrap();
        assert!(json.get("session_cap").is_some());
        assert!(json.get("current_spend").is_some());
        assert!(json.get("ceiling_pct").is_some());
    }

    /// H7 behaviour: the endpoint must read the REAL on-disk budget keys
    /// (`session_cap_tokens` / `per_fanout_cap_tokens`, written by `budget set`). Seeding the
    /// real keys and asserting they surface fails against the old `session_cap` reader (which
    /// would silently fall back to the 2_000_000 default), and a seeded spend event drives a
    /// correct ceiling %.
    #[test]
    fn test_endpoint_session_budget_reads_real_keys_and_ceiling() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_dir("dash-budget");
        let events = dir.join("honesty-events.jsonl");
        // One spend event: latest session reading of 250_000 tokens.
        std::fs::write(
            &events,
            r#"{"ts":1000,"session":"S","kind":"spend","by_model":[{"model":"opus","tokens":250000,"cost_usd":1.0}],"total_tokens":250000,"total_cost_usd":1.0}"#,
        )
        .unwrap();
        // budget.json written with the REAL keys (as `tf budget set` does).
        std::fs::write(
            dir.join("budget.json"),
            r#"{"session_cap_tokens":1000000,"per_fanout_cap_tokens":75000}"#,
        )
        .unwrap();

        std::env::set_var("I2P_COST_STATE_DIR", &dir);
        std::env::set_var("I2P_HONESTY_EVENTS", &events);
        let json = endpoint_session_budget().expect("endpoint ok");
        std::env::remove_var("I2P_COST_STATE_DIR");
        std::env::remove_var("I2P_HONESTY_EVENTS");

        assert_eq!(
            json.get("session_cap").unwrap().as_u64().unwrap(),
            1_000_000
        );
        assert_eq!(
            json.get("per_fanout_cap").unwrap().as_u64().unwrap(),
            75_000
        );
        assert_eq!(
            json.get("current_spend").unwrap().as_u64().unwrap(),
            250_000
        );
        // 250_000 / 1_000_000 = 25%
        assert_eq!(json.get("ceiling_pct").unwrap().as_f64().unwrap(), 25.0);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A FRESH snapshot grounds the account-wide windows: `fresh:true`, `blind:false`, and each
    /// window's `used_pct` is the live provider percentage (5h=77, weekly=21) — the real lockout
    /// signal the dashboard previously omitted.
    #[test]
    fn test_endpoint_windows_fresh_grounds_account_wide_used_pct() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_dir("dash-windows-fresh");
        let snap = dir.join("ratelimit-snapshot.json");
        std::fs::write(
            &snap,
            r#"{"captured_at":1900,"rate_limits":{"five_hour":{"used_percentage":77,"resets_at":1900000000},"seven_day":{"used_percentage":21,"resets_at":1900500000}}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("budget.json"),
            r#"{"session_cap_tokens":2000000,"weekly_cap_tokens":20000000,"headroom_pct":15}"#,
        )
        .unwrap();

        std::env::set_var("I2P_COST_STATE_DIR", &dir);
        std::env::set_var("I2P_RATELIMIT_SNAPSHOT", &snap);
        std::env::set_var("I2P_CLOCK", "2000"); // 100s after captured_at ⇒ fresh (< 900).
        let json = endpoint_windows().expect("endpoint ok");
        std::env::remove_var("I2P_COST_STATE_DIR");
        std::env::remove_var("I2P_RATELIMIT_SNAPSHOT");
        std::env::remove_var("I2P_CLOCK");

        assert!(json.get("fresh").unwrap().as_bool().unwrap());
        assert!(!json.get("blind").unwrap().as_bool().unwrap());
        assert_eq!(json.get("captured_at").unwrap().as_i64().unwrap(), 1900);
        assert_eq!(
            json.get("snapshot_age_seconds").unwrap().as_i64().unwrap(),
            100
        );
        assert_eq!(json.get("max_age_seconds").unwrap().as_i64().unwrap(), 900);
        assert_eq!(
            json.pointer("/five_hour/used_pct")
                .unwrap()
                .as_f64()
                .unwrap(),
            77.0
        );
        assert_eq!(
            json.pointer("/seven_day/used_pct")
                .unwrap()
                .as_f64()
                .unwrap(),
            21.0
        );
        // headroom_pct = min over fresh windows of (85 - used): 5h ⇒ 8, weekly ⇒ 64 ⇒ 8.
        assert_eq!(json.get("headroom_pct").unwrap().as_f64().unwrap(), 8.0);
        assert!(json
            .get("note")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("Account-wide"));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A STALE snapshot makes the account-wide signal BLIND: `blind:true` and each window's
    /// `used_pct` is `null` — we never fabricate a percentage we cannot see (mirrors
    /// `windows::live_windows_honours_freshness`).
    #[test]
    fn test_endpoint_windows_stale_is_blind_with_null_used_pct() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_dir("dash-windows-stale");
        let snap = dir.join("ratelimit-snapshot.json");
        std::fs::write(
            &snap,
            r#"{"captured_at":100,"rate_limits":{"five_hour":{"used_percentage":77},"seven_day":{"used_percentage":21}}}"#,
        )
        .unwrap();

        std::env::set_var("I2P_COST_STATE_DIR", &dir);
        std::env::set_var("I2P_RATELIMIT_SNAPSHOT", &snap);
        std::env::set_var("I2P_CLOCK", "2000"); // 1900s after captured_at ⇒ stale (> 900).
        let json = endpoint_windows().expect("endpoint ok");
        std::env::remove_var("I2P_COST_STATE_DIR");
        std::env::remove_var("I2P_RATELIMIT_SNAPSHOT");
        std::env::remove_var("I2P_CLOCK");

        assert!(!json.get("fresh").unwrap().as_bool().unwrap());
        assert!(json.get("blind").unwrap().as_bool().unwrap());
        assert!(json.pointer("/five_hour/used_pct").unwrap().is_null());
        assert!(json.pointer("/seven_day/used_pct").unwrap().is_null());
        assert!(json.get("headroom_pct").unwrap().is_null());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `endpoint_mcp_invocations` reflects the audit log: a `count` of all records and the recent
    /// list (most recent last), capped at 100.
    #[test]
    fn test_endpoint_mcp_invocations_count_and_recent() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_dir("dash-mcp-inv");
        let log = dir.join("mcp-invocations.jsonl");
        // 105 lines ⇒ count=105, recent capped at 100 (last 100, newest last).
        let mut body = String::new();
        for i in 0..105 {
            body.push_str(&format!(
                "{{\"ts\":{},\"kind\":\"mcp\",\"method\":\"tf_budget_read\",\"params\":[],\"ok\":true}}\n",
                i
            ));
        }
        std::fs::write(&log, body).unwrap();

        std::env::set_var("I2P_MCP_INVOCATIONS", &log);
        let json = endpoint_mcp_invocations().expect("endpoint ok");
        std::env::remove_var("I2P_MCP_INVOCATIONS");

        assert_eq!(json.get("count").unwrap().as_u64().unwrap(), 105);
        let recent = json.get("recent").unwrap().as_array().unwrap();
        assert_eq!(recent.len(), 100, "recent capped at 100");
        // Newest last: the final element is ts=104.
        assert_eq!(recent[99].get("ts").unwrap().as_i64().unwrap(), 104);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A missing audit log yields an empty, non-erroring view.
    #[test]
    fn test_endpoint_mcp_invocations_absent_log_is_empty() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_dir("dash-mcp-inv-absent");
        let missing = dir.join("never-written.jsonl"); // dir exists, file does not.
        std::env::set_var("I2P_MCP_INVOCATIONS", &missing);
        let json = endpoint_mcp_invocations().expect("endpoint ok");
        std::env::remove_var("I2P_MCP_INVOCATIONS");
        assert_eq!(json.get("count").unwrap().as_u64().unwrap(), 0);
        assert_eq!(json.get("recent").unwrap().as_array().unwrap().len(), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_endpoint_spend_by_model_returns_array() {
        let result = endpoint_spend_by_model();
        assert!(result.is_ok());
        assert!(result.unwrap().is_array());
    }

    #[test]
    fn test_endpoint_guard_efficacy_returns_metrics() {
        let result = endpoint_guard_efficacy();
        assert!(result.is_ok());
        let json = result.unwrap();
        assert!(json.get("saves_count").is_some());
        assert!(json.get("blown_count").is_some());
    }

    #[test]
    fn test_endpoint_estimator_accuracy_returns_mape() {
        let result = endpoint_estimator_accuracy();
        assert!(result.is_ok());
        let json = result.unwrap();
        assert!(json.get("mean_absolute_percentage_error").is_some());
    }

    #[test]
    fn test_prometheus_metrics_format() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_dir("prom-format");
        std::env::set_var("I2P_COST_STATE_DIR", &dir); // no snapshot ⇒ blind windows.
        let state = FoldState::default();
        let budget_json = json!({ "session_cap_tokens": 2_000_000 });
        let metrics = PrometheusMetrics::from_fold(&state, &budget_json);
        std::env::remove_var("I2P_COST_STATE_DIR");
        let output = metrics.to_string();

        // Verify format includes HELP and TYPE lines
        assert!(output.contains("# HELP"));
        assert!(output.contains("# TYPE"));
        assert!(output.contains("gauge"));
        assert!(output.contains("counter"));
        assert!(output.contains("tf_session_spend_tokens"));
        assert!(output.contains("tf_guard_saves_total"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_prometheus_gauge_vs_counter_types() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_dir("prom-types");
        std::env::set_var("I2P_COST_STATE_DIR", &dir);
        let state = FoldState::default();
        let budget_json = json!({ "session_cap_tokens": 2_000_000 });
        let metrics = PrometheusMetrics::from_fold(&state, &budget_json);
        std::env::remove_var("I2P_COST_STATE_DIR");
        let output = metrics.to_string();

        // Verify spend is gauge (can decrease)
        assert!(output.contains("tf_session_spend_tokens"));
        assert!(output.contains("# TYPE tf_session_spend_tokens gauge"));

        // Verify saves/blown are counters (monotonic)
        assert!(output.contains("# TYPE tf_guard_saves_total counter"));
        assert!(output.contains("# TYPE tf_guard_blown_total counter"));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The faked `tf_weekly_ceiling_percent` is GONE; a fresh snapshot emits the REAL account-wide
    /// window gauges plus the freshness gauge, and the session HELP line is explicit that the
    /// session metric is per-session, not account-wide.
    #[test]
    fn test_prometheus_emits_real_window_gauges_when_fresh() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_dir("prom-windows-fresh");
        let snap = dir.join("ratelimit-snapshot.json");
        std::fs::write(
            &snap,
            r#"{"captured_at":1900,"rate_limits":{"five_hour":{"used_percentage":77},"seven_day":{"used_percentage":21}}}"#,
        )
        .unwrap();
        std::env::set_var("I2P_COST_STATE_DIR", &dir);
        std::env::set_var("I2P_RATELIMIT_SNAPSHOT", &snap);
        std::env::set_var("I2P_CLOCK", "2000");
        let metrics = PrometheusMetrics::from_fold(&FoldState::default(), &json!({}));
        let output = metrics.to_string();
        std::env::remove_var("I2P_COST_STATE_DIR");
        std::env::remove_var("I2P_RATELIMIT_SNAPSHOT");
        std::env::remove_var("I2P_CLOCK");

        // The lie is gone.
        assert!(
            !output.contains("tf_weekly_ceiling_percent"),
            "the faked weekly gauge must be removed"
        );
        // Real account-wide gauges present with the live percentages.
        assert!(output.contains("tf_five_hour_used_percent 77.00"));
        assert!(output.contains("tf_seven_day_used_percent 21.00"));
        assert!(output.contains("tf_window_snapshot_fresh 1"));
        assert!(output.contains("tf_window_snapshot_age_seconds 100"));
        // Honest HELP on the session metric.
        let session_help = output
            .lines()
            .find(|l| l.starts_with("# HELP tf_session_ceiling_percent"))
            .expect("session ceiling HELP present");
        assert!(
            session_help.contains("PER-SESSION") && session_help.contains("NOT account-wide"),
            "session HELP must clarify it is per-session, not account-wide"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// When BLIND (no fresh snapshot) the window VALUE lines are omitted but the freshness gauge
    /// reports 0 — alerting can detect the blind state rather than reading a stale number.
    #[test]
    fn test_prometheus_window_gauges_blind_when_stale() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_dir("prom-windows-blind");
        let snap = dir.join("ratelimit-snapshot.json");
        std::fs::write(
            &snap,
            r#"{"captured_at":100,"rate_limits":{"five_hour":{"used_percentage":77}}}"#,
        )
        .unwrap();
        std::env::set_var("I2P_COST_STATE_DIR", &dir);
        std::env::set_var("I2P_RATELIMIT_SNAPSHOT", &snap);
        std::env::set_var("I2P_CLOCK", "2000"); // 1900s old ⇒ stale.
        let metrics = PrometheusMetrics::from_fold(&FoldState::default(), &json!({}));
        let output = metrics.to_string();
        std::env::remove_var("I2P_COST_STATE_DIR");
        std::env::remove_var("I2P_RATELIMIT_SNAPSHOT");
        std::env::remove_var("I2P_CLOCK");

        // No fabricated value line, but the freshness gauge is explicit.
        assert!(!output.contains("tf_five_hour_used_percent 77"));
        assert!(output.contains("tf_window_snapshot_fresh 0"));
        // HELP/TYPE for the window gauge are still present (metric is declared, just unset).
        assert!(output.contains("# TYPE tf_five_hour_used_percent gauge"));

        std::fs::remove_dir_all(&dir).ok();
    }
}
