//! windows — WINDOW-AWARE budget core (CORE-A, the unlock half).
//!
//! The original CORE-A (`budget.rs`) gated on a single cumulative token total that only reset
//! on a manual `tf budget set --reset`. That number climbs across many provider rolling
//! windows and locks out a fan-out even when the live 5-hour window has reset to ~0% — a FALSE
//! lockout (plenty of allocation available). This module replaces the cumulative cap with a
//! per-window decision anchored to the provider's two rolling windows:
//!
//!   - `five_hour`  — the 5-hour rolling SESSION window.
//!   - `seven_day`  — the 7-day rolling WEEKLY ("all-models") window.
//!
//! Two regimes (see `decide`):
//!   1. FRESH live signal (a recent `ratelimit-snapshot.json`): the live `used_percentage` is
//!      the source of truth. Below the headroom ceiling → UNLOCK; at/over it → fail closed
//!      (consistent with the L1 ceiling guard). When a per-window token QUOTA has been inferred
//!      with confidence, also refuse a fan-out whose estimate exceeds the window's remaining
//!      headroom; until confident, a below-ceiling window simply allows (cold-start).
//!   2. BLIND (no fresh snapshot): a per-window token cap whose baseline AUTO-REBASELINES on
//!      every observed window reset, so it no longer needs a manual `--reset`. This preserves
//!      lockout protection when we cannot see the live signal.
//!
//! Per-window token QUOTA is inferred empirically (EWMA over `Δtokens / (Δused% / 100)`),
//! matching the project's calibration style. CAVEAT: `used_percentage` is account-wide while
//! `session.json.billable_tokens` counts only this session, so a single-session quota is biased
//! LOW under concurrent usage. Delta-sampling cancels the constant other-usage contribution,
//! and the `MIN_SAMPLES` confidence gate + cold-start-allows ensure a low/early quota never
//! FALSELY denies — it only ever adds a refusal once it is confident the estimate won't fit.

use crate::state;
use serde_json::{json, Value};

/// EWMA smoothing for the quota estimate (newer observations weighted ALPHA).
pub const ALPHA: f64 = 0.3;
/// A usable quota sample needs at least this rise in `used_percentage` between two snapshots —
/// below it the `Δtokens / Δused%` ratio is dominated by measurement noise.
pub const MIN_DELTA_PCT: f64 = 1.0;
/// Samples required before the inferred quota is trusted to REFUSE (vs merely report).
pub const MIN_SAMPLES: i64 = 3;
/// Sanity clamp on a single inferred-quota observation (tokens) — reject obvious garbage.
pub const QUOTA_MIN: f64 = 100_000.0;
pub const QUOTA_MAX: f64 = 1_000_000_000.0;
/// Headroom percent the gate keeps below 100% (mirrors the L1 ceiling default 15 → ceiling 85).
pub const DEFAULT_HEADROOM: i64 = 15;
/// A single `Agent`/`Task` declares no budget; it is gated only against window fullness, so it
/// carries this nominal estimate through `decide` (enough to trip an at-ceiling / cap-exhausted
/// window, never enough to matter otherwise).
pub const SINGLE_AGENT_EST: i64 = 1;

/// Per-window persisted state (one entry per window in `windows.json`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WinSt {
    /// Cumulative `billable_tokens` at the start of the current window (auto-rebaselined on reset).
    pub baseline_tokens: i64,
    /// The `resets_at` epoch of the window we are currently anchored to.
    pub seen_resets_at: i64,
    /// Inferred total token allocation for this window; 0 = unknown.
    pub quota_tokens: i64,
    /// Number of accepted quota samples (the confidence counter).
    pub samples: i64,
    /// Last observed cumulative tokens (for the next delta).
    pub last_tokens: i64,
    /// Last observed `used_percentage` (for the next delta).
    pub last_used_pct: f64,
}

