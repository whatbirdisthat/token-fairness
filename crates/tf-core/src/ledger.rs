//! ledger — L4 CHEAP-RESUME LEDGER. Port of `job-ledger.sh`.
//!
//! The durable record of a guarded job: done / remaining / failed units, the budget +
//! off-peak terms, context pointers (so a resume reuses derived context), and a checkpoint
//! log of every pause with the live ceiling snapshot that triggered it. Project-local at
//! `<dir>/.i2p/jobs/<safe_jid>.json`; atomic writes; pure state transitions.
//!
//! Set semantics reproduce jq exactly: `-=` removes every equal element; `unique` sorts.

use crate::{state, Out};
use serde_json::{json, Value};

fn ledger_path(dir: &str, jid: &str) -> String {
    let d = dir.strip_suffix('/').unwrap_or(dir);
    format!("{}/.i2p/jobs/{}.json", d, state::safe_id(jid))
}

/// Read the ledger or fail closed with the bash's exact stderr + exit 2.
fn load(lf: &str) -> Result<Value, Out> {
    state::read_json(lf).ok_or_else(|| Out::err(format!("job-ledger: no ledger at {}", lf), 2))
}

/// `arr -= [u]` — drop every element equal to `u`.
fn remove_all(arr: &mut Vec<Value>, u: &str) {
    arr.retain(|e| e.as_str() != Some(u));
}

/// `(arr + [u]) | unique` — append then jq-`unique` (sort + dedupe). jq sorts strings by
/// codepoint; for ASCII unit ids Rust's byte sort agrees.
fn add_unique(arr: &mut Vec<Value>, u: &str) {
    arr.push(json!(u));
    arr.sort_by(|a, b| a.as_str().unwrap_or("").cmp(b.as_str().unwrap_or("")));
    arr.dedup_by(|a, b| a.as_str() == b.as_str());
}

/// Borrow `.units.<key>` as a Vec for in-place mutation, writing back at the end.
fn units_arr<'a>(root: &'a mut Value, key: &str) -> &'a mut Vec<Value> {
    root["units"][key]
        .as_array_mut()
        .expect("units array present")
}

