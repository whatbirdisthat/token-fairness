//! budget — CORE-A of spend-safety enforcement (doc/design/spend-safety-enforcement.md).
//!
//! Answers ONE question for the blocking fan-out hook (CORE-B): "is this fan-out allowed to
//! run?". The decision is now WINDOW-AWARE (see [`crate::windows`]): instead of a single
//! cumulative token total that only reset on a manual `tf budget set --reset` — which climbed
//! across many provider rolling windows and FALSELY locked out a fan-out even when the live
//! 5-hour window had reset to ~0% — the gate is anchored to the two provider rolling windows:
//!
//!   - `five_hour` — the 5-hour rolling SESSION window.
//!   - `seven_day` — the 7-day rolling WEEKLY ("all-models") window.
//!
//! When a FRESH live snapshot is available the live `used_percentage` is the source of truth
//! (below the headroom ceiling → unlock; at/over → fail closed). When BLIND, a per-window token
//! cap with an auto-rebaselining baseline preserves lockout protection without a manual reset.
//! Per-window token quotas are inferred empirically by the snapshot hook (see [`crate::windows`]).
//!
//! The per-fanout cap and the "a Workflow must be armed" invariant (INV-1) are unchanged.
//! The PURE decision lives in [`crate::windows::decide`]; the wrappers here are the IO layer.

use crate::{state, windows, Out};

/// Conservative starting defaults (tokens). The user tunes these with `tf budget set`; they are
/// deliberately finite so an unconfigured environment still refuses a runaway fan-out. The
/// 5-hour cap reuses the historical `session_cap_tokens` field (now interpreted PER WINDOW).
pub const DEFAULT_SESSION_CAP: i64 = 2_000_000;
pub const DEFAULT_PER_FANOUT_CAP: i64 = 150_000;
/// Blind-regime fallback cap for the 7-day weekly window (tokens). Generous vs the 5-hour cap.
pub const DEFAULT_WEEKLY_CAP: i64 = 20_000_000;
/// Exit code for a DENY verdict (matches the scheduler's 0/3 confidence-branch convention).
pub const DENY_CODE: i32 = 3;
/// Max snapshot age (s) the gate accepts as a live signal before falling back to BLIND — the
/// same default the dispatcher's `tf gate` uses.
const SNAPSHOT_MAX_AGE: i64 = 900;

fn budget_file() -> String {
    format!("{}/budget.json", state::state_dir())
}

fn session_file() -> String {
    format!("{}/session.json", state::state_dir())
}

/// Cumulative session tokens, written by the Stop hook (`session-tokens.sh`); 0 if absent.
/// Prefers `billable_tokens` (in+out+cache_creation) when present and falls back to the full
/// `tokens`. Cache-read tokens are ~$0.10/M but dominate the raw count on a long session (e.g.
/// 71.6M tokens for $61) — counting them made the cap trip at trivial real cost (Issue #2
/// follow-up). The cap therefore tracks the EXPENSIVE tokens; `tokens`/`usd` stay full for
/// spend-audit and convergence. `pub` so the snapshot hook samples the SAME basis the cap reads.
pub fn session_tokens() -> i64 {
    match state::read_json(&session_file()) {
        Some(v) => {
            let bill = state::int(&v, "billable_tokens", -1);
            if bill >= 0 {
                bill
            } else {
                state::int(&v, "tokens", 0)
            }
        }
        None => 0,
    }
}

/// The budget configuration (from budget.json, else defaults).
struct Cfg {
    /// Per 5-hour-window token cap (blind regime); historical `session_cap_tokens` field.
    session_cap: i64,
    per_fanout_cap: i64,
    /// Legacy manual `--reset` baseline — retained for the back-compat `status`/`spent` fields.
    baseline: i64,
    headroom: i64,
    weekly_cap: i64,
}

fn load_cfg() -> Cfg {
    match state::read_json(&budget_file()) {
        Some(v) => Cfg {
            session_cap: state::int(&v, "session_cap_tokens", DEFAULT_SESSION_CAP),
            per_fanout_cap: state::int(&v, "per_fanout_cap_tokens", DEFAULT_PER_FANOUT_CAP),
            baseline: state::int(&v, "baseline_tokens", 0),
            headroom: state::int(&v, "headroom_pct", windows::DEFAULT_HEADROOM),
            weekly_cap: state::int(&v, "weekly_cap_tokens", DEFAULT_WEEKLY_CAP),
        },
        None => Cfg {
            session_cap: DEFAULT_SESSION_CAP,
            per_fanout_cap: DEFAULT_PER_FANOUT_CAP,
            baseline: 0,
            headroom: windows::DEFAULT_HEADROOM,
            weekly_cap: DEFAULT_WEEKLY_CAP,
        },
    }
}