/// What the gate sees for one window at decision time.
pub struct WinView {
    pub state: WinSt,
    /// `Some(used_pct)` when a fresh snapshot carries this window; `None` = blind.
    pub live: Option<f64>,
    /// Fallback token cap for the blind regime (e.g. the configured 5h / weekly cap).
    pub blind_cap: i64,
}

/// PURE: roll the baseline forward only when the window has reset (`cur_reset > prev_seen`).
/// On the very first observation (`prev_seen == 0`) this adopts the current reset and anchors
/// the baseline to the current token reading. Returns `(baseline, seen_resets_at)`.
pub fn rebaseline(
    prev_seen: i64,
    cur_reset: i64,
    cur_tokens: i64,
    prev_baseline: i64,
) -> (i64, i64) {
    if cur_reset > prev_seen {
        (cur_tokens, cur_reset)
    } else {
        (prev_baseline, prev_seen)
    }
}

/// PURE: tokens spent within the current window. Reuses the stale-baseline guard
/// (`budget::spent_from`): a baseline larger than the current reading (session counter
/// reset/compacted) counts the full reading rather than silently reporting zero.
pub fn spent_in_window(cur_tokens: i64, baseline: i64) -> i64 {
    crate::budget::spent_from(cur_tokens, baseline)
}

/// PURE: delta-based EWMA quota inference. `d_used_pct` is the rise in `used_percentage` and
/// `d_tokens` the rise in cumulative tokens between two consecutive snapshots WITHIN one window.
/// Returns the updated `(quota, samples)`; a non-usable or out-of-clamp observation is a no-op.
pub fn update_quota(
    prev_quota: i64,
    prev_samples: i64,
    d_tokens: i64,
    d_used_pct: f64,
) -> (i64, i64) {
    if d_used_pct < MIN_DELTA_PCT || d_tokens <= 0 {
        return (prev_quota, prev_samples);
    }
    let obs = d_tokens as f64 / (d_used_pct / 100.0);
    if !(QUOTA_MIN..=QUOTA_MAX).contains(&obs) {
        return (prev_quota, prev_samples);
    }
    let quota = if prev_samples == 0 || prev_quota <= 0 {
        obs
    } else {
        ALPHA * obs + (1.0 - ALPHA) * prev_quota as f64
    };
    (quota.round() as i64, prev_samples + 1)
}

/// PURE: tokens estimated to remain in a window before its headroom ceiling, given the live
/// `used_pct` and the inferred `quota`. Floors at 0 (at/over ceiling, or unknown quota).
pub fn remaining(quota: i64, used_pct: f64, headroom: i64) -> i64 {
    let room_pct = (100 - headroom) as f64 - used_pct;
    if room_pct <= 0.0 || quota <= 0 {
        return 0;
    }
    (quota as f64 * room_pct / 100.0).round() as i64
}

/// The per-window verdict: allow, or deny with a machine reason.
fn window_decide(
    name: &str,
    v: &WinView,
    cur_tokens: i64,
    est: i64,
    headroom: i64,
) -> Option<String> {
    let ceiling = (100 - headroom) as f64;
    match v.live {
        Some(used) => {
            if used >= ceiling {
                return Some(format!("{}-window-at-ceiling", name));
            }
            // Below the ceiling. Refuse only when a CONFIDENT quota says the estimate won't fit;
            // a cold-start (or zero-quota) window unlocks on headroom alone.
            if v.state.samples >= MIN_SAMPLES && v.state.quota_tokens > 0 {
                let rem = remaining(v.state.quota_tokens, used, headroom);
                if est > rem {
                    return Some(format!("exceeds-{}-window-budget", name));
                }
            }
            None
        }
        None => {
            // Blind: per-window token cap with the auto-rebaselined baseline.
            let spent = spent_in_window(cur_tokens, v.state.baseline_tokens);
            let cap = if v.state.quota_tokens > 0 {
                v.state.quota_tokens
            } else {
                v.blind_cap
            };
            if spent.saturating_add(est) > cap {
                Some(format!("{}-window-cap-reached", name))
            } else {
                None
            }
        }
    }
}

