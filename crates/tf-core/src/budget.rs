//! budget — CORE-A of spend-safety enforcement (doc/design/spend-safety-enforcement.md).
//!
//! A self-tracked token budget that needs NO live harness signal (there is none — `tf gate`
//! returns `no-live-signal` and `session.json` is only written at Stop). It answers ONE
//! question for the blocking fan-out hook (CORE-B): "is this fan-out allowed to run?".
//!
//! Two ceilings, both configurable, both deny-by-refusal (INV-1, INV-2):
//!   - per_fanout_cap — a single fan-out may not exceed this without an explicit `+Xk` consent
//!     that RAISES the declared estimate; a fan-out that declares NO budget is refused outright.
//!   - session_cap   — cumulative spend since the last `tf budget set --reset` baseline may not
//!     pass this. `spent_since_baseline = max(0, session.json.tokens - baseline_tokens)`.
//!
//! `decide` is a PURE function (no IO) so it is exhaustively unit-tested; the dispatch wrappers
//! are the thin IO layer over `state.rs`.

use crate::{state, Out};

/// Conservative starting defaults (tokens). The user tunes these with `tf budget set`; they are
/// deliberately finite so an unconfigured environment still refuses a runaway fan-out.
pub const DEFAULT_SESSION_CAP: i64 = 2_000_000;
pub const DEFAULT_PER_FANOUT_CAP: i64 = 150_000;
/// Exit code for a DENY verdict (matches the scheduler's 0/3 confidence-branch convention).
pub const DENY_CODE: i32 = 3;

fn budget_file() -> String {
    format!("{}/budget.json", state::state_dir())
}

fn session_file() -> String {
    format!("{}/session.json", state::state_dir())
}

/// Cumulative session tokens written by the Stop hook (`session-tokens.sh`); 0 if absent.
fn session_tokens() -> i64 {
    state::read_json(&session_file())
        .map(|v| state::int(&v, "tokens", 0))
        .unwrap_or(0)
}

/// (session_cap, per_fanout_cap, baseline_tokens) from budget.json, else the defaults.
fn load_cfg() -> (i64, i64, i64) {
    match state::read_json(&budget_file()) {
        Some(v) => (
            state::int(&v, "session_cap_tokens", DEFAULT_SESSION_CAP),
            state::int(&v, "per_fanout_cap_tokens", DEFAULT_PER_FANOUT_CAP),
            state::int(&v, "baseline_tokens", 0),
        ),
        None => (DEFAULT_SESSION_CAP, DEFAULT_PER_FANOUT_CAP, 0),
    }
}

/// THE enforcement decision — pure. `est` is the fan-out's declared token estimate (its `+Xk`).
/// Returns (allowed, reason). A non-positive `est` means "no budget declared" → refused (INV-1):
/// the whole failure was a fan-out launched with no `budget.total`.
pub fn decide(
    session_cap: i64,
    per_fanout_cap: i64,
    spent_since_baseline: i64,
    est: i64,
) -> (bool, &'static str) {
    if est <= 0 {
        return (false, "no-budget-declared");
    }
    if est > per_fanout_cap {
        return (false, "exceeds-per-fanout-cap");
    }
    if spent_since_baseline.saturating_add(est) > session_cap {
        return (false, "exceeds-session-cap");
    }
    (true, "ok")
}

fn spent_since(baseline: i64) -> i64 {
    (session_tokens() - baseline).max(0)
}

/// One-shot record that a fan-out's budget has been declared+approved (its `+Xk`). The blocking
/// preflight REQUIRES this for a `Workflow` launch — a Workflow with no arm is the incident.
fn arm_file() -> String {
    format!("{}/fanout-arm.json", state::state_dir())
}

fn read_arm() -> Option<i64> {
    state::read_json(&arm_file()).map(|v| state::int(&v, "est", 0))
}