/// PURE: spend since a baseline, distrusting a STALE baseline. A baseline GREATER than the current
/// reading means it was captured under a different basis — a pre-0.1.1 `--reset` stored full
/// `.tokens` but the cap now reads the smaller `billable_tokens` — or the session counter was
/// reset/compacted. In that case `cur - baseline` would go negative and `.max(0)` would silently
/// report ZERO consumed, disabling the cap. Instead count the full current reading (fail toward
/// enforcement). When the baseline is valid (`cur >= baseline`) this is the plain delta. `pub` so
/// the window core ([`crate::windows::spent_in_window`]) shares the exact same guard.
pub fn spent_from(cur: i64, baseline: i64) -> i64 {
    if cur >= baseline {
        cur - baseline
    } else {
        cur
    }
}

fn spent_since(baseline: i64) -> i64 {
    spent_from(session_tokens(), baseline)
}

/// One-shot record that a fan-out's budget has been declared+approved (its `+Xk`). The blocking
/// preflight REQUIRES this for a `Workflow` launch — a Workflow with no arm is the incident.
fn arm_file() -> String {
    format!("{}/fanout-arm.json", state::state_dir())
}

fn read_arm() -> Option<i64> {
    state::read_json(&arm_file()).map(|v| state::int(&v, "est", 0))
}

/// The IO wrapper around the pure [`crate::windows::decide`]: load config + current tokens +
/// per-window state + the (possibly stale) live snapshot, then decide for `est`.
fn windowed_decide(est: i64) -> (bool, String) {
    let cfg = load_cfg();
    let cur = session_tokens();
    let (five, seven) = windows::load();
    let (l5, l7) = windows::live_windows(SNAPSHOT_MAX_AGE);
    let views = [
        (
            "5h",
            windows::WinView {
                state: five,
                live: l5,
                blind_cap: cfg.session_cap,
            },
        ),
        (
            "weekly",
            windows::WinView {
                state: seven,
                live: l7,
                blind_cap: cfg.weekly_cap,
            },
        ),
    ];
    windows::decide(&views, cur, est, cfg.per_fanout_cap, cfg.headroom)
}

