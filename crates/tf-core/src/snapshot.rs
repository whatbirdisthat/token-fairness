//! snapshot — the LIVE→disk BRIDGE. Port of `ratelimit-snapshot.sh`.
//!
//! Run as a hook on whatever event carries the live `.rate_limits` signal. Mirrors the
//! latest reading to a freshness-stamped `ratelimit-snapshot.json` the dispatcher can read
//! mid-turn, and forward-updates `signal-findings.json` to record that this event delivered
//! the signal. No-op (exit 0) when the payload carries no rate_limits.

use crate::{state, Out};
use serde_json::{json, Value};

pub fn dispatch(payload: &str) -> Out {
    if payload.trim().is_empty() {
        return Out::default();
    }
    let v: Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(_) => return Out::default(),
    };

    // Only act when a real five-hour or seven-day percentage is present.
    let has_signal = v
        .pointer("/rate_limits/five_hour/used_percentage")
        .filter(|x| !x.is_null())
        .or_else(|| {
            v.pointer("/rate_limits/seven_day/used_percentage")
                .filter(|x| !x.is_null())
        });
    if has_signal.is_none() {
        return Out::default();
    }

    let dir = state::state_dir();
    let now = state::now_epoch();

    let rate_limits = v.get("rate_limits").cloned().unwrap_or_else(|| json!({}));
    let cost = v.get("cost").cloned().unwrap_or_else(|| json!({}));
    let snap = json!({ "captured_at": now, "rate_limits": rate_limits, "cost": cost });
    let snap_path = format!("{}/ratelimit-snapshot.json", dir);
    let _ = state::write_atomic(
        &snap_path,
        &(serde_json::to_string(&snap).unwrap_or_default() + "\n"),
    );

    // Forward-compat self-update of the standing verdict: this event proved it delivers .rate_limits.
    let evt = v
        .get("hook_event_name")
        .or_else(|| v.get("hookEventName"))
        .and_then(|x| x.as_str())
        .unwrap_or("unknown")
        .to_string();
    let findings_path = format!("{}/signal-findings.json", dir);
    let mut findings = state::read_json(&findings_path).unwrap_or_else(|| json!({ "events": {} }));
    if !findings.is_object() {
        findings = json!({ "events": {} });
    }
    findings["verdict"] = json!("hook-signal-available");
    findings["guard_mode"] = json!("live-ceiling");
    findings["concluded_at"] = json!(now);
    if !findings
        .get("events")
        .map(|e| e.is_object())
        .unwrap_or(false)
    {
        findings["events"] = json!({});
    }
    let events = findings["events"].as_object_mut().unwrap();
    let entry = events
        .entry(evt)
        .or_insert_with(|| json!({ "fires": 0, "with_rate_limits": 0 }));
    entry["present"] = json!(true);

    // BLOWN detection (P-I, HON-1): a window AT/OVER 100% is a genuine lockout — the guard FAILED
    // to prevent it. Honesty demands we record it as loudly as a SAVE. Dedup by the window's
    // resets_at so one lockout EPISODE logs exactly one BLOWN, not one per hook fire.
    let blown = ["five_hour", "seven_day"].iter().find_map(|w| {
        match v
            .pointer(&format!("/rate_limits/{}/used_percentage", w))
            .and_then(|x| x.as_f64())
        {
            Some(p) if p >= 100.0 => {
                let reset = v
                    .pointer(&format!("/rate_limits/{}/resets_at", w))
                    .map(|x| x.to_string())
                    .unwrap_or_default();
                Some((*w, reset))
            }
            _ => None,
        }
    });
    if let Some((w, reset)) = blown {
        let marker = format!("{}@{}", w, reset);
        if findings.get("last_blown").and_then(|x| x.as_str()) != Some(marker.as_str()) {
            crate::observe::log_blown(&format!("rate-limit {} window exhausted (100%)", w));
            findings["last_blown"] = json!(marker);
        }
    }

    let _ = state::write_atomic(
        &findings_path,
        &(serde_json::to_string(&findings).unwrap_or_default() + "\n"),
    );

    Out::default()
}