/// PURE: the blocking-hook decision for a PreToolUse on a spawn tool. None = allow, Some = deny
/// reason. `Workflow` is always a fan-out → it must be ARMED within caps; `Agent`/`Task` are
/// allowed until the session cap is reached (so ordinary single-agent work is never bricked).
pub fn gate_spend(
    tool: &str,
    armed: Option<i64>,
    session_cap: i64,
    per_fanout_cap: i64,
    spent: i64,
) -> Option<String> {
    match tool {
        "Workflow" => match armed {
            None => Some(
                "fan-out has no declared budget — run `tf budget arm <est>` (your +Xk) first"
                    .into(),
            ),
            Some(est) => match decide(session_cap, per_fanout_cap, spent, est) {
                (true, _) => None,
                (false, reason) => Some(format!("fan-out budget refused: {}", reason)),
            },
        },
        "Agent" | "Task" => {
            if spent >= session_cap {
                Some(format!(
                    "session token cap reached ({}/{}). Raise with `tf budget set --session-cap` or `--reset`.",
                    spent, session_cap
                ))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// `tf preflight-spend` — the PreToolUse blocking-hook body. Emits the SAME deny contract the
/// ceiling gate uses (`hookSpecificOutput.permissionDecision=deny`), else allows silently.
pub fn preflight_spend(payload: &str) -> Out {
    if payload.trim().is_empty() {
        return Out::default();
    }
    let v: serde_json::Value = serde_json::from_str(payload).unwrap_or(serde_json::Value::Null);
    let tool = v.get("tool_name").and_then(|x| x.as_str()).unwrap_or("");
    let (session_cap, per_fanout_cap, baseline) = load_cfg();
    let spent = spent_since(baseline);
    let est = read_arm().unwrap_or(0);
    if let Some(reason) = gate_spend(tool, read_arm(), session_cap, per_fanout_cap, spent) {
        // Capture the deny for the Honesty Observatory (P-I) — SAVES + procedural denies + friction.
        crate::observe::log_gate(tool, "deny", &reason, est);
        let deny = serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason
            }
        });
        return Out::ok(serde_json::to_string(&deny).unwrap_or_default() + "\n");
    }
    // Log only fan-out (Workflow) launches; ordinary single-agent allows would be noise.
    if tool == "Workflow" {
        crate::observe::log_gate(tool, "allow", "armed", est);
    }
    Out::default()
}

/// Minimal flag reader: `--key value` / `--key=value`; presence-only flags via `has`.
fn flag<'a>(argv: &'a [String], key: &str) -> Option<&'a str> {
    let pfx = format!("--{}", key);
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        if let Some(v) = a.strip_prefix(&format!("{}=", pfx)) {
            return Some(v);
        }
        if a == &pfx && i + 1 < argv.len() && !argv[i + 1].starts_with("--") {
            return Some(&argv[i + 1]);
        }
        i += 1;
    }
    None
}
fn has(argv: &[String], key: &str) -> bool {
    let pfx = format!("--{}", key);
    argv.iter()
        .any(|a| a == &pfx || a.starts_with(&format!("{}=", pfx)))
}

