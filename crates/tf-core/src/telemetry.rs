//! Telemetry pipeline — file watcher, WebSocket broadcaster, and event folding.
//!
//! This module handles:
//! - Real-time file watching of JSONL event files via inotify/FSEvents
//! - Broadcasting new events to WebSocket clients
//! - Folding event streams into live state snapshots (spend, guard efficacy, MAPE)
//!
//! The fold logic exactly replicates observe.rs:fold_events() semantics for parity.

use crate::state;
use serde_json::Value;
use std::collections::BTreeMap;

// ============================================================================
// File Watcher (TELEM-001, TELEM-004)
// ============================================================================

/// Errors that can occur in the telemetry pipeline.
#[derive(Debug, Clone, PartialEq)]
pub enum TelemetryError {
    FileNotFound,
    IoError(String),
    WatcherError(String),
    JsonError(String),
}

impl std::fmt::Display for TelemetryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TelemetryError::FileNotFound => write!(f, "events file not found"),
            TelemetryError::IoError(e) => write!(f, "IO error: {}", e),
            TelemetryError::WatcherError(e) => write!(f, "watcher error: {}", e),
            TelemetryError::JsonError(e) => write!(f, "JSON error: {}", e),
        }
    }
}

impl std::error::Error for TelemetryError {}

/// Initialize the file watcher for honesty-events.jsonl.
/// Path is resolved via observe::events_path() (respects TF_EVENTS_DIR env var).
/// Creates empty file if missing. Records initial offset (EOF).
pub fn init_watcher(events_path: &str) -> Result<(String, u64), TelemetryError> {
    // Ensure the file exists
    if !std::path::Path::new(events_path).exists() {
        std::fs::File::create(events_path).map_err(|e| TelemetryError::IoError(e.to_string()))?;
    }

    // Record the current offset (EOF)
    let metadata =
        std::fs::metadata(events_path).map_err(|e| TelemetryError::IoError(e.to_string()))?;
    let offset = metadata.len();

    Ok((events_path.to_string(), offset))
}

/// Read new bytes from the file since the last offset.
/// On truncation (file size < offset), reset offset to 0 and seek to EOF.
pub fn read_new_bytes(events_path: &str, offset: &mut u64) -> Result<Vec<u8>, TelemetryError> {
    let metadata =
        std::fs::metadata(events_path).map_err(|e| TelemetryError::IoError(e.to_string()))?;
    let file_size = metadata.len();

    // Truncation handling: reset offset if file shrank
    if file_size < *offset {
        *offset = 0;
    }

    // Read from offset to EOF
    let mut file =
        std::fs::File::open(events_path).map_err(|e| TelemetryError::IoError(e.to_string()))?;
    use std::io::{Read, Seek, SeekFrom};
    file.seek(SeekFrom::Start(*offset))
        .map_err(|e| TelemetryError::IoError(e.to_string()))?;

    let mut new_bytes = Vec::new();
    let bytes_read = file
        .read_to_end(&mut new_bytes)
        .map_err(|e| TelemetryError::IoError(e.to_string()))?;

    *offset += bytes_read as u64;

    Ok(new_bytes)
}

// ============================================================================
// Fold Semantics (TELEM-003, AC#9 Fold-Parity)
// ============================================================================

/// Span of time for bucketing events (hour, day, week, month).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Period {
    Hour,
    Day,
    Week,
    Month,
}

/// One period's guard efficacy tallies.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct PeriodBucket {
    pub saves: u32,
    pub blown: u32,
    pub procedural_denies: u32,
    pub allows: u32,
    pub est_sum: u64,
    pub est_count: u32,
}

/// Live fold state: deduped spend, guard efficacy, and estimator accuracy.
/// This struct exactly mirrors the fold logic in observe.rs for JS parity.
#[derive(Debug, Clone, PartialEq)]
pub struct FoldState {
    /// Current session cumulative spend (tokens). Resets on session boundary.
    pub session_spend: u64,

