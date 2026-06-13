//! spend — CORE-C of spend-safety enforcement (INV-3: no secret spend).
//!
//! The ground-truth audit. It reads the same transcript fields the Stop hook prices
//! (`session-tokens.sh`), but across ALL transcripts of a session — the main one PLUS every
//! subagent and workflow transcript — and reconciles the true total against `session.json`.
//!
//! Why this matters: the Stop hook processes only `.transcript_path` (the MAIN transcript), so
//! `session.json` SILENTLY UNDERCOUNTS fan-out spend. `tf spend` surfaces that gap instead of
//! hiding it — the exact 604,939 tokens that vanished in the incident would show here as
//! "untracked_by_session_json".
//!
//! `aggregate` is a PURE function (no IO) over transcript lines, exhaustively unit-tested; the
//! dispatch wraps it with filesystem discovery.

use crate::{state, Out};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// (input, output, cache_write, cache_read) USD per 1M tokens.
pub type PriceRow = (f64, f64, f64, f64);
/// A loaded price table: model-id prefix → its 4 rates.
pub type PriceTable = Vec<(String, PriceRow)>;

/// The 5-column model-prices.tsv canon, matching `session-tokens.sh` (which prices cache too;
/// routing.rs only keeps in/out).
fn default_prices() -> Vec<(&'static str, PriceRow)> {
    vec![
        ("claude-opus-4", (5.00, 25.00, 6.25, 0.50)),
        ("claude-sonnet-4", (3.00, 15.00, 3.75, 0.30)),
        ("claude-haiku-4", (1.00, 5.00, 1.25, 0.10)),
    ]
}

/// Load `prefix\tin\tout\tcache_write\tcache_read` rows, else the built-in default. Honours
/// `$I2P_MODEL_PRICES` then `$CLAUDE_PLUGIN_ROOT/statusline/model-prices.tsv`.
fn load_prices() -> PriceTable {
    let path = std::env::var("I2P_MODEL_PRICES").ok().or_else(|| {
        std::env::var("CLAUDE_PLUGIN_ROOT")
            .ok()
            .map(|r| format!("{}/statusline/model-prices.tsv", r.trim_end_matches('/')))
    });
    if let Some(p) = path {
        if let Ok(body) = std::fs::read_to_string(&p) {
            let mut m = Vec::new();
            for line in body.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let c: Vec<&str> = line.split('\t').collect();
                if c.len() >= 5 {
                    if let (Ok(i), Ok(o), Ok(cw), Ok(cr)) = (
                        c[1].parse::<f64>(),
                        c[2].parse::<f64>(),
                        c[3].parse::<f64>(),
                        c[4].parse::<f64>(),
                    ) {
                        m.push((c[0].to_string(), (i, o, cw, cr)));
                    }
                }
            }
            if !m.is_empty() {
                return m;
            }
        }
    }
    default_prices()
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}

/// Prefix-match a model id to its price row (`claude-opus-4-8` → `claude-opus-4`). None = unpriced.
fn price_of<'a>(prices: &'a [(String, PriceRow)], model: &str) -> Option<&'a PriceRow> {
    prices
        .iter()
        .find(|(k, _)| model.starts_with(k.as_str()))
        .map(|(_, v)| v)
}

/// Per-model running totals.
#[derive(Default, Clone, Copy, PartialEq, Debug)]
pub struct Tot {
    pub in_t: i64,
    pub out_t: i64,
    pub cache_w: i64,
    pub cache_r: i64,
    pub cost: f64,
    pub unpriced: bool,
}
impl Tot {
    pub fn tokens(&self) -> i64 {
        self.in_t + self.out_t + self.cache_w + self.cache_r
    }
}