pub fn dispatch(argv: &[String]) -> Out {
    let sub = argv.first().map(|s| s.as_str()).unwrap_or("status");
    let rest = if argv.is_empty() { &[][..] } else { &argv[1..] };

    match sub {
        // tf budget set [--session-cap N] [--per-fanout-cap N] [--reset]
        "set" => {
            let (cur_session, cur_fanout, cur_baseline) = load_cfg();
            let session_cap = flag(rest, "session-cap")
                .map(|s| state::digits_or(s, cur_session))
                .unwrap_or(cur_session);
            let per_fanout_cap = flag(rest, "per-fanout-cap")
                .map(|s| state::digits_or(s, cur_fanout))
                .unwrap_or(cur_fanout);
            // --reset re-baselines spent-since to the current cumulative session total.
            let baseline = if has(rest, "reset") {
                session_tokens()
            } else {
                cur_baseline
            };
            let doc = serde_json::json!({
                "session_cap_tokens": session_cap,
                "per_fanout_cap_tokens": per_fanout_cap,
                "baseline_tokens": baseline,
                "set_at": state::now_epoch(),
            });
            if state::write_json(&budget_file(), &doc).is_err() {
                return Out::err(format!("budget: cannot write {}", budget_file()), 2);
            }
            Out::ok(format!(
                "{{\"session_cap_tokens\":{},\"per_fanout_cap_tokens\":{},\"baseline_tokens\":{}}}\n",
                session_cap, per_fanout_cap, baseline
            ))
        }

        // tf budget status — the human/machine view of the current ceiling.
        "status" => {
            let (session_cap, per_fanout_cap, baseline) = load_cfg();
            let spent = spent_since(baseline);
            let remaining = (session_cap - spent).max(0);
            Out::ok(format!(
                "{{\"session_cap_tokens\":{},\"per_fanout_cap_tokens\":{},\"baseline_tokens\":{},\"session_spent\":{},\"session_remaining\":{}}}\n",
                session_cap, per_fanout_cap, baseline, spent, remaining
            ))
        }

        // tf budget spent — just the cumulative-since-baseline number.
        "spent" => {
            let (_, _, baseline) = load_cfg();
            Out::ok(format!("{}\n", spent_since(baseline)))
        }

        // tf budget check <est> — THE gate the blocking hook calls. Exit 0 = OK, DENY_CODE = refuse.
        "check" => {
            let est = state::digits_or(rest.first().map(|s| s.as_str()).unwrap_or(""), 0);
            let (session_cap, per_fanout_cap, baseline) = load_cfg();
            let spent = spent_since(baseline);
            let (ok, reason) = decide(session_cap, per_fanout_cap, spent, est);
            let line = format!(
                "{{\"decision\":\"{}\",\"reason\":\"{}\",\"est\":{},\"per_fanout_cap\":{},\"session_spent\":{},\"session_remaining\":{}}}\n",
                if ok { "OK" } else { "DENY" },
                reason,
                est,
                per_fanout_cap,
                spent,
                (session_cap - spent).max(0),
            );
            Out::line(line, if ok { 0 } else { DENY_CODE })
        }

        // tf budget arm <est> — declare a fan-out's budget (its +Xk). Refuses to arm an over-cap est.
        "arm" => {
            let est = state::digits_or(rest.first().map(|s| s.as_str()).unwrap_or(""), 0);
            let (session_cap, per_fanout_cap, baseline) = load_cfg();
            let (ok, reason) = decide(session_cap, per_fanout_cap, spent_since(baseline), est);
            if !ok {
                return Out::line(
                    format!("{{\"armed\":false,\"reason\":\"{}\",\"est\":{}}}\n", reason, est),
                    DENY_CODE,
                );
            }
            let doc = serde_json::json!({ "est": est, "at": state::now_epoch() });
            if state::write_json(&arm_file(), &doc).is_err() {
                return Out::err(format!("budget: cannot write {}", arm_file()), 2);
            }
            Out::ok(format!("{{\"armed\":true,\"est\":{}}}\n", est))
        }

        // tf budget disarm — clear the arm (the launch wrapper calls this post-launch; one-shot hygiene).
        "disarm" => {
            let _ = std::fs::remove_file(arm_file());
            Out::ok("{\"armed\":false}\n".to_string())
        }

        _ => Out::err(
            "usage: tf budget {set [--session-cap N] [--per-fanout-cap N] [--reset]|status|spent|check <est>|arm <est>|disarm}",
            2,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_declared_budget_is_refused() {
        // INV-1: the exact failure — a fan-out with no budget.total.
        assert!(!decide(2_000_000, 150_000, 0, 0).0);
        assert_eq!(decide(2_000_000, 150_000, 0, 0).1, "no-budget-declared");
        assert_eq!(decide(2_000_000, 150_000, 0, -5).1, "no-budget-declared");
    }

    #[test]
    fn within_both_caps_is_allowed() {
        assert_eq!(decide(2_000_000, 150_000, 100_000, 120_000), (true, "ok"));
    }

    #[test]
    fn over_per_fanout_cap_is_refused() {
        // The 605k runaway: a single fan-out far over the per-fanout ceiling.
        assert_eq!(
            decide(2_000_000, 150_000, 0, 605_000),
            (false, "exceeds-per-fanout-cap")
        );
    }

    #[test]
    fn over_session_cap_is_refused_even_if_per_fanout_ok() {
        // est within per-fanout cap, but cumulative would breach the session ceiling.
        assert_eq!(
            decide(500_000, 150_000, 400_000, 140_000),
            (false, "exceeds-session-cap")
        );
    }

    #[test]
    fn exactly_at_caps_is_allowed_just_over_is_not() {
        assert_eq!(decide(1_000, 1_000, 0, 1_000), (true, "ok")); // est == per_fanout_cap, spent+est == session_cap
        assert_eq!(
            decide(1_000, 1_000, 1, 1_000),
            (false, "exceeds-session-cap")
        ); // one token over
        assert_eq!(
            decide(10_000, 1_000, 0, 1_001),
            (false, "exceeds-per-fanout-cap")
        );
    }

    #[test]
    fn saturating_add_does_not_overflow() {
        assert!(decide(i64::MAX, i64::MAX, i64::MAX, i64::MAX).0);
    }

    #[test]
    fn workflow_without_arm_is_denied() {
        // INV-1, structural: a Workflow fan-out with no declared budget — the incident.
        assert!(gate_spend("Workflow", None, 2_000_000, 150_000, 0).is_some());
    }

    #[test]
    fn workflow_armed_within_caps_is_allowed() {
        assert!(gate_spend("Workflow", Some(120_000), 2_000_000, 150_000, 0).is_none());
    }

    #[test]
    fn workflow_armed_over_cap_is_denied() {
        // The 605k runaway, even if someone armed it.
        assert!(gate_spend("Workflow", Some(605_000), 2_000_000, 150_000, 0).is_some());
    }

    #[test]
    fn single_agent_allowed_until_session_cap() {
        // Ordinary single-agent work must NOT be bricked — only blocked once over the session cap.
        assert!(gate_spend("Agent", None, 1_000, 150_000, 999).is_none());
        assert!(gate_spend("Agent", None, 1_000, 150_000, 1_000).is_some());
        assert!(gate_spend("Task", None, 1_000, 150_000, 1_000).is_some());
    }

    #[test]
    fn non_spawn_tools_are_never_gated() {
        assert!(gate_spend("Read", None, 0, 0, 999_999).is_none());
        assert!(gate_spend("Bash", Some(0), 0, 0, 999_999).is_none());
    }
}