pub fn dispatch(argv: &[String]) -> Out {
    let cmd = argv.first().map(|s| s.as_str()).unwrap_or("status");
    let dir = argv.get(1).map(|s| s.as_str()).unwrap_or(".");
    let jid = argv.get(2).map(|s| s.as_str()).unwrap_or("");
    if jid.is_empty() {
        return Out::err("job-ledger: <job-id> required", 2);
    }
    let lf = ledger_path(dir, jid);
    let arg = |i: usize| argv.get(i).map(|s| s.as_str()).unwrap_or("");

    match cmd {
        "init" => {
            let profile = arg(3);
            let units_csv = arg(4);
            let budget = state::digits_or(arg(5), 0);
            let headroom = if arg(6).is_empty() { 15 } else { state::digits_or(arg(6), 15) };
            let units: Vec<Value> = units_csv
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| json!(s))
                .collect();
            let n = units.len();
            let doc = json!({
                "schema_version": "1.0",
                "job_id": jid,
                "profile": profile,
                "budget_total": budget,
                "headroom_pct": headroom,
                "offpeak_window": Value::Null,
                "state": "running",
                "units": { "total": n, "done": [], "remaining": units, "failed": [] },
                "context_pointers": {},
                "checkpoints": []
            });
            if state::write_json(&lf, &doc).is_err() {
                let d = dir.strip_suffix('/').unwrap_or(dir);
                return Out::err(format!("job-ledger: cannot create {}/.i2p/jobs", d), 2);
            }
            Out::ok(format!("job-ledger: initialised {} ({} units)\n", lf, n))
        }

        "mark-done" => {
            let u = arg(3);
            if u.is_empty() {
                return Out::err("job-ledger: <unit> required", 2);
            }
            let mut root = match load(&lf) {
                Ok(v) => v,
                Err(o) => return o,
            };
            remove_all(units_arr(&mut root, "remaining"), u);
            remove_all(units_arr(&mut root, "failed"), u);
            add_unique(units_arr(&mut root, "done"), u);
            let _ = state::write_json(&lf, &root);
            Out::default()
        }

        "mark-failed" => {
            let u = arg(3);
            if u.is_empty() {
                return Out::err("job-ledger: <unit> required", 2);
            }
            let mut root = match load(&lf) {
                Ok(v) => v,
                Err(o) => return o,
            };
            remove_all(units_arr(&mut root, "remaining"), u);
            add_unique(units_arr(&mut root, "failed"), u);
            let _ = state::write_json(&lf, &root);
            Out::default()
        }

        // `spend <dir> <job> <tokens>` — local token accounting against `budget_total` (Issue #2,
        // Fix 2a). Signal-INDEPENDENT: pure arithmetic on the ledger, so it bites in exactly the
        // blind condition that defeated L1 (no `.rate_limits`). Distinct from the CORE-A session cap
        // (`tf budget`): that guards a whole session; this guards ONE scheduled/off-peak job's budget.
        // Exit 10 (HALT) once cumulative spend reaches `budget_total` so the wave loop checkpoints.
        "spend" => {
            let add = state::digits_or(arg(3), 0);
            let mut root = match load(&lf) {
                Ok(v) => v,
                Err(o) => return o,
            };
            let spent = root
                .get("spent_tokens")
                .and_then(|x| x.as_i64())
                .unwrap_or(0)
                .saturating_add(add);
            root["spent_tokens"] = json!(spent);
            let budget = root
                .get("budget_total")
                .and_then(|x| x.as_i64())
                .unwrap_or(0);
            // Fail loud if the durable write fails — else the next call re-reads the old spent and
            // under-counts against the cap (the verdict would diverge from persisted state).
            if state::write_json(&lf, &root).is_err() {
                return Out::err(format!("job-ledger: cannot write {}", lf), 2);
            }
            let halt = budget > 0 && spent >= budget;
            Out::line(
                format!(
                    "{{\"spent\":{},\"budget_total\":{},\"verdict\":\"{}\"}}\n",
                    spent,
                    budget,
                    if halt { "HALT" } else { "CONTINUE" }
                ),
                if halt { 10 } else { 0 },
            )
        }

        "remaining" => {
            let root = match load(&lf) {
                Ok(v) => v,
                Err(o) => return o,
            };
            let mut s = String::new();
            if let Some(arr) = root.pointer("/units/remaining").and_then(|v| v.as_array()) {
                for e in arr {
                    if let Some(u) = e.as_str() {
                        s.push_str(u);
                        s.push('\n');
                    }
                }
            }
            Out::ok(s)
        }

        "pause" => {
            let reason = if arg(3).is_empty() { "unspecified" } else { arg(3) };
            let used = state::num_or_null(arg(4));
            let reset = state::num_or_null(arg(5));
            let spent = state::num_or_null(arg(6));
            let at = state::digits_or(arg(7), 0);
            let mut root = match load(&lf) {
                Ok(v) => v,
                Err(o) => return o,
            };
            let done_n = root.pointer("/units/done").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
            let rem_n = root.pointer("/units/remaining").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
            root["state"] = json!("paused");
            let ck = json!({
                "at": at,
                "reason": reason,
                "five_hour_pct": used,
                "resets_at": reset,
                "spent_tokens": spent,
                "units_done": done_n,
                "units_remaining": rem_n
            });
            if let Some(arr) = root["checkpoints"].as_array_mut() {
                arr.push(ck);
            }
            let _ = state::write_json(&lf, &root);
            Out::ok(format!("job-ledger: paused {} (reason={})\n", jid, reason))
        }

        "resume" => {
            let mut root = match load(&lf) {
                Ok(v) => v,
                Err(o) => return o,
            };
            root["state"] = json!("running");
            let _ = state::write_json(&lf, &root);
            Out::ok(format!("job-ledger: resumed {}\n", jid))
        }

        "set-offpeak" => {
            let start = if arg(3).is_empty() { "22:00" } else { arg(3) };
            let end = if arg(4).is_empty() { "08:00" } else { arg(4) };
            let tz_raw = arg(5);
            let tz: i64 = if !tz_raw.is_empty()
                && tz_raw.bytes().all(|b| b.is_ascii_digit() || b == b'-')
            {
                tz_raw.parse().unwrap_or(0)
            } else {
                0
            };
            let mut root = match load(&lf) {
                Ok(v) => v,
                Err(o) => return o,
            };
            root["offpeak_window"] = json!({ "start": start, "end": end, "tz_offset_min": tz });
            let _ = state::write_json(&lf, &root);
            Out::default()
        }

        "set-pointer" => {
            let key = arg(3);
            let val = arg(4);
            if key.is_empty() {
                return Out::err("job-ledger: <key> <value> required", 2);
            }
            let mut root = match load(&lf) {
                Ok(v) => v,
                Err(o) => return o,
            };
            if let Some(obj) = root["context_pointers"].as_object_mut() {
                obj.insert(key.to_string(), json!(val));
            }
            let _ = state::write_json(&lf, &root);
            Out::default()
        }

        "status" => match std::fs::read_to_string(&lf) {
            Ok(s) => Out::ok(s),
            Err(_) => Out::err(format!("job-ledger: no ledger at {}", lf), 2),
        },

        _ => Out::err(
            "usage: job-ledger.sh {init|mark-done|mark-failed|remaining|pause|resume|set-offpeak|set-pointer|status} <dir> <job-id> …",
            2,
        ),
    }
}