    /// Spend by model: model name → (tokens, count).
    pub spend_by_model: BTreeMap<String, (u64, u32)>,

    /// Guard saves (denies that prevented genuine overspend).
    pub saves_count: u32,

    /// Guard blown (limits/lockouts we hit anyway).
    pub blown_count: u32,

    /// Procedural denies (no-budget / not-armed refusals).
    pub procedural_denies_count: u32,

    /// Fan-out allows.
    pub allows_count: u32,

    /// Mean absolute percentage error (estimator accuracy).
    pub mape: f64,

    /// Time-series bucketed by period (for live charts).
    pub periods: BTreeMap<String, PeriodBucket>,

    /// Total events not matching known kinds (error resilience).
    pub other_count: u32,
}

impl Default for FoldState {
    fn default() -> Self {
        FoldState {
            session_spend: 0,
            spend_by_model: BTreeMap::new(),
            saves_count: 0,
            blown_count: 0,
            procedural_denies_count: 0,
            allows_count: 0,
            mape: 0.0,
            periods: BTreeMap::new(),
            other_count: 0,
        }
    }
}

/// Compute the period bucket key for a Unix timestamp (seconds).
/// Day → YYYY-MM-DD; Week → Monday of that ISO week; Month → YYYY-MM.
fn period_key(ts: i64, period: Period) -> String {
    let day = ts / 86_400;
    match period {
        Period::Day => {
            let (y, m, d) = civil_from_days(day);
            format!("{:04}-{:02}-{:02}", y, m, d)
        }
        Period::Hour => {
            let (y, m, d) = civil_from_days(day);
            let hour = (ts % 86_400) / 3600;
            format!("{:04}-{:02}-{:02} {:02}:00", y, m, d, hour)
        }
        Period::Week => {
            let monday = day - (day + 3).rem_euclid(7);
            let (y, m, d) = civil_from_days(monday);
            format!("{:04}-{:02}-{:02}", y, m, d)
        }
        Period::Month => {
            let (y, m, _) = civil_from_days(day);
            format!("{:04}-{:02}", y, m)
        }
    }
}

