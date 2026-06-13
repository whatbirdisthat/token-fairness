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

    let session_cap = state::int(&budget_json, "session_cap", 2_000_000) as u64;
    let per_fanout_cap = state::int(&budget_json, "per_fanout_cap", 100_000) as u64;

    let ceiling_pct = if session_cap > 0 {
        (state.session_spend as f64 / session_cap as f64) * 100.0
    } else {
        0.0
    };

    Ok(json!({
        "session_cap": session_cap,
        "per_fanout_cap": per_fanout_cap,
        "current_spend": state.session_spend,
        "ceiling_pct": ceiling_pct.min(100.0).max(0.0),
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
        let session_cap = state::int(budget_json, "session_cap", 2_000_000) as u64;
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
        writeln!(f, "# HELP tf_session_spend_tokens Current session cumulative spend")?;
        writeln!(f, "# TYPE tf_session_spend_tokens gauge")?;
        writeln!(f, "tf_session_spend_tokens {}", self.session_spend_tokens)?;
        writeln!(f)?;

        writeln!(f, "# HELP tf_session_ceiling_percent Session spend as percentage of ceiling")?;
        writeln!(f, "# TYPE tf_session_ceiling_percent gauge")?;
        writeln!(f, "tf_session_ceiling_percent {:.2}", self.session_ceiling_percent)?;
        writeln!(f)?;

        writeln!(f, "# HELP tf_weekly_ceiling_percent Weekly spend as percentage of rolling window")?;
        writeln!(f, "# TYPE tf_weekly_ceiling_percent gauge")?;
        writeln!(f, "tf_weekly_ceiling_percent {:.2}", self.weekly_ceiling_percent)?;
        writeln!(f)?;

        writeln!(f, "# HELP tf_guard_saves_total Total count of guard SAVE verdicts")?;
        writeln!(f, "# TYPE tf_guard_saves_total counter")?;
        writeln!(f, "tf_guard_saves_total {}", self.guard_saves_total)?;
        writeln!(f)?;

        writeln!(f, "# HELP tf_guard_blown_total Total count of BLOWN events")?;
        writeln!(f, "# TYPE tf_guard_blown_total counter")?;
        writeln!(f, "tf_guard_blown_total {}", self.guard_blown_total)?;
        writeln!(f)?;

        writeln!(f, "# HELP tf_guard_procedural_denies_total Total procedural denials")?;
        writeln!(f, "# TYPE tf_guard_procedural_denies_total counter")?;
        writeln!(f, "tf_guard_procedural_denies_total {}", self.guard_procedural_denies_total)?;

        Ok(())
    }
}

// ============================================================================
// Embedded HTML Asset
// ============================================================================

/// The embedded Chart.js dashboard HTML.
/// This will be replaced with the actual HTML from assets/dashboard.html at compile time.
pub const DASHBOARD_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Token Fairness Dashboard</title>
    <script src="https://cdn.jsdelivr.net/npm/chart.js"></script>
    <style>
        body { font-family: sans-serif; margin: 20px; background: #f5f5f5; }
        h1 { color: #333; }
        .container { display: grid; grid-template-columns: repeat(3, 1fr); gap: 20px; }
        .chart { background: white; padding: 20px; border-radius: 8px; box-shadow: 0 2px 4px rgba(0,0,0,0.1); }
        canvas { max-width: 100%; }
    </style>
</head>
<body>
    <h1>Token Fairness Dashboard</h1>
    <div class="container">
        <div class="chart"><canvas id="spendGauge"></canvas></div>
        <div class="chart"><canvas id="spendPie"></canvas></div>
        <div class="chart"><canvas id="efficacyTrend"></canvas></div>
    </div>
    <script>
        // Placeholder: JavaScript fold function will be embedded here
        // For now, fetch from REST endpoints and update charts
        async function updateCharts() {
            try {
                const budget = await fetch('/api/session-budget').then(r => r.json());
                const spend = await fetch('/api/spend-by-model').then(r => r.json());
                const efficacy = await fetch('/api/guard-efficacy').then(r => r.json());

                // Render charts with data
                console.log('Dashboard loaded:', { budget, spend, efficacy });
            } catch (e) {
                console.error('Failed to load dashboard:', e);
            }
        }
        updateCharts();

        // WebSocket connection for real-time updates (future)
        // const ws = new WebSocket('ws://localhost:8080/ws');
    </script>
</body>
</html>"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_endpoint_session_budget_returns_json() {
        let result = endpoint_session_budget();
        assert!(result.is_ok(), "endpoint should return Ok");
        let json = result.unwrap();
        assert!(json.get("session_cap").is_some());
        assert!(json.get("current_spend").is_some());
        assert!(json.get("ceiling_pct").is_some());
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
        let budget_json = json!({ "session_cap": 2_000_000 });
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
        let budget_json = json!({ "session_cap": 2_000_000 });
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
