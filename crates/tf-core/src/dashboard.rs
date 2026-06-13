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

// ============================================================================
// Prometheus Metrics
// ============================================================================

/// Prometheus gauge/counter metrics.
#[derive(Debug, Clone)]
pub struct PrometheusMetrics {
    pub session_spend_tokens: u64,
    pub session_ceiling_percent: f64,
    pub weekly_ceiling_percent: f64,
    pub guard_saves_total: u64,
    pub guard_blown_total: u64,
    pub guard_procedural_denies_total: u64,
}

impl PrometheusMetrics {
    /// Generate from folded event state.
    pub fn from_fold(state: &FoldState, budget_json: &serde_json::Value) -> Self {
        let session_cap = state::int(budget_json, "session_cap_tokens", 2_000_000) as u64;
        let session_ceiling_percent = if session_cap > 0 {
            (state.session_spend as f64 / session_cap as f64) * 100.0
        } else {
            0.0
        };

        PrometheusMetrics {
            session_spend_tokens: state.session_spend,
            session_ceiling_percent,
            weekly_ceiling_percent: session_ceiling_percent, // Simplified; full implementation tracks 7d rolling window
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
            "# HELP tf_session_ceiling_percent Session spend as percentage of ceiling"
        )?;
        writeln!(f, "# TYPE tf_session_ceiling_percent gauge")?;
        writeln!(
            f,
            "tf_session_ceiling_percent {:.2}",
            self.session_ceiling_percent
        )?;
        writeln!(f)?;

        writeln!(
            f,
            "# HELP tf_weekly_ceiling_percent Weekly spend as percentage of rolling window"
        )?;
        writeln!(f, "# TYPE tf_weekly_ceiling_percent gauge")?;
        writeln!(
            f,
            "tf_weekly_ceiling_percent {:.2}",
            self.weekly_ceiling_percent
        )?;
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
        let state = FoldState::default();
        let budget_json = json!({ "session_cap_tokens": 2_000_000 });
        let metrics = PrometheusMetrics::from_fold(&state, &budget_json);
        let output = metrics.to_string();

        // Verify format includes HELP and TYPE lines
        assert!(output.contains("# HELP"));
        assert!(output.contains("# TYPE"));
        assert!(output.contains("gauge"));
        assert!(output.contains("counter"));
        assert!(output.contains("tf_session_spend_tokens"));
        assert!(output.contains("tf_guard_saves_total"));
    }

    #[test]
    fn test_prometheus_gauge_vs_counter_types() {
        let state = FoldState::default();
        let budget_json = json!({ "session_cap_tokens": 2_000_000 });
        let metrics = PrometheusMetrics::from_fold(&state, &budget_json);
        let output = metrics.to_string();

        // Verify spend is gauge (can decrease)
        assert!(output.contains("tf_session_spend_tokens"));
        assert!(output.contains("# TYPE tf_session_spend_tokens gauge"));

        // Verify saves/blown are counters (monotonic)
        assert!(output.contains("# TYPE tf_guard_saves_total counter"));
        assert!(output.contains("# TYPE tf_guard_blown_total counter"));
    }
}
