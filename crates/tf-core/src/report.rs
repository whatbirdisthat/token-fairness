//! report — "what's scheduled, and how's the estimator doing?" Port of `report.sh`.
//!
//! Two deterministic, read-only sections: 📋 SCHEDULED JOBS (registry + each job's ledger)
//! and 📈 ESTIMATOR (the calibration convergence per profile/class). `--brief` is the
//! SessionStart dashboard — two tight lines, and SILENT when there is nothing to report.

use crate::{calibrate, fmt, observe, registry, state, Out};
use serde_json::Value;

fn signal_findings_path() -> String {
    if let Ok(p) = std::env::var("I2P_SIGNAL_FINDINGS") {
        return p;
    }
    format!("{}/signal-findings.json", state::home_cost_dir())
}

/// `jq -r` rendering of a *computed* number: integers bare, floats shortest. Used for the
/// registry/ledger integer fields the bash re-renders via `jq -r`.
fn jqnum(v: &Value) -> String {
    if v.is_i64() || v.is_u64() {
        v.to_string()
    } else if let Some(f) = v.as_f64() {
        fmt::shortest(f)
    } else {
        v.as_str().unwrap_or("").to_string()
    }
}

use crate::state::raw_field;

/// awk fmt_tok: ≥1M → "%.1fM"; ≥1000 → "%dk" (round half up); else "%d".
fn fmt_tok(t: f64) -> String {
    if t >= 1_000_000.0 {
        format!("{}M", fmt::fixed(t / 1_000_000.0, 1))
    } else if t >= 1000.0 {
        format!("{}k", fmt::round_i64(t / 1000.0))
    } else {
        format!("{}", t as i64)
    }
}

fn trend_arrow(c: &Value) -> &'static str {
    match c.get("trend").and_then(|x| x.as_str()) {
        Some("improving") => "↑",
        Some("worsening") => "↓",
        _ => "→",
    }
}

/// Read the project registry jobs by reusing `registry list`.
fn list_jobs(dir: &str) -> Vec<Value> {
    let out = registry::dispatch(&["list".to_string(), dir.to_string()]);
    serde_json::from_str::<Value>(out.stdout.trim())
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

fn calibration_keys() -> Vec<String> {
    let mut keys: Vec<String> = state::read_json(&state::calibration_file())
        .and_then(|v| v.as_object().map(|o| o.keys().cloned().collect()))
        .unwrap_or_default();
    keys.sort();
    keys
}

fn jget<'a>(j: &'a Value, k: &str) -> Option<&'a str> {
    j.get(k).and_then(|x| x.as_str())
}

fn ledger_progress_full(dir: &str, ledger_rel: &str) -> Option<String> {
    if ledger_rel.is_empty() {
        return None;
    }
    let lf = format!(
        "{}/{}",
        dir.strip_suffix('/').unwrap_or(dir),
        ledger_rel.strip_prefix("./").unwrap_or(ledger_rel)
    );
    let lv = state::read_json(&lf)?;
    let state_s = lv.get("state").and_then(|x| x.as_str()).unwrap_or("?");
    let done_n = lv
        .pointer("/units/done")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let rem_n = lv
        .pointer("/units/remaining")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let tot_n = lv
        .pointer("/units/total")
        .map(jqnum)
        .unwrap_or_else(|| "0".into());
    let ck = match lv
        .get("checkpoints")
        .and_then(|v| v.as_array())
        .and_then(|a| a.last())
    {
        Some(c) => {
            let reason = c.get("reason").and_then(|x| x.as_str()).unwrap_or("");
            let pct = match c.get("five_hour_pct") {
                Some(Value::Null) | None => String::new(),
                Some(v) => jqnum(v),
            };
            format!("{}@{}%", reason, pct)
        }
        None => "—".to_string(),
    };
    Some(format!(
        "{}/{} done · {} left · {} · last checkpoint {}",
        done_n, tot_n, rem_n, state_s, ck
    ))
}