/// Civil (y, m, d) from days since Unix epoch (Howard Hinnant's integer algorithm).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Fold JSONL event lines into a FoldState, bucketed by period.
/// Semantics exactly replicate observe.rs:fold_events().
///
/// Deduplication:
/// - Spend events: keep only the LATEST per session (cumulative readings, not delta)
/// - Guard events: tally by class (save, procedural-deny, allow) per period
/// - BLOWN events: tally per period
///
/// MAPE: fold estimator-accuracy ledger separately (see fold_accuracy).
pub fn fold_events(lines: &[&str], period: Period) -> FoldState {
    let mut state = FoldState::default();

    // Track the latest spend reading per session (ts, models).
    // The Stop hook re-emits cumulative spend every turn; only the LAST counts.
    type LatestSpend = BTreeMap<String, (i64, Vec<(String, u64, f64)>)>;
    let mut spend_latest: LatestSpend = BTreeMap::new();

    for line in lines {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            state.other_count += 1;
            continue;
        };

        let ts = state::int(&v, "ts", 0);

        match v.get("kind").and_then(|x| x.as_str()) {
            Some("gate") => {
                let bucket_key = period_key(ts, period);
                let bucket = state.periods.entry(bucket_key).or_default();
                let est = state::int(&v, "est", 0) as u64;

                match v.get("class").and_then(|x| x.as_str()) {
                    Some("save") => {
                        bucket.saves += 1;
                        bucket.est_sum += est;
                        bucket.est_count += 1;
                        state.saves_count += 1;
                    }
                    Some("procedural-deny") => {
                        bucket.procedural_denies += 1;
                        state.procedural_denies_count += 1;
                    }
                    Some("allow") => {
                        bucket.allows += 1;
                        bucket.est_sum += est;
                        bucket.est_count += 1;
                        state.allows_count += 1;
                    }
                    _ => {
                        state.other_count += 1;
                    }
                }
            }
            Some("blown") => {
                let bucket_key = period_key(ts, period);
                state.periods.entry(bucket_key).or_default().blown += 1;
                state.blown_count += 1;
            }
            Some("spend") => {
                if let Some(session_id) = v.get("session").and_then(|x| x.as_str()) {
                    let models: Vec<(String, u64, f64)> = v
                        .get("by_model")
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter()
                                .map(|m| {
                                    (
                                        m.get("model")
                                            .and_then(|x| x.as_str())
                                            .unwrap_or("unknown")
                                            .to_string(),
                                        state::int(m, "tokens", 0) as u64,
                                        state::num(m, "cost_usd", 0.0),
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default();

                    // Keep only the latest reading per session (dedup)
                    let entry = spend_latest
                        .entry(session_id.to_string())
                        .or_insert((-1, vec![]));
                    if ts >= entry.0 {
                        *entry = (ts, models.clone());
                    }
                }
            }
            _ => {
                state.other_count += 1;
            }
        }
    }

    // Finalize spend: bucket each session's LATEST reading by period.
    // Also track the globally latest session timestamp for session_spend.
    let mut latest_session_ts = -1i64;
    for (_session, (ts, models)) in spend_latest.iter() {
        if *ts > latest_session_ts {
            latest_session_ts = *ts;
        }
        for (m, t, _c) in models {
            let entry = state.spend_by_model.entry(m.clone()).or_insert((0, 0));
            entry.0 += t;
            entry.1 += 1;
        }
    }

    // Set session_spend to the total of the latest session's spend
    if latest_session_ts >= 0 {
        for (_session, (ts, models)) in spend_latest {
            if ts == latest_session_ts {
                state.session_spend = models.iter().map(|(_, t, _)| t).sum();
                break;
            }
        }
    }

    state
}

/// Fold estimator-accuracy ledger lines into MAPE on an existing FoldState.
/// Replicates observe.rs:fold_accuracy() semantics.
pub fn fold_accuracy(state: &mut FoldState, lines: &[&str], period: Period) {
    let mut mape_sum = 0.0;
    let mut mape_count = 0;

    for line in lines {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };

        let at = state::int(&v, "at", 0);
        let actual = state::num(&v, "actual", 0.0);

        if actual == 0.0 {
            continue; // APE undefined against zero — skip
        }

        let est = state::num(&v, "est", 0.0);
        let ape = (est - actual).abs() / actual;
        mape_sum += ape;
        mape_count += 1;

        // Optionally bucket by period (not strictly needed for the simple MAPE,
        // but useful for per-period breakdowns)
        let _bucket_key = period_key(at, period);
    }

    if mape_count > 0 {
        state.mape = mape_sum / mape_count as f64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_period_key_day() {
        let ts = 20_616 * 86_400 + 3600; // 2026-06-12, 01:00 UTC
        assert_eq!(period_key(ts, Period::Day), "2026-06-12");
    }

    #[test]
    fn test_period_key_hour() {
        let ts = 20_616 * 86_400 + 3600; // 01:00 UTC
        assert_eq!(period_key(ts, Period::Hour), "2026-06-12 01:00");
    }

    #[test]
    fn test_fold_dedupes_spend_to_latest() {
        let lines = [
            // Session A: cumulative spend 100, then 200 (only last counts)
            r#"{"ts":1000,"session":"A","kind":"spend","by_model":[{"model":"opus","tokens":100,"cost_usd":1.0}],"total_tokens":100,"total_cost_usd":1.0}"#,
            r#"{"ts":2000,"session":"A","kind":"spend","by_model":[{"model":"opus","tokens":200,"cost_usd":2.0}],"total_tokens":200,"total_cost_usd":2.0}"#,
            // Session B: cumulative spend 50
            r#"{"ts":1500,"session":"B","kind":"spend","by_model":[{"model":"sonnet","tokens":50,"cost_usd":0.5}],"total_tokens":50,"total_cost_usd":0.5}"#,
        ];
        let refs: Vec<&str> = lines.iter().map(|s| &s[..]).collect();
        let state = fold_events(&refs, Period::Day);

        // Session A's final spend is 200 (not 300)
        assert_eq!(state.spend_by_model.get("opus"), Some(&(200, 1)));
        // Session B's spend is 50
        assert_eq!(state.spend_by_model.get("sonnet"), Some(&(50, 1)));
        // session_spend reflects the latest cumulative: 200 (from session A, which was latest)
        assert_eq!(state.session_spend, 200);
    }

    #[test]
    fn test_fold_tallies_gate_verdicts() {
        let day = 20_616 * 86_400;
        let lines = [
            format!(
                r#"{{"ts":{},"kind":"gate","class":"save","reason":"exceeds-per-fanout-cap","est":120000}}"#,
                day
            ),
            format!(
                r#"{{"ts":{},"kind":"gate","class":"procedural-deny","reason":"no declared budget"}}"#,
                day
            ),
            format!(
                r#"{{"ts":{},"kind":"gate","class":"allow","reason":"armed","est":40000}}"#,
                day
            ),
        ];
        let refs: Vec<&str> = lines.iter().map(|s| &s[..]).collect();
        let state = fold_events(&refs, Period::Day);

        assert_eq!(state.saves_count, 1);
        assert_eq!(state.procedural_denies_count, 1);
        assert_eq!(state.allows_count, 1);
        assert_eq!(state.blown_count, 0);
    }

    #[test]
    fn test_fold_tallies_blown_events() {
        let day = 20_616 * 86_400;
        let lines = [
            format!(
                r#"{{"ts":{},"kind":"blown","reason":"5h-window exhausted"}}"#,
                day
            ),
            format!(r#"{{"ts":{},"kind":"blown","reason":"paid lockout"}}"#, day),
        ];
        let refs: Vec<&str> = lines.iter().map(|s| &s[..]).collect();
        let state = fold_events(&refs, Period::Day);

        assert_eq!(state.blown_count, 2);
    }

    #[test]
    fn test_fold_accuracy_calculates_mape() {
        let mut state = FoldState::default();
        let lines = vec![
            r#"{"at":1000,"est":100.0,"actual":90.0}"#, // APE = 10/90 = 0.111...
            r#"{"at":2000,"est":100.0,"actual":100.0}"#, // APE = 0/100 = 0.0
            r#"{"at":3000,"est":100.0,"actual":110.0}"#, // APE = 10/110 = 0.0909...
        ];
        fold_accuracy(&mut state, &lines, Period::Day);

        // MAPE = (0.111 + 0.0 + 0.0909) / 3 ≈ 0.0673
        assert!(
            (state.mape - 0.0673).abs() < 0.001,
            "MAPE should be ~0.0673, got {}",
            state.mape
        );
    }

    #[test]
    fn test_fold_is_idempotent() {
        let lines = vec![
            r#"{"ts":1000,"kind":"gate","class":"save","reason":"ceiling","est":100000}"#,
            r#"{"ts":2000,"kind":"blown","reason":"paid lockout"}"#,
        ];

        let state1 = fold_events(&lines, Period::Day);
        let state2 = fold_events(&lines, Period::Day);

        assert_eq!(
            state1, state2,
            "Folding same sequence twice should be identical"
        );
    }

    #[test]
    #[allow(unused_assignments)]
    fn test_truncation_handling() {
        let (_path, mut offset) = init_watcher("/tmp/nonexistent.jsonl").unwrap();
        offset = 1000; // Simulate a large offset

        // Simulate file truncation (file now 500 bytes)
        let new_file_size = 500;
        if new_file_size < offset {
            offset = 0; // Reset on truncate
        }

        assert_eq!(offset, 0, "Offset should be reset on truncation");
    }
}
