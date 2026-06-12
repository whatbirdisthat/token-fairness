//! observe — P-I The Honesty Observatory (doc/design/spend-safety-enforcement.md §P-I).
//!
//! The cross-time efficacy record: how often the guard ACTUALLY prevented an overspend (SAVE),
//! how often we BLEW the limit anyway, and what the guard COST (friction). It must be honest —
//! failures reported as loudly as wins — or it is the vanity dashboard we are replacing.
//!
//! This file is the CAPTURE foundation: an append-only event log (`honesty-events.jsonl`) plus
//! the HONEST classification of a gate decision. The rollup/report renderer lands next.
//! Metadata only — prompt content is NEVER written (HON-5).

use crate::{state, Out};

/// The durable append-only event log — HOME-rooted, so it survives across sessions (transcripts
/// get compacted; this is the system of record).
pub fn events_path() -> String {
    if let Ok(p) = std::env::var("I2P_HONESTY_EVENTS") {
        return p;
    }
    format!("{}/honesty-events.jsonl", state::state_dir())
}

fn session_id() -> String {
    let p = format!("{}/session.json", state::state_dir());
    state::read_json(&p)
        .and_then(|v| {
            v.get("session_id")
                .and_then(|x| x.as_str())
                .map(String::from)
        })
        .unwrap_or_default()
}

/// HONEST classification of a gate decision (review §1 — no vanity SAVES). A **save** is ONLY a
/// deny that prevented a genuine overspend (a cap/ceiling breach). A no-budget / not-armed refusal
/// is **procedural** — real, but not a headline win. Non-denies are **allow**.
pub fn classify_gate(decision: &str, reason: &str) -> &'static str {
    if decision != "deny" {
        return "allow";
    }
    if reason.contains("exceeds-per-fanout-cap")
        || reason.contains("exceeds-session-cap")
        || reason.contains("session token cap")
        || reason.contains("ceiling")
    {
        "save"
    } else {
        "procedural-deny"
    }
}

/// Append one gate event. Best-effort; metadata only.
pub fn log_gate(tool: &str, decision: &str, reason: &str, est: i64) {
    let ev = serde_json::json!({
        "ts": state::now_epoch(),
        "session": session_id(),
        "kind": "gate",
        "class": classify_gate(decision, reason),
        "tool": tool,
        "decision": decision,
        "reason": reason,
        "est": est,
    });
    let _ = state::append_line(&events_path(), &ev.to_string());
}

/// `tf observe` — for now, a faithful tally of the captured log (the rollup renderer lands next).
pub fn dispatch(argv: &[String]) -> Out {
    let _ = argv;
    let body = std::fs::read_to_string(events_path()).unwrap_or_default();
    let (mut saves, mut procedural, mut allows, mut blown, mut other) = (0, 0, 0, 0, 0);
    for line in body.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match v.get("kind").and_then(|x| x.as_str()) {
            Some("gate") => match v.get("class").and_then(|x| x.as_str()) {
                Some("save") => saves += 1,
                Some("procedural-deny") => procedural += 1,
                Some("allow") => allows += 1,
                _ => other += 1,
            },
            Some("blown") => blown += 1,
            _ => other += 1,
        }
    }
    // BLOWN beside SAVES, equal prominence (HON-1).
    Out::ok(format!(
        "{{\"saves\":{},\"blown\":{},\"procedural_denies\":{},\"fanout_allows\":{},\"other\":{}}}\n",
        saves, blown, procedural, allows, other
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_only_for_genuine_overspend() {
        // The honest line: a real cap/ceiling breach is a SAVE…
        assert_eq!(
            classify_gate("deny", "fan-out budget refused: exceeds-per-fanout-cap"),
            "save"
        );
        assert_eq!(
            classify_gate("deny", "session token cap reached (2000000/2000000)."),
            "save"
        );
        // …but a no-budget / not-armed refusal is procedural, NOT a headline win.
        assert_eq!(
            classify_gate(
                "deny",
                "fan-out has no declared budget — run `tf budget arm <est>` first"
            ),
            "procedural-deny"
        );
        // allows are allows.
        assert_eq!(classify_gate("allow", "armed"), "allow");
    }
}