pub fn dispatch(argv: &[String]) -> Out {
    let mut dir = ".".to_string();
    let mut mode = "full";
    for a in argv {
        match a.as_str() {
            "--scheduled" => mode = "scheduled",
            "--estimator" => mode = "estimator",
            "--kaizen" => mode = "kaizen",
            "--taxonomy" => mode = "taxonomy",
            "--honesty" => mode = "honesty",
            "--brief" => mode = "brief",
            s if s.starts_with('-') => {}
            s => dir = s.to_string(),
        }
    }

    // The Honesty Observatory (P-I) is its own cross-time surface — delegate to observe,
    // bypassing the scheduler/estimator sections this report otherwise renders.
    if mode == "honesty" {
        return Out::ok(observe::report(observe::Period::Month));
    }

    let jobs = list_jobs(&dir);
    let n_jobs = jobs.len();
    let cal_keys = calibration_keys();
    let n_cal = cal_keys.len();

    let mut s = String::new();
    let print_scheduled = |s: &mut String| {
        s.push_str(&format!("📋 Scheduled jobs ({})\n", n_jobs));
        if n_jobs == 0 {
            s.push_str("   none registered in this repo.\n");
            return;
        }
        for j in &jobs {
            let id = jget(j, "id").unwrap_or("");
            let cron = jget(j, "cron").unwrap_or("—");
            let budget = j
                .get("budget_total")
                .map(jqnum)
                .unwrap_or_else(|| "0".into());
            let armed = j.get("armed").and_then(|x| x.as_bool()).unwrap_or(false);
            let via = jget(j, "armed_via").unwrap_or("session");
            let ledger_rel = jget(j, "ledger").unwrap_or("");
            let note = jget(j, "note").unwrap_or("");
            let progress = ledger_progress_full(&dir, ledger_rel).unwrap_or_else(|| {
                "(ledger not created yet — first off-peak fire will init it)".to_string()
            });
            let budget_n: f64 = budget.parse().unwrap_or(0.0);
            s.push_str(&format!(
                "  • {} — cron \"{}\" · budget {}\n",
                id,
                cron,
                fmt_tok(budget_n)
            ));
            s.push_str(&format!("      {}\n", progress));
            if !note.is_empty() {
                s.push_str(&format!("      ↳ {}\n", note));
            }
            if armed && via == "oscron" {
                s.push_str("      ✓ OS-cron armed — fires even with Claude closed (machine awake); survives restarts.\n");
            } else if armed {
                s.push_str("      ✓ armed this session (in-session cron; re-arm needed after a restart).\n");
            } else {
                s.push_str("      ⚠ NOT armed — install OS-cron (install-oscron.sh) or re-arm the in-session cron.\n");
            }
        }
    };
    let print_estimator = |s: &mut String| {
        s.push_str(&format!("📈 Estimator convergence ({} tracked)\n", n_cal));
        if n_cal == 0 {
            s.push_str(
                "   no samples yet — estimates start at SEEDING and sharpen as jobs complete.\n",
            );
            return;
        }
        for k in &cal_keys {
            let cs = calibrate::confidence_string(k);
            let c: Value = serde_json::from_str(&cs).unwrap_or(Value::Null);
            let samples = raw_field(&cs, "samples");
            let mr = raw_field(&cs, "mean_ratio");
            let band = raw_field(&cs, "p95_band_pct");
            let tier = raw_field(&cs, "tier");
            s.push_str(&format!(
                "  {:<22} {:>3} samples · mean×{} · p95 ±{}% · {} {}\n",
                k,
                samples,
                mr,
                band,
                tier,
                trend_arrow(&c)
            ));
        }
    };

    // 🧠 KAIZEN — the self-improving ensemble made visible: champion, blend, accuracy, and which
    // formula leads each class. The "continuous improvement in numbers" surface.
    let print_kaizen = |s: &mut String| {
        s.push_str(&format!(
            "🧠 Estimator KAIZEN — ensemble scoreboard ({} classes)\n",
            n_cal
        ));
        if n_cal == 0 {
            s.push_str("   no samples yet — the field starts on the legacy EWMA-0.4 champion.\n");
            return;
        }
        for k in &cal_keys {
            let ks = calibrate::kaizen_string(k);
            let champ = raw_field(&ks, "champion");
            let cr = raw_field(&ks, "champion_ratio");
            let blend = raw_field(&ks, "blend_ratio");
            let mape = raw_field(&ks, "mape");
            let samples = raw_field(&ks, "samples");
            let acc = if mape.is_empty() || mape == "null" {
                "n/a".to_string()
            } else {
                let m: f64 = mape.parse().unwrap_or(0.0);
                format!("{}%", fmt::fixed(m * 100.0, 2))
            };
            s.push_str(&format!(
                "  {:<24} {:>3} samples · champion {} ×{} · blend ×{} · MAPE {}\n",
                k, samples, champ, cr, blend, acc
            ));
        }
        s.push_str(
            "   (champion drives the estimate; blend is the wisdom-of-the-ensemble cross-check)\n",
        );
    };
    // 🌳 TAXONOMY — the increasing-fidelity classification graph: each `/`-delimited node with its
    // own sample count + champion. Deep nodes shrink toward parents until they have enough data.
    let print_taxonomy = |s: &mut String| {
        s.push_str(&format!(
            "🌳 Estimator taxonomy — job classification graph ({} nodes)\n",
            n_cal
        ));
        if n_cal == 0 {
            s.push_str("   no classes yet.\n");
            return;
        }
        let mut keys = cal_keys.clone();
        keys.sort();
        for k in &keys {
            let depth = k.matches('/').count();
            let leaf = k.rsplit('/').next().unwrap_or(k);
            let cs = calibrate::confidence_string(k);
            let samples = raw_field(&cs, "samples");
            let band = raw_field(&cs, "p95_band_pct");
            let champ = raw_field(&calibrate::kaizen_string(k), "champion");
            s.push_str(&format!(
                "  {}{} — {} samples · ±{}% · {}\n",
                "  ".repeat(depth),
                leaf,
                samples,
                band,
                champ
            ));
        }
    };

    match mode {
        "scheduled" => print_scheduled(&mut s),
        "estimator" => print_estimator(&mut s),
        "kaizen" => print_kaizen(&mut s),
        "taxonomy" => print_taxonomy(&mut s),
        "brief" => {
            if n_jobs == 0 && n_cal == 0 {
                return Out::default();
            }
            if n_jobs > 0 {
                let j = &jobs[0];
                let id = jget(j, "id").unwrap_or("");
                let armed = j.get("armed").and_then(|x| x.as_bool()).unwrap_or(false);
                let via = jget(j, "armed_via").unwrap_or("session");
                let ledger_rel = jget(j, "ledger").unwrap_or("");
                let prog = (|| {
                    if ledger_rel.is_empty() {
                        return None;
                    }
                    let lf = format!(
                        "{}/{}",
                        dir.strip_suffix('/').unwrap_or(&dir),
                        ledger_rel.strip_prefix("./").unwrap_or(ledger_rel)
                    );
                    let lv = state::read_json(&lf)?;
                    let done_n = lv
                        .pointer("/units/done")
                        .and_then(|v| v.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    let tot = lv
                        .pointer("/units/total")
                        .map(jqnum)
                        .unwrap_or_else(|| "0".into());
                    Some(format!("{}/{} done", done_n, tot))
                })()
                .unwrap_or_else(|| "pending".to_string());
                let armstr = if armed && via == "oscron" {
                    "OS-cron armed"
                } else if armed {
                    "armed (session)"
                } else {
                    "⚠ NOT armed"
                };
                let sig = state::read_json(&signal_findings_path())
                    .and_then(|f| {
                        f.get("guard_mode")
                            .and_then(|x| x.as_str())
                            .map(|s| s.to_string())
                    })
                    .unwrap_or_else(|| "unknown".to_string());
                let extra = if n_jobs > 1 {
                    format!(" (+{} more)", n_jobs - 1)
                } else {
                    String::new()
                };
                s.push_str(&format!(
                    "🛡️ Scheduler · {} {} · {} · signal: {}{}\n",
                    id, prog, armstr, sig, extra
                ));
            }
            if n_cal > 0 {
                let mut line = String::new();
                for k in cal_keys.iter().take(2) {
                    let cs = calibrate::confidence_string(k);
                    let c: Value = serde_json::from_str(&cs).unwrap_or(Value::Null);
                    let b = raw_field(&cs, "p95_band_pct");
                    let t = raw_field(&cs, "tier");
                    line.push_str(&format!("{} ±{}% {} {} · ", k, b, t, trend_arrow(&c)));
                }
                let line = line.strip_suffix(" · ").unwrap_or(&line);
                s.push_str(&format!("📈 Estimator · {} profiles · {}\n", n_cal, line));
            }
        }
        _ => {
            print_scheduled(&mut s);
            s.push('\n');
            print_estimator(&mut s);
        }
    }
    Out::ok(s)
}