/// THE decision — pure. Allowed iff `est` clears the per-fanout cap AND every window allows.
/// `views` is `(window-name, view)` pairs (5h, weekly). Returns `(allowed, reason)`.
pub fn decide(
    views: &[(&str, WinView)],
    cur_tokens: i64,
    est: i64,
    per_fanout_cap: i64,
    headroom: i64,
) -> (bool, String) {
    if est <= 0 {
        return (false, "no-budget-declared".into());
    }
    if est > per_fanout_cap {
        return (false, "exceeds-per-fanout-cap".into());
    }
    for (name, v) in views {
        if let Some(reason) = window_decide(name, v, cur_tokens, est, headroom) {
            return (false, reason);
        }
    }
    (true, "ok".into())
}

// ── state IO ─────────────────────────────────────────────────────────────────────────────

fn windows_file() -> String {
    format!("{}/windows.json", state::state_dir())
}

/// The freshness-stamped live snapshot the dispatcher and this gate both read. Mirrors
/// `scheduler::snapshot_path` (honours `I2P_RATELIMIT_SNAPSHOT`).
pub fn snapshot_path() -> String {
    std::env::var("I2P_RATELIMIT_SNAPSHOT")
        .unwrap_or_else(|_| format!("{}/ratelimit-snapshot.json", state::state_dir()))
}

fn read_win(root: Option<&Value>, key: &str) -> WinSt {
    let o = root.and_then(|v| v.get(key));
    let g_i = |k: &str| o.map(|o| state::int(o, k, 0)).unwrap_or(0);
    WinSt {
        baseline_tokens: g_i("baseline_tokens"),
        seen_resets_at: g_i("seen_resets_at"),
        quota_tokens: g_i("quota_tokens"),
        samples: g_i("samples"),
        last_tokens: g_i("last_tokens"),
        last_used_pct: o
            .map(|o| state::num(o, "last_used_pct", 0.0))
            .unwrap_or(0.0),
    }
}

/// Load both windows' persisted state from `windows.json` (defaults when absent).
pub fn load() -> (WinSt, WinSt) {
    let v = state::read_json(&windows_file());
    (
        read_win(v.as_ref(), "five_hour"),
        read_win(v.as_ref(), "seven_day"),
    )
}

fn win_json(s: &WinSt) -> Value {
    json!({
        "baseline_tokens": s.baseline_tokens,
        "seen_resets_at": s.seen_resets_at,
        "quota_tokens": s.quota_tokens,
        "samples": s.samples,
        "last_tokens": s.last_tokens,
        "last_used_pct": s.last_used_pct,
    })
}

fn save(five: &WinSt, seven: &WinSt) -> std::io::Result<()> {
    let doc = json!({ "five_hour": win_json(five), "seven_day": win_json(seven) });
    state::write_json(&windows_file(), &doc)
}

/// Session boundary: the cumulative token counter restarted at 0 (a new session), so re-anchor
/// each window's `baseline_tokens` and `last_tokens` to 0 — spend-in-window then measures from
/// the new session. The inferred `quota_tokens`/`samples` (account-level) and `seen_resets_at`
/// (the still-active window) are PRESERVED. No-op-safe: a missing windows.json loads defaults.
pub fn reanchor_for_new_session() {
    let (mut five, mut seven) = load();
    for w in [&mut five, &mut seven] {
        w.baseline_tokens = 0;
        w.last_tokens = 0;
    }
    let _ = save(&five, &seven);
}