/// PURE: fold transcript lines → per-model totals. Skips non-assistant / unparseable lines.
/// Identical token+cost arithmetic to `session-tokens.sh` (incl. cache tokens), so the audit and
/// the Stop hook agree on the main transcript and only DIVERGE by the subagent spend the hook omits.
pub fn aggregate<'a, I: IntoIterator<Item = &'a str>>(
    lines: I,
    prices: &[(String, PriceRow)],
) -> BTreeMap<String, Tot> {
    let mut out: BTreeMap<String, Tot> = BTreeMap::new();
    for line in lines {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        let msg = v.get("message");
        let model = msg
            .and_then(|m| m.get("model"))
            .and_then(|x| x.as_str())
            .unwrap_or("unknown");
        let u = msg.and_then(|m| m.get("usage"));
        let g = |k: &str| {
            u.and_then(|u| u.get(k))
                .and_then(|x| x.as_i64())
                .unwrap_or(0)
        };
        let (it, ot, cw, cr) = (
            g("input_tokens"),
            g("output_tokens"),
            g("cache_creation_input_tokens"),
            g("cache_read_input_tokens"),
        );
        let e = out.entry(model.to_string()).or_default();
        e.in_t += it;
        e.out_t += ot;
        e.cache_w += cw;
        e.cache_r += cr;
        match price_of(prices, model) {
            Some((pi, po, pcw, pcr)) => {
                e.cost += (it as f64 * pi + ot as f64 * po + cw as f64 * pcw + cr as f64 * pcr)
                    / 1_000_000.0;
            }
            None => e.unpriced = true,
        }
    }
    out
}

/// cwd → the harness project-dir slug (`/home/u/x` → `-home-u-x`), the `~/.claude/projects/<slug>` key.
fn project_slug() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().replace('/', "-"))
        .unwrap_or_default()
}

fn projects_root() -> PathBuf {
    PathBuf::from(format!("{}/.claude/projects", state::home()))
}

/// Collect every `*.jsonl` under `dir` (recursive) — main + subagents + workflows.
fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_jsonl(&p, out);
        } else if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
            out.push(p);
        }
    }
}