/// The blocking-hook decision for a PreToolUse on a spawn tool. None = allow, Some = deny reason.
/// `Workflow` is always a fan-out → it must be ARMED, then cleared by the window-aware decision;
/// `Agent`/`Task` declare no budget and are gated only against window FULLNESS (a nominal est), so
/// ordinary single-agent work is never bricked while there is allocation available.
pub fn gate_spend(tool: &str, armed: Option<i64>) -> Option<String> {
    match tool {
        "Workflow" => match armed {
            None => Some(
                "fan-out has no declared budget — run `tf budget arm <est>` (your +Xk) first"
                    .into(),
            ),
            Some(est) => match windowed_decide(est) {
                (true, _) => None,
                (false, reason) => Some(format!("fan-out budget refused: {}", reason)),
            },
        },
        "Agent" | "Task" => match windowed_decide(windows::SINGLE_AGENT_EST) {
            (true, _) => None,
            (false, reason) => Some(format!("token window blocked: {}", reason)),
        },
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
    let armed = read_arm();
    let est = armed.unwrap_or(0);
    if let Some(reason) = gate_spend(tool, armed) {
        // Capture the deny for the Honesty Observatory (P-I) — SAVES + procedural denies + friction.
        crate::observe::log_gate(tool, "deny", &reason, est);
        let human = format!(
            "{}. The live 5-hour and weekly rolling windows gate spend now — this unlocks automatically when the window resets.",
            reason
        );
        let deny = serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": human
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

/// `tf session-boundary` — the SessionStart hook body. `session.json` is a single shared file the
/// Stop hook overwrites, so a NEW session still sees the PRIOR session's cumulative token count
/// until its own first Stop — which makes `preflight-spend` read a stale total and FALSELY block
/// ordinary single-agent work (the reported incident). When the payload's `session_id` differs
/// from the stored one, zero `session.json` (the basis EVERY consumer reads — the legacy cap, the
/// window-aware blind path, and `plan-close` convergence) and re-anchor the per-window baselines.
/// No-op when the ids match (a resumed session legitimately keeps its count) or either is absent.
pub fn session_boundary(payload: &str) -> Out {
    if payload.trim().is_empty() {
        return Out::ok("{\"reset\":false,\"reason\":\"no-payload\"}\n");
    }
    let v: serde_json::Value = serde_json::from_str(payload).unwrap_or(serde_json::Value::Null);
    let new_sid = v.get("session_id").and_then(|x| x.as_str()).unwrap_or("");
    let stored_sid = state::read_json(&session_file())
        .as_ref()
        .and_then(|s| s.get("session_id"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    // Reset only on a genuine boundary: both ids known and different.
    if new_sid.is_empty() || stored_sid.is_empty() || new_sid == stored_sid {
        return Out::ok(format!("{{\"reset\":false,\"session\":\"{}\"}}\n", new_sid));
    }
    let doc = serde_json::json!({
        "session_id": new_sid,
        "tokens": 0,
        "usd": 0,
        "billable_tokens": 0,
    });
    if state::write_json(&session_file(), &doc).is_err() {
        return Out::err(
            format!("session-boundary: cannot write {}", session_file()),
            2,
        );
    }
    windows::reanchor_for_new_session();
    Out::ok(format!("{{\"reset\":true,\"session\":\"{}\"}}\n", new_sid))
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

/// Per-window display object for `tf budget status`.
fn win_disp(
    st: &windows::WinSt,
    live: Option<f64>,
    cap: i64,
    headroom: i64,
    cur: i64,
) -> serde_json::Value {
    let spent_w = windows::spent_in_window(cur, st.baseline_tokens);
    let remaining_w = match live {
        Some(u) if st.quota_tokens > 0 => windows::remaining(st.quota_tokens, u, headroom),
        _ => (cap - spent_w).max(0),
    };
    serde_json::json!({
        "used_pct": live,
        "resets_at": st.seen_resets_at,
        "quota_tokens": st.quota_tokens,
        "samples": st.samples,
        "spent_in_window": spent_w,
        "remaining_tokens": remaining_w,
        "fresh": live.is_some(),
    })
}

pub fn dispatch(argv: &[String]) -> Out {
    let sub = argv.first().map(|s| s.as_str()).unwrap_or("status");
    let rest = if argv.is_empty() { &[][..] } else { &argv[1..] };

    match sub {
        // tf budget set [--session-cap/--five-hour-cap N] [--per-fanout-cap N] [--weekly-cap N]
        //               [--headroom N] [--reset]
        "set" => {
            let cur = load_cfg();
            // `--five-hour-cap` is an alias for the historical `--session-cap`.
            let session_cap = flag(rest, "five-hour-cap")
                .or_else(|| flag(rest, "session-cap"))
                .map(|s| state::digits_or(s, cur.session_cap))
                .unwrap_or(cur.session_cap);
            let per_fanout_cap = flag(rest, "per-fanout-cap")
                .map(|s| state::digits_or(s, cur.per_fanout_cap))
                .unwrap_or(cur.per_fanout_cap);
            let weekly_cap = flag(rest, "weekly-cap")
                .map(|s| state::digits_or(s, cur.weekly_cap))
                .unwrap_or(cur.weekly_cap);
            let headroom = flag(rest, "headroom")
                .map(|s| state::digits_or(s, cur.headroom))
                .unwrap_or(cur.headroom);
            // --reset re-baselines the legacy spent-since to the current cumulative session total.
            let baseline = if has(rest, "reset") {
                session_tokens()
            } else {
                cur.baseline
            };
            let doc = serde_json::json!({
                "session_cap_tokens": session_cap,
                "per_fanout_cap_tokens": per_fanout_cap,
                "weekly_cap_tokens": weekly_cap,
                "headroom_pct": headroom,
                "baseline_tokens": baseline,
                "set_at": state::now_epoch(),
            });
            if state::write_json(&budget_file(), &doc).is_err() {
                return Out::err(format!("budget: cannot write {}", budget_file()), 2);
            }
            Out::ok(format!(
                "{{\"session_cap_tokens\":{},\"per_fanout_cap_tokens\":{},\"weekly_cap_tokens\":{},\"headroom_pct\":{},\"baseline_tokens\":{}}}\n",
                session_cap, per_fanout_cap, weekly_cap, headroom, baseline
            ))
        }

        // tf budget status — the human/machine view: legacy cumulative fields + per-window state.
        "status" => {
            let cfg = load_cfg();
            let spent = spent_since(cfg.baseline);
            let remaining = (cfg.session_cap - spent).max(0);
            let cur = session_tokens();
            let (five, seven) = windows::load();
            let (l5, l7) = windows::live_windows(SNAPSHOT_MAX_AGE);
            let win_doc = serde_json::json!({
                "five_hour": win_disp(&five, l5, cfg.session_cap, cfg.headroom, cur),
                "seven_day": win_disp(&seven, l7, cfg.weekly_cap, cfg.headroom, cur),
            });
            Out::ok(format!(
                "{{\"session_cap_tokens\":{},\"per_fanout_cap_tokens\":{},\"weekly_cap_tokens\":{},\"headroom_pct\":{},\"baseline_tokens\":{},\"session_spent\":{},\"session_remaining\":{},\"windows\":{}}}\n",
                cfg.session_cap, cfg.per_fanout_cap, cfg.weekly_cap, cfg.headroom, cfg.baseline, spent, remaining,
                serde_json::to_string(&win_doc).unwrap_or_else(|_| "{}".into())
            ))
        }

        // tf budget spent — just the legacy cumulative-since-baseline number.
        "spent" => {
            let cfg = load_cfg();
            Out::ok(format!("{}\n", spent_since(cfg.baseline)))
        }

        // tf budget check <est> — THE gate the blocking hook calls. Exit 0 = OK, DENY_CODE = refuse.
        // Window-aware: a fresh live window with headroom clears it even when the legacy cumulative
        // total is large (the fix), while an at-ceiling / blind-cap-exhausted window refuses.
        "check" => {
            let est = state::digits_or(rest.first().map(|s| s.as_str()).unwrap_or(""), 0);
            let (ok, reason) = windowed_decide(est);
            let line = format!(
                "{{\"decision\":\"{}\",\"reason\":\"{}\",\"est\":{}}}\n",
                if ok { "OK" } else { "DENY" },
                reason,
                est,
            );
            Out::line(line, if ok { 0 } else { DENY_CODE })
        }

        // tf budget arm <est> — declare a fan-out's budget (its +Xk). Refuses an est the
        // window-aware gate would not currently allow.
        "arm" => {
            let est = state::digits_or(rest.first().map(|s| s.as_str()).unwrap_or(""), 0);
            let (ok, reason) = windowed_decide(est);
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
            "usage: tf budget {set [--five-hour-cap N] [--per-fanout-cap N] [--weekly-cap N] [--headroom N] [--reset]|status|spent|check <est>|arm <est>|disarm}",
            2,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{temp_dir, ENV_LOCK};

    #[test]
    fn spent_from_distrusts_a_stale_larger_baseline() {
        // Valid baseline → plain delta.
        assert_eq!(spent_from(1_800_000, 1_000_000), 800_000);
        assert_eq!(spent_from(1_000_000, 1_000_000), 0);
        // STALE baseline (Issue #2 follow-up): a pre-0.1.1 `--reset` stored full .tokens (71.6M);
        // the cap now reads the smaller billable_tokens (1.8M). Old code: max(0, 1.8M-71.6M)=0 →
        // cap silently disabled. New: count the full current reading → cap stays live.
        assert_eq!(spent_from(1_800_000, 71_600_673), 1_800_000);
    }

    #[test]
    fn workflow_without_arm_is_denied() {
        // INV-1, structural: a Workflow fan-out with no declared budget — the incident.
        assert!(gate_spend("Workflow", None).is_some());
    }

    #[test]
    fn non_spawn_tools_are_never_gated() {
        assert!(gate_spend("Read", None).is_none());
        assert!(gate_spend("Bash", Some(0)).is_none());
    }

    /// Lay down a state dir with a fresh snapshot + a big cumulative session.json + an arm.
    fn scenario(tag: &str, five_pct: f64, seven_pct: f64) -> std::path::PathBuf {
        let dir = temp_dir(tag);
        std::env::set_var("I2P_COST_STATE_DIR", &dir);
        std::env::set_var("I2P_RATELIMIT_SNAPSHOT", dir.join("snap.json"));
        std::env::set_var("I2P_HONESTY_EVENTS", dir.join("ev.jsonl"));
        std::env::set_var("I2P_CLOCK", "2000");
        std::fs::write(dir.join("session.json"), r#"{"billable_tokens":90000000}"#).unwrap();
        std::fs::write(
            dir.join("snap.json"),
            format!(
                r#"{{"captured_at":1990,"rate_limits":{{"five_hour":{{"used_percentage":{},"resets_at":1800000000}},"seven_day":{{"used_percentage":{},"resets_at":1800500000}}}}}}"#,
                five_pct, seven_pct
            ),
        )
        .unwrap();
        std::fs::write(dir.join("fanout-arm.json"), r#"{"est":120000}"#).unwrap();
        dir
    }

    fn clear_env() {
        for k in [
            "I2P_COST_STATE_DIR",
            "I2P_RATELIMIT_SNAPSHOT",
            "I2P_HONESTY_EVENTS",
            "I2P_CLOCK",
        ] {
            std::env::remove_var(k);
        }
    }

    #[test]
    fn preflight_spend_unlocks_when_live_window_is_empty() {
        let _g = ENV_LOCK.lock().unwrap();
        // THE reported bug: cumulative tokens are huge (90M) but the live 5h window is at 1% and
        // the weekly at 3% — plenty of allocation. The gate must ALLOW (empty output, no deny).
        let dir = scenario("budget-unlock", 1.0, 3.0);
        let out = preflight_spend(r#"{"tool_name":"Workflow"}"#);
        clear_env();
        assert!(
            out.stdout.is_empty(),
            "expected ALLOW (no deny JSON), got: {}",
            out.stdout
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dispatch_set_status_check_arm_roundtrip() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_dir("budget-dispatch");
        std::env::set_var("I2P_COST_STATE_DIR", &dir);
        std::env::set_var("I2P_RATELIMIT_SNAPSHOT", dir.join("snap.json"));
        std::env::set_var("I2P_HONESTY_EVENTS", dir.join("ev.jsonl"));
        std::env::set_var("I2P_CLOCK", "2000");
        // A fresh window with headroom so the window-aware check/arm clear.
        std::fs::write(
            dir.join("snap.json"),
            r#"{"captured_at":1990,"rate_limits":{"five_hour":{"used_percentage":4.0,"resets_at":1800000000},"seven_day":{"used_percentage":1.0,"resets_at":1800500000}}}"#,
        )
        .unwrap();

        let s = |args: &[&str]| dispatch(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>());

        // set persists all five fields (incl. the --five-hour-cap alias + new flags).
        let set = s(&[
            "set",
            "--five-hour-cap",
            "3000000",
            "--weekly-cap",
            "30000000",
            "--headroom",
            "20",
        ]);
        assert!(set.stdout.contains("\"session_cap_tokens\":3000000"));
        assert!(set.stdout.contains("\"weekly_cap_tokens\":30000000"));
        assert!(set.stdout.contains("\"headroom_pct\":20"));

        // status surfaces the per-window object built from the fresh snapshot.
        let status = s(&["status"]);
        assert!(status.stdout.contains("\"windows\""));
        assert!(status.stdout.contains("\"used_pct\":4.0"));

        // check within caps + fresh headroom ⇒ OK exit 0; arm records it.
        let chk = s(&["check", "120000"]);
        assert_eq!(chk.code, 0);
        assert!(chk.stdout.contains("\"decision\":\"OK\""));
        let arm = s(&["arm", "120000"]);
        assert!(arm.stdout.contains("\"armed\":true"));
        assert!(read_arm().is_some());

        // an over-per-fanout-cap est is refused at check and arm (exit DENY_CODE).
        let over = s(&["check", "999999"]);
        assert_eq!(over.code, DENY_CODE);
        assert!(over.stdout.contains("exceeds-per-fanout-cap"));

        // disarm clears the one-shot arm.
        s(&["disarm"]);
        assert!(read_arm().is_none());

        // unknown subcommand → usage on stderr, exit 2.
        assert_eq!(s(&["wat"]).code, 2);

        for k in [
            "I2P_COST_STATE_DIR",
            "I2P_RATELIMIT_SNAPSHOT",
            "I2P_HONESTY_EVENTS",
            "I2P_CLOCK",
        ] {
            std::env::remove_var(k);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn preflight_spend_denies_when_live_window_at_ceiling() {
        let _g = ENV_LOCK.lock().unwrap();
        // 5h window at 95% ⇒ near lockout ⇒ fail closed even though weekly is clear.
        let dir = scenario("budget-ceiling", 95.0, 3.0);
        let out = preflight_spend(r#"{"tool_name":"Workflow"}"#);
        clear_env();
        assert!(out.stdout.contains("\"permissionDecision\":\"deny\""));
        assert!(
            out.stdout.contains("5h-window-at-ceiling"),
            "deny should name the window, got: {}",
            out.stdout
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn session_boundary_resets_on_new_session_and_noops_otherwise() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_dir("budget-boundary");
        std::env::set_var("I2P_COST_STATE_DIR", &dir);
        let sj = dir.join("session.json");
        let read_sj = || std::fs::read_to_string(&sj).unwrap_or_default();

        // Prior heavy session left a huge cumulative total under session_id "old".
        std::fs::write(
            &sj,
            r#"{"session_id":"old","tokens":238599810,"billable_tokens":238599810}"#,
        )
        .unwrap();
        // A windows.json with a learned quota + a non-zero baseline from the prior session.
        std::fs::write(
            dir.join("windows.json"),
            r#"{"five_hour":{"baseline_tokens":230000000,"seen_resets_at":1800000000,"quota_tokens":2000000,"samples":5,"last_tokens":238599810,"last_used_pct":40.0},"seven_day":{"baseline_tokens":0,"seen_resets_at":1800500000,"quota_tokens":20000000,"samples":4,"last_tokens":238599810,"last_used_pct":12.0}}"#,
        )
        .unwrap();

        // Same session_id ⇒ no reset (a resumed session keeps its count).
        let same = session_boundary(r#"{"session_id":"old"}"#);
        assert!(same.stdout.contains("\"reset\":false"));
        assert!(
            read_sj().contains("238599810"),
            "matching sid must not zero session.json"
        );

        // New session_id ⇒ reset: session.json zeroed with the new sid.
        let out = session_boundary(r#"{"session_id":"new"}"#);
        assert!(out.stdout.contains("\"reset\":true"));
        let after = read_sj();
        assert!(after.contains("\"session_id\": \"new\""));
        assert!(after.contains("\"billable_tokens\": 0"));
        assert!(after.contains("\"tokens\": 0"));
        // windows.json baselines re-anchored to 0 but the learned quota is PRESERVED.
        let (five, seven) = windows::load();
        assert_eq!(five.baseline_tokens, 0);
        assert_eq!(five.last_tokens, 0);
        assert_eq!(
            five.quota_tokens, 2_000_000,
            "quota survives a session boundary"
        );
        assert_eq!(
            five.seen_resets_at, 1_800_000_000,
            "active window reset epoch preserved"
        );
        assert_eq!(seven.quota_tokens, 20_000_000);

        // After the reset, the prior heavy total no longer blocks a single Agent spawn (blind path:
        // no fresh snapshot ⇒ spent_in_window(0, 0) = 0).
        let agent = preflight_spend(r#"{"tool_name":"Agent"}"#);
        assert!(
            agent.stdout.is_empty(),
            "Agent spawn must be unblocked, got: {}",
            agent.stdout
        );

        std::env::remove_var("I2P_COST_STATE_DIR");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn session_boundary_noops_without_stored_or_payload() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_dir("budget-boundary-empty");
        std::env::set_var("I2P_COST_STATE_DIR", &dir);
        // No session.json yet ⇒ nothing stale to reset.
        assert!(session_boundary(r#"{"session_id":"new"}"#)
            .stdout
            .contains("\"reset\":false"));
        // Empty payload ⇒ no-op.
        assert!(session_boundary("").stdout.contains("\"reset\":false"));
        assert!(!dir.join("session.json").exists());
        std::env::remove_var("I2P_COST_STATE_DIR");
        std::fs::remove_dir_all(&dir).ok();
    }
}