/// PURE step for one window: fold a fresh `(used_pct, resets_at, cur_tokens)` observation into
/// the window's state — rebaseline on reset, then sample the quota only WITHIN a window
/// (`!rolled`) once a prior reading exists. Extracted so the maintenance loop is unit-tested.
pub fn fold(st: WinSt, used_pct: f64, resets_at: i64, cur_tokens: i64) -> WinSt {
    let rolled = resets_at > st.seen_resets_at && st.seen_resets_at != 0;
    let (baseline, seen) = rebaseline(st.seen_resets_at, resets_at, cur_tokens, st.baseline_tokens);
    let (quota, samples) = if !rolled && st.last_tokens > 0 {
        update_quota(
            st.quota_tokens,
            st.samples,
            cur_tokens - st.last_tokens,
            used_pct - st.last_used_pct,
        )
    } else {
        (st.quota_tokens, st.samples)
    };
    WinSt {
        baseline_tokens: baseline,
        seen_resets_at: seen,
        quota_tokens: quota,
        samples,
        last_tokens: cur_tokens,
        last_used_pct: used_pct,
    }
}

fn fold_payload(st: WinSt, payload: &Value, key: &str, cur_tokens: i64) -> WinSt {
    let used = payload
        .pointer(&format!("/rate_limits/{}/used_percentage", key))
        .and_then(|x| x.as_f64());
    let reset = payload
        .pointer(&format!("/rate_limits/{}/resets_at", key))
        .and_then(|x| x.as_i64());
    match (used, reset) {
        (Some(u), Some(r)) => fold(st, u, r, cur_tokens),
        _ => st,
    }
}

/// Maintain `windows.json` from a fresh live payload (called by the snapshot hook). `cur_tokens`
/// is the current cumulative `billable_tokens`. Best-effort: a write failure is non-fatal.
pub fn maintain(payload: &Value, cur_tokens: i64) {
    let (five, seven) = load();
    let five = fold_payload(five, payload, "five_hour", cur_tokens);
    let seven = fold_payload(seven, payload, "seven_day", cur_tokens);
    let _ = save(&five, &seven);
}