fn session_json_tokens() -> i64 {
    let p = format!("{}/session.json", state::state_dir());
    state::read_json(&p)
        .map(|v| state::int(&v, "tokens", 0))
        .unwrap_or(0)
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

/// `tf spend [--session <sid>] [--project-dir <path>]` — per-model audit + reconciliation.
pub fn dispatch(argv: &[String]) -> Out {
    let flag = |k: &str| -> Option<String> {
        let pfx = format!("--{}", k);
        let mut i = 0;
        while i < argv.len() {
            if argv[i] == pfx && i + 1 < argv.len() {
                return Some(argv[i + 1].clone());
            }
            if let Some(v) = argv[i].strip_prefix(&format!("{}=", pfx)) {
                return Some(v.to_string());
            }
            i += 1;
        }
        None
    };

    // `--capture` ALSO appends a `spend` event to the honesty log (P-I), for the cross-time
    // spend-by-model rollup. Wired into the Stop hook; the renderer dedups to the latest per session.
    let capture = argv.iter().any(|a| a == "--capture");
    let sid = flag("session").unwrap_or_else(session_id);
    let proj = flag("project-dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| projects_root().join(project_slug()));

    // The session's transcripts: <proj>/<sid>.jsonl (main) + everything under <proj>/<sid>/.
    let mut files: Vec<PathBuf> = Vec::new();
    if !sid.is_empty() {
        let main = proj.join(format!("{}.jsonl", sid));
        if main.is_file() {
            files.push(main);
        }
        collect_jsonl(&proj.join(&sid), &mut files);
    }
    if files.is_empty() {
        return Out::err(
            format!(
                "spend: no transcripts for session '{}' under {} (pass --project-dir/--session)",
                sid,
                proj.display()
            ),
            2,
        );
    }

    let prices = load_prices();
    let mut totals: BTreeMap<String, Tot> = BTreeMap::new();
    for f in &files {
        if let Ok(body) = std::fs::read_to_string(f) {
            for (m, t) in aggregate(body.lines(), &prices) {
                let e = totals.entry(m).or_default();
                e.in_t += t.in_t;
                e.out_t += t.out_t;
                e.cache_w += t.cache_w;
                e.cache_r += t.cache_r;
                e.cost += t.cost;
                e.unpriced |= t.unpriced;
            }
        }
    }

    let grand_tokens: i64 = totals.values().map(|t| t.tokens()).sum();
    let grand_cost: f64 = totals.values().map(|t| t.cost).sum();
    let sj = session_json_tokens();
    let untracked = grand_tokens - sj; // >0 ⇒ spend session.json missed (subagent/workflow fan-out)

    // P-I capture: append this session's per-model reading to the honesty log.
    if capture {
        let by_model: Vec<(String, i64, f64)> = totals
            .iter()
            .map(|(m, t)| (m.clone(), t.tokens(), t.cost))
            .collect();
        crate::observe::log_spend(&sid, &by_model);
    }

    // Per-model JSON array, sorted by model id (BTreeMap order).
    let by_model = totals
        .iter()
        .map(|(m, t)| {
            format!(
                "{{\"model\":\"{}\",\"tokens\":{},\"cost_usd\":{:.4}{}}}",
                m,
                t.tokens(),
                t.cost,
                if t.unpriced { ",\"unpriced\":true" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join(",");

    let line = format!(
        "{{\"session\":\"{}\",\"transcripts\":{},\"by_model\":[{}],\"total_tokens\":{},\"total_cost_usd\":{:.4},\"session_json_tokens\":{},\"untracked_by_session_json\":{}}}\n",
        sid,
        files.len(),
        by_model,
        grand_tokens,
        grand_cost,
        sj,
        untracked
    );
    Out::ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prices() -> PriceTable {
        default_prices()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect()
    }

    #[test]
    fn aggregates_per_model_with_cache_pricing() {
        let lines = vec![
            r#"{"type":"assistant","message":{"model":"claude-haiku-4-5","usage":{"input_tokens":1000,"output_tokens":500,"cache_creation_input_tokens":0,"cache_read_input_tokens":2000}}}"#,
            r#"{"type":"assistant","message":{"model":"claude-opus-4-8","usage":{"input_tokens":1000000,"output_tokens":0}}}"#,
            r#"{"type":"user","message":{"content":"ignored"}}"#,
        ];
        let t = aggregate(lines, &prices());
        // haiku: in 1000@1.0 + out 500@5.0 + cache_read 2000@0.10 = 0.001 + 0.0025 + 0.0002 = 0.0037
        let h = t.get("claude-haiku-4-5").unwrap();
        assert_eq!(h.tokens(), 3500);
        assert!((h.cost - 0.0037).abs() < 1e-9);
        // opus: 1M input @ $5/1M = $5.00 exactly
        let o = t.get("claude-opus-4-8").unwrap();
        assert!((o.cost - 5.0).abs() < 1e-9);
        // the user line contributed nothing
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn unknown_model_is_counted_but_flagged_unpriced() {
        let lines = vec![
            r#"{"type":"assistant","message":{"model":"claude-fable-5","usage":{"input_tokens":1000,"output_tokens":1000}}}"#,
        ];
        let t = aggregate(lines, &prices());
        let f = t.get("claude-fable-5").unwrap();
        assert_eq!(f.tokens(), 2000);
        assert!(f.unpriced);
        assert_eq!(f.cost, 0.0); // unpriced ⇒ tokens visible, cost 0 (never silently absorbed)
    }

    #[test]
    fn garbage_and_empty_lines_are_skipped() {
        let lines = vec!["", "not json", r#"{"type":"system"}"#];
        assert!(aggregate(lines, &prices()).is_empty());
    }

    // ---- dispatch: fixture-based (explicit --project-dir/--session, no env) ----

    fn write(p: &std::path::Path, body: &str) {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(p, body).unwrap();
    }

    /// A main transcript + a subagent transcript under `<sid>/` — exactly what `tf spend` discovers
    /// and reconciles. Returns the project dir; caller removes it.
    fn fixture_project(sid: &str) -> PathBuf {
        let proj = crate::testutil::temp_dir("spend");
        // main transcript: 1M opus input ($5.00) + a haiku turn
        write(
            &proj.join(format!("{}.jsonl", sid)),
            "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-8\",\"usage\":{\"input_tokens\":1000000,\"output_tokens\":0}}}\n\
             {\"type\":\"user\",\"message\":{\"content\":\"x\"}}\n",
        );
        // subagent transcript (under <sid>/): a fan-out turn session.json would miss
        write(
            &proj.join(sid).join("sub-1.jsonl"),
            "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-haiku-4-5\",\"usage\":{\"input_tokens\":1000,\"output_tokens\":500}}}\n",
        );
        proj
    }

    #[test]
    fn dispatch_reconciles_main_plus_subagent_transcripts() {
        // Isolate the state dir under the env lock: `untracked_by_session_json` is
        // `grand_tokens − session.json.tokens`, so the test must NOT read the developer's real
        // ~/.claude session.json (which holds a live cumulative count). An empty state dir ⇒ sj=0.
        let _g = crate::testutil::ENV_LOCK.lock().unwrap();
        let proj = fixture_project("SIDA");
        let state = crate::testutil::temp_dir("spend-state");
        std::env::set_var("I2P_COST_STATE_DIR", &state);
        let out = dispatch(&[
            "--project-dir".into(),
            proj.to_string_lossy().into_owned(),
            "--session".into(),
            "SIDA".into(),
        ]);
        std::env::remove_var("I2P_COST_STATE_DIR");
        assert_eq!(out.code, 0);
        // Two transcripts discovered (main + subagent), both models present.
        assert!(out.stdout.contains("\"transcripts\":2"));
        assert!(out.stdout.contains("claude-opus-4-8"));
        assert!(out.stdout.contains("claude-haiku-4-5"));
        // opus 1,000,000 + haiku 1,500 = 1,001,500 total tokens.
        assert!(out.stdout.contains("\"total_tokens\":1001500"));
        // session.json is absent (sj=0) ⇒ everything is "untracked" (the gap tf spend surfaces).
        assert!(out.stdout.contains("\"untracked_by_session_json\":1001500"));
        std::fs::remove_dir_all(&proj).ok();
        std::fs::remove_dir_all(&state).ok();
    }

    #[test]
    fn dispatch_errors_when_no_transcripts_for_session() {
        let proj = crate::testutil::temp_dir("spend-empty");
        let out = dispatch(&[
            "--project-dir".into(),
            proj.to_string_lossy().into_owned(),
            "--session".into(),
            "NOPE".into(),
        ]);
        assert_eq!(out.code, 2);
        assert!(out.stderr.contains("no transcripts"));
        std::fs::remove_dir_all(&proj).ok();
    }

    #[test]
    fn dispatch_capture_appends_one_spend_event() {
        let _g = crate::testutil::ENV_LOCK.lock().unwrap();
        let proj = fixture_project("SIDB");
        let evdir = crate::testutil::temp_dir("spend-ev");
        let evfile = evdir.join("honesty-events.jsonl");
        std::env::set_var("I2P_HONESTY_EVENTS", &evfile);
        let out = dispatch(&[
            "--project-dir".into(),
            proj.to_string_lossy().into_owned(),
            "--session".into(),
            "SIDB".into(),
            "--capture".into(),
        ]);
        std::env::remove_var("I2P_HONESTY_EVENTS");
        assert_eq!(out.code, 0);
        let body = std::fs::read_to_string(&evfile).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 1, "exactly one spend event appended");
        let v: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(v.get("kind").and_then(|x| x.as_str()), Some("spend"));
        assert_eq!(v.get("session").and_then(|x| x.as_str()), Some("SIDB"));
        assert_eq!(
            v.get("total_tokens").and_then(|x| x.as_i64()),
            Some(1_001_500)
        );
        std::fs::remove_dir_all(&proj).ok();
        std::fs::remove_dir_all(&evdir).ok();
    }
}