/// Read the live `used_percentage` for (five_hour, seven_day) from a FRESH snapshot; `None` per
/// window when no snapshot, a stale one (older than `max_age` seconds), or the window is absent.
pub fn live_windows(max_age: i64) -> (Option<f64>, Option<f64>) {
    let Some(v) = state::read_json(&snapshot_path()) else {
        return (None, None);
    };
    let cap = state::int(&v, "captured_at", 0);
    let now = state::now_epoch();
    if cap <= 0 || now - cap < 0 || now - cap > max_age {
        return (None, None);
    }
    let g = |w: &str| {
        v.pointer(&format!("/rate_limits/{}/used_percentage", w))
            .and_then(|x| x.as_f64())
    };
    (g("five_hour"), g("seven_day"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(state: WinSt, live: Option<f64>, blind_cap: i64) -> WinView {
        WinView {
            state,
            live,
            blind_cap,
        }
    }

    #[test]
    fn rebaseline_only_advances_on_reset() {
        // First observation: adopt the reset, anchor baseline to current tokens.
        assert_eq!(
            rebaseline(0, 1_800_000_000, 500_000, 0),
            (500_000, 1_800_000_000)
        );
        // Same window (reset unchanged): no-op.
        assert_eq!(
            rebaseline(1_800_000_000, 1_800_000_000, 900_000, 500_000),
            (500_000, 1_800_000_000)
        );
        // Window rolled (later reset): re-anchor baseline to current tokens.
        assert_eq!(
            rebaseline(1_800_000_000, 1_800_018_000, 900_000, 500_000),
            (900_000, 1_800_018_000)
        );
    }

    #[test]
    fn update_quota_seeds_then_ewma_blends() {
        // Seed: 420k tokens for a 21% rise ⇒ ~2.0M quota.
        let (q1, n1) = update_quota(0, 0, 420_000, 21.0);
        assert_eq!(n1, 1);
        assert_eq!(q1, 2_000_000);
        // Blend: a second observation suggesting 2.5M ⇒ 0.3*2.5M + 0.7*2.0M = 2.15M.
        let (q2, n2) = update_quota(q1, n1, 250_000, 10.0);
        assert_eq!(n2, 2);
        assert_eq!(q2, 2_150_000);
    }

    #[test]
    fn update_quota_skips_noise_and_outliers() {
        // Tiny Δ% ⇒ not a sample.
        assert_eq!(update_quota(2_000_000, 3, 5_000, 0.4), (2_000_000, 3));
        // Non-positive Δtokens (session counter reset) ⇒ not a sample.
        assert_eq!(update_quota(2_000_000, 3, -10, 5.0), (2_000_000, 3));
        // Absurd observation (10 tokens for 5% ⇒ 200 quota < clamp) ⇒ ignored.
        assert_eq!(update_quota(2_000_000, 3, 10, 5.0), (2_000_000, 3));
    }

    #[test]
    fn remaining_applies_headroom_and_floors() {
        // 2M quota, 50% used, headroom 15 ⇒ ceiling 85 ⇒ 35% of 2M = 700k.
        assert_eq!(remaining(2_000_000, 50.0, 15), 700_000);
        // At/over the ceiling ⇒ 0.
        assert_eq!(remaining(2_000_000, 85.0, 15), 0);
        assert_eq!(remaining(2_000_000, 99.0, 15), 0);
        // Unknown quota ⇒ 0.
        assert_eq!(remaining(0, 10.0, 15), 0);
    }

    #[test]
    fn no_budget_and_per_fanout_cap_still_enforced() {
        let v = [("5h", view(WinSt::default(), Some(1.0), 2_000_000))];
        assert_eq!(decide(&v, 0, 0, 150_000, 15).1, "no-budget-declared");
        let v = [("5h", view(WinSt::default(), Some(1.0), 2_000_000))];
        assert_eq!(
            decide(&v, 0, 605_000, 150_000, 15).1,
            "exceeds-per-fanout-cap"
        );
    }

    #[test]
    fn fresh_below_ceiling_unlocks_despite_huge_cumulative() {
        // THE reported false lockout: 5h at 1%, weekly at 3%, cumulative tokens are enormous.
        // Both windows fresh and below ceiling ⇒ UNLOCK.
        let five = view(WinSt::default(), Some(1.0), 2_000_000);
        let seven = view(WinSt::default(), Some(3.0), 20_000_000);
        let v = [("5h", five), ("weekly", seven)];
        assert_eq!(
            decide(&v, 90_000_000, 120_000, 150_000, 15),
            (true, "ok".into())
        );
    }

    #[test]
    fn fresh_at_ceiling_fails_closed() {
        let five = view(WinSt::default(), Some(92.0), 2_000_000);
        let v = [("5h", five)];
        assert_eq!(
            decide(&v, 0, 120_000, 150_000, 15).1,
            "5h-window-at-ceiling"
        );
    }

    #[test]
    fn confident_quota_refuses_oversized_estimate_but_cold_start_allows() {
        // Confident quota: 2M, 80% used, headroom 15 ⇒ remaining = 5% = 100k. 120k won't fit.
        let st = WinSt {
            quota_tokens: 2_000_000,
            samples: MIN_SAMPLES,
            ..Default::default()
        };
        let v = [("5h", view(st, Some(80.0), 2_000_000))];
        assert_eq!(
            decide(&v, 0, 120_000, 150_000, 15).1,
            "exceeds-5h-window-budget"
        );
        // Same window but quota not yet confident ⇒ below-ceiling allows on headroom alone.
        let cold = WinSt {
            quota_tokens: 2_000_000,
            samples: MIN_SAMPLES - 1,
            ..Default::default()
        };
        let v = [("5h", view(cold, Some(80.0), 2_000_000))];
        assert_eq!(decide(&v, 0, 120_000, 150_000, 15), (true, "ok".into()));
    }

    #[test]
    fn blind_uses_per_window_cap_with_rebaselined_baseline() {
        // No live signal. Spent-in-window = cur(1.5M) - baseline(1.4M) = 100k; cap 2M ⇒ 120k fits.
        let st = WinSt {
            baseline_tokens: 1_400_000,
            ..Default::default()
        };
        let v = [("5h", view(st, None, 2_000_000))];
        assert_eq!(
            decide(&v, 1_500_000, 120_000, 150_000, 15),
            (true, "ok".into())
        );
        // Push spent to the cap ⇒ blind cap reached.
        let st = WinSt {
            baseline_tokens: 0,
            ..Default::default()
        };
        let v = [("5h", view(st, None, 2_000_000))];
        assert_eq!(
            decide(&v, 1_950_000, 120_000, 150_000, 15).1,
            "5h-window-cap-reached"
        );
    }

    #[test]
    fn weekly_window_can_block_while_five_hour_is_clear() {
        let five = view(WinSt::default(), Some(2.0), 2_000_000);
        let seven = view(WinSt::default(), Some(95.0), 20_000_000);
        let v = [("5h", five), ("weekly", seven)];
        assert_eq!(
            decide(&v, 0, 120_000, 150_000, 15).1,
            "weekly-window-at-ceiling"
        );
    }

    #[test]
    fn fold_rebaselines_and_accumulates_quota() {
        // First fold: anchors baseline + reset, records last reading, no sample yet.
        let s0 = fold(WinSt::default(), 5.0, 1_800_000_000, 100_000);
        assert_eq!(s0.baseline_tokens, 100_000);
        assert_eq!(s0.seen_resets_at, 1_800_000_000);
        assert_eq!(s0.samples, 0);
        // Second fold, same window: +420k tokens for +21% ⇒ ~2.0M quota, 1 sample.
        let s1 = fold(s0, 26.0, 1_800_000_000, 520_000);
        assert_eq!(s1.samples, 1);
        assert_eq!(s1.quota_tokens, 2_000_000);
        // Window rolls: used% drops, baseline re-anchors, no sample across the reset.
        let s2 = fold(s1, 2.0, 1_800_018_000, 600_000);
        assert_eq!(s2.baseline_tokens, 600_000);
        assert_eq!(s2.seen_resets_at, 1_800_018_000);
        assert_eq!(s2.samples, 1, "no sample taken across a reset");
    }

    #[test]
    fn maintain_and_load_roundtrip() {
        let _g = crate::testutil::ENV_LOCK.lock().unwrap();
        let dir = crate::testutil::temp_dir("windows");
        std::env::set_var("I2P_COST_STATE_DIR", &dir);
        let p: Value = serde_json::from_str(
            r#"{"rate_limits":{"five_hour":{"used_percentage":5.0,"resets_at":1800000000},
                 "seven_day":{"used_percentage":2.0,"resets_at":1800500000}}}"#,
        )
        .unwrap();
        maintain(&p, 100_000);
        let (five, _seven) = load();
        std::env::remove_var("I2P_COST_STATE_DIR");
        assert_eq!(five.baseline_tokens, 100_000);
        assert_eq!(five.seen_resets_at, 1_800_000_000);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn live_windows_honours_freshness() {
        let _g = crate::testutil::ENV_LOCK.lock().unwrap();
        let dir = crate::testutil::temp_dir("windows-live");
        let snap = dir.join("snap.json");
        std::env::set_var("I2P_RATELIMIT_SNAPSHOT", &snap);
        std::env::set_var("I2P_CLOCK", "2000");
        // Fresh (captured_at within max_age).
        std::fs::write(
            &snap,
            r#"{"captured_at":1900,"rate_limits":{"five_hour":{"used_percentage":7.5}}}"#,
        )
        .unwrap();
        let (l5, l7) = live_windows(900);
        assert_eq!(l5, Some(7.5));
        assert_eq!(l7, None);
        // Stale (older than max_age) ⇒ blind.
        std::fs::write(
            &snap,
            r#"{"captured_at":100,"rate_limits":{"five_hour":{"used_percentage":7.5}}}"#,
        )
        .unwrap();
        assert_eq!(live_windows(900), (None, None));
        std::env::remove_var("I2P_RATELIMIT_SNAPSHOT");
        std::env::remove_var("I2P_CLOCK");
        std::fs::remove_dir_all(&dir).ok();
    }
}
