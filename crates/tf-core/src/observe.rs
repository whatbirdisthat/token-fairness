//! observe — P-I The Honesty Observatory (doc/design/spend-safety-enforcement.md §P-I).
//!
//! The cross-time efficacy record: how often the guard ACTUALLY prevented an overspend (SAVE),
//! how often we BLEW the limit anyway, and what the guard COST (friction). It must be honest —
//! failures reported as loudly as wins — or it is the vanity dashboard we are replacing.
//!
//! Two halves:
//!   * CAPTURE — an append-only event log (`honesty-events.jsonl`) plus the HONEST classification
//!     of a gate decision. Metadata only — prompt content is NEVER written (HON-5).
//!   * ROLLUP — `tf observe --period day|week|month [--write <dir>]` folds the log into a
//!     longitudinal markdown + Mermaid report, regenerated idempotently into `doc/honesty/`.
//!     The headline is the **#SAVES vs #BLOWN** efficacy ratio, rendered with equal prominence
//!     (HON-1). The pure fold (`fold_events`) is exhaustively unit-tested; dispatch does the IO.
//!
//! NOTE on data not yet captured: per-event `model`/`tokens`/`cost` are not yet recorded, so the
//! spec's spend-by-model × period and MAPE-over-time tables await a capture extension (a `spend`
//! event kind appended at session end). Until then those sections declare the gap rather than fake
//! it — the same honesty rule (HON-1) that forbids hiding BLOWNs forbids inventing spend.

use crate::{state, Out};
use serde_json::Value;
use std::collections::BTreeMap;

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

/// Append a **BLOWN** event — a limit/lockout we hit ANYWAY (the guard failed). The honest
/// counterweight to a SAVE; the renderer reports it with equal prominence (HON-1). Best-effort,
/// metadata only. `reason` is the failure (e.g. "5h-window exhausted", "paid lockout").
pub fn log_blown(reason: &str) {
    let ev = serde_json::json!({
        "ts": state::now_epoch(),
        "session": session_id(),
        "kind": "blown",
        "class": "blown",
        "reason": reason,
    });
    let _ = state::append_line(&events_path(), &ev.to_string());
}

// ---------------------------------------------------------------------------------------------
// ROLLUP — pure fold over the event log, then markdown + Mermaid renderers.
// ---------------------------------------------------------------------------------------------

/// The period a rollup buckets events into.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Period {
    Day,
    Week,
    Month,
}
impl Period {
    pub fn parse(s: &str) -> Option<Period> {
        match s {
            "day" => Some(Period::Day),
            "week" => Some(Period::Week),
            "month" => Some(Period::Month),
            _ => None,
        }
    }
    fn label(&self) -> &'static str {
        match self {
            Period::Day => "day",
            Period::Week => "week",
            Period::Month => "month",
        }
    }
}

/// Civil (y, m, d) from days since the Unix epoch — Howard Hinnant's integer algorithm, exact
/// for all dates, no floating point, no clock. `z` is `ts.div_euclid(86400)`.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The bucket key a timestamp falls in, for a given period. Day → `YYYY-MM-DD`; week → the
/// `YYYY-MM-DD` of that ISO week's Monday; month → `YYYY-MM`. Sorts lexically into time order.
pub fn period_key(ts: i64, period: Period) -> String {
    let day = ts.div_euclid(86_400);
    match period {
        Period::Day => {
            let (y, m, d) = civil_from_days(day);
            format!("{:04}-{:02}-{:02}", y, m, d)
        }
        Period::Week => {
            // Epoch day 0 is a Thursday; ISO Monday of the week containing `day`.
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

/// One period's tallies. SAVE and BLOWN sit side by side, equal prominence (HON-1).
#[derive(Default, Clone, Debug, PartialEq)]
pub struct Bucket {
    pub saves: i64,
    pub blown: i64,
    pub procedural: i64,
    pub allows: i64,
    pub est_sum: i64, // estimated tokens guarded (the friction the gate weighed), not actual spend
    pub est_n: i64,
}

/// A complete rollup: per-period buckets (time-ordered), a deny-reason histogram, and a
/// decision-class mix for the pie. Pure — built only from event lines.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct Rollup {
    pub buckets: BTreeMap<String, Bucket>,
    pub by_reason: BTreeMap<String, i64>, // denies (save + procedural) keyed by reason
    pub other: i64,
}
impl Rollup {
    pub fn totals(&self) -> Bucket {
        let mut t = Bucket::default();
        for b in self.buckets.values() {
            t.saves += b.saves;
            t.blown += b.blown;
            t.procedural += b.procedural;
            t.allows += b.allows;
            t.est_sum += b.est_sum;
            t.est_n += b.est_n;
        }
        t
    }
}

/// PURE: fold event-log lines into a `Rollup` bucketed by `period`. Skips unparseable lines.
/// Mirrors the `spend::aggregate` pure-fold pattern — dispatch supplies the IO.
pub fn fold_events<'a, I: IntoIterator<Item = &'a str>>(lines: I, period: Period) -> Rollup {
    let mut r = Rollup::default();
    for line in lines {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let ts = state::int(&v, "ts", 0);
        let key = period_key(ts, period);
        let b = r.buckets.entry(key).or_default();
        let reason = v.get("reason").and_then(|x| x.as_str()).unwrap_or("");
        match v.get("kind").and_then(|x| x.as_str()) {
            Some("gate") => {
                let est = state::int(&v, "est", 0);
                match v.get("class").and_then(|x| x.as_str()) {
                    Some("save") => {
                        b.saves += 1;
                        b.est_sum += est;
                        b.est_n += 1;
                        *r.by_reason.entry(reason.to_string()).or_default() += 1;
                    }
                    Some("procedural-deny") => {
                        b.procedural += 1;
                        *r.by_reason.entry(reason.to_string()).or_default() += 1;
                    }
                    Some("allow") => {
                        b.allows += 1;
                        b.est_sum += est;
                        b.est_n += 1;
                    }
                    _ => r.other += 1,
                }
            }
            Some("blown") => b.blown += 1,
            _ => r.other += 1,
        }
    }
    r
}

/// The headline efficacy ratio, honestly rendered. "n/a" when there is nothing to weigh.
fn saves_vs_blown(t: &Bucket) -> String {
    if t.saves == 0 && t.blown == 0 {
        return "no guard events yet".to_string();
    }
    format!(
        "**{} SAVE{}** vs **{} BLOWN**",
        t.saves,
        if t.saves == 1 { "" } else { "s" },
        t.blown
    )
}

/// Render the full markdown report for a period (tables + Mermaid). Pure.
pub fn render_markdown(r: &Rollup, period: Period) -> String {
    let t = r.totals();
    let mut s = String::new();
    s.push_str("# The Honesty Observatory\n\n");
    s.push_str(
        "> Cross-time efficacy of the token-fairness guard. The honesty rule (HON-1): the BLOWN \
         count is reported as prominently as the SAVES — a tool that hides its own failures is the \
         thing we are replacing.\n\n",
    );
    s.push_str(&format!("**Headline:** {}\n\n", saves_vs_blown(&t)));
    s.push_str(&format!(
        "- SAVEs (overspend genuinely prevented): **{}**\n- BLOWNs (limit/lockout hit anyway): \
         **{}**\n- Procedural denies (no-budget / not-armed): {}\n- Fan-out allows: {}\n",
        t.saves, t.blown, t.procedural, t.allows
    ));
    let avg = if t.est_n > 0 { t.est_sum / t.est_n } else { 0 };
    s.push_str(&format!(
        "- Est. tokens guarded (sum / avg per event): {} / {}\n\n",
        t.est_sum, avg
    ));

    // Per-period table.
    s.push_str(&format!("## By {}\n\n", period.label()));
    s.push_str("| Period | SAVEs | BLOWNs | Procedural | Allows | Est. guarded |\n");
    s.push_str("|---|---:|---:|---:|---:|---:|\n");
    for (k, b) in &r.buckets {
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            k, b.saves, b.blown, b.procedural, b.allows, b.est_sum
        ));
    }
    s.push('\n');

    // Denies by reason.
    s.push_str("## Denies by reason\n\n");
    if r.by_reason.is_empty() {
        s.push_str("_No denies recorded._\n\n");
    } else {
        s.push_str("| Reason | Count |\n|---|---:|\n");
        for (reason, n) in &r.by_reason {
            s.push_str(&format!("| {} | {} |\n", reason.replace('|', "\\|"), n));
        }
        s.push('\n');
    }

    // Mermaid: decision mix (pie) + SAVES-vs-BLOWN trend (xychart-beta).
    s.push_str(&render_mermaid(r, &t));

    // Honest declaration of the not-yet-captured sections.
    s.push_str(
        "\n## Spend by model · MAPE over time\n\n_Pending a capture extension._ Per-event \
         `model`/`tokens`/`cost` are not yet written to the event log, so cross-time spend-by-model \
         and estimate-vs-actual (MAPE) cannot be rendered honestly here yet. They land when a \
         session-end hook appends a `spend` event (design §P-I). Per-session spend is available now \
         via `tf spend`; estimator accuracy via `tf report --estimator`.\n",
    );
    s
}

fn render_mermaid(r: &Rollup, t: &Bucket) -> String {
    let mut s = String::new();
    // Decision-mix pie — only non-zero slices (an all-zero pie renders empty).
    let slices = [
        ("SAVE", t.saves),
        ("BLOWN", t.blown),
        ("procedural-deny", t.procedural),
        ("allow", t.allows),
    ];
    if slices.iter().any(|(_, n)| *n > 0) {
        s.push_str("## Decision mix\n\n```mermaid\npie title Decision mix\n");
        for (name, n) in slices {
            if n > 0 {
                s.push_str(&format!("    \"{}\" : {}\n", name, n));
            }
        }
        s.push_str("```\n\n");
    }
    // SAVES-vs-BLOWN trend — two line series over the period buckets.
    if !r.buckets.is_empty() {
        let ymax = r
            .buckets
            .values()
            .map(|b| b.saves.max(b.blown))
            .max()
            .unwrap_or(1)
            .max(1);
        let xs: Vec<String> = r.buckets.keys().cloned().collect();
        let saves: Vec<String> = r.buckets.values().map(|b| b.saves.to_string()).collect();
        let blown: Vec<String> = r.buckets.values().map(|b| b.blown.to_string()).collect();
        s.push_str("## SAVES vs BLOWN trend\n\n```mermaid\nxychart-beta\n");
        s.push_str("    title \"SAVES (line 1) vs BLOWN (line 2)\"\n");
        s.push_str(&format!("    x-axis [{}]\n", xs.join(", ")));
        s.push_str(&format!("    y-axis \"count\" 0 --> {}\n", ymax));
        s.push_str(&format!("    line [{}]\n", saves.join(", ")));
        s.push_str(&format!("    line [{}]\n", blown.join(", ")));
        s.push_str("```\n");
    }
    s
}

/// The legacy flat tally (`tf observe` with no args) — kept for back-compat. JSON one-liner.
fn tally(body: &str) -> String {
    let (mut saves, mut procedural, mut allows, mut blown, mut other) = (0, 0, 0, 0, 0);
    for line in body.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
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
    format!(
        "{{\"saves\":{},\"blown\":{},\"procedural_denies\":{},\"fanout_allows\":{},\"other\":{}}}\n",
        saves, blown, procedural, allows, other
    )
}

/// `tf report --honesty` entry — render the markdown rollup for `period` to a string.
pub fn report(period: Period) -> String {
    let body = std::fs::read_to_string(events_path()).unwrap_or_default();
    render_markdown(&fold_events(body.lines(), period), period)
}

/// `tf observe [--period day|week|month] [--write <dir>]`.
///   * no flags            → the legacy JSON tally (back-compat).
///   * --period P          → the markdown rollup to stdout.
///   * --write DIR         → also write `DIR/honesty.md` (idempotent regen); implies a rollup.
pub fn dispatch(argv: &[String]) -> Out {
    let mut period: Option<Period> = None;
    let mut write_dir: Option<String> = None;
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--period" => {
                let Some(p) = argv.get(i + 1).and_then(|s| Period::parse(s)) else {
                    return Out::err("observe: --period needs day|week|month", 2);
                };
                period = Some(p);
                i += 2;
            }
            "--write" => {
                let Some(d) = argv.get(i + 1) else {
                    return Out::err("observe: --write needs a directory", 2);
                };
                write_dir = Some(d.clone());
                i += 2;
            }
            _ => i += 1,
        }
    }

    let body = std::fs::read_to_string(events_path()).unwrap_or_default();

    // Back-compat: no rollup flags ⇒ the original JSON tally.
    if period.is_none() && write_dir.is_none() {
        return Out::ok(tally(&body));
    }

    let period = period.unwrap_or(Period::Month);
    let md = render_markdown(&fold_events(body.lines(), period), period);

    if let Some(dir) = write_dir {
        let path = format!("{}/honesty.md", dir.trim_end_matches('/'));
        if let Err(e) = state::write_atomic(&path, &md) {
            return Out::err(format!("observe: cannot write {}: {}", path, e), 1);
        }
        return Out::ok(format!("wrote {}\n", path));
    }
    Out::ok(md)
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

    #[test]
    fn civil_date_pins() {
        // 1970-01-01 is epoch day 0; a few anchors incl. a leap day.
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(18_993), (2022, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        // 2026-06-12 (the repo's "today") — day number 20616.
        assert_eq!(civil_from_days(20_616), (2026, 6, 12));
    }

    #[test]
    fn period_keys_bucket_by_day_week_month() {
        let ts = 20_616 * 86_400 + 3600; // 2026-06-12, mid-morning UTC (a Friday)
        assert_eq!(period_key(ts, Period::Day), "2026-06-12");
        assert_eq!(period_key(ts, Period::Month), "2026-06");
        // The Monday of that week is 2026-06-08.
        assert_eq!(period_key(ts, Period::Week), "2026-06-08");
    }

    #[test]
    fn fold_tallies_saves_blown_and_reasons_into_buckets() {
        let day1 = 20_616 * 86_400;
        let day2 = 20_617 * 86_400;
        let lines = [
            // day1: one genuine SAVE + one procedural deny + one allow
            format!(
                r#"{{"ts":{},"kind":"gate","class":"save","reason":"exceeds-per-fanout-cap","est":120000}}"#,
                day1
            ),
            format!(
                r#"{{"ts":{},"kind":"gate","class":"procedural-deny","reason":"no declared budget"}}"#,
                day1
            ),
            format!(
                r#"{{"ts":{},"kind":"gate","class":"allow","reason":"armed","est":40000}}"#,
                day1
            ),
            // day2: a BLOWN — the guard failed, reported with equal prominence
            format!(
                r#"{{"ts":{},"kind":"blown","reason":"5h-window exhausted"}}"#,
                day2
            ),
        ];
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let r = fold_events(refs, Period::Day);
        let t = r.totals();
        assert_eq!((t.saves, t.blown, t.procedural, t.allows), (1, 1, 1, 1));
        // est is summed over saves + allows only (120k + 40k); n = 2.
        assert_eq!((t.est_sum, t.est_n), (160_000, 2));
        // Two day-buckets, time-ordered.
        let keys: Vec<&String> = r.buckets.keys().collect();
        assert_eq!(keys, vec!["2026-06-12", "2026-06-13"]);
        // The SAVE and the procedural deny both land in the reason histogram; the allow does not.
        assert_eq!(r.by_reason.get("exceeds-per-fanout-cap"), Some(&1));
        assert_eq!(r.by_reason.get("no declared budget"), Some(&1));
        assert_eq!(r.by_reason.get("armed"), None);
    }

    #[test]
    fn markdown_reports_blown_as_prominently_as_saves() {
        let day = 20_616 * 86_400;
        let lines = [
            format!(
                r#"{{"ts":{},"kind":"gate","class":"save","reason":"ceiling","est":100000}}"#,
                day
            ),
            format!(r#"{{"ts":{},"kind":"blown","reason":"paid lockout"}}"#, day),
        ];
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let md = render_markdown(&fold_events(refs, Period::Day), Period::Day);
        // HON-1: both counts present in the headline, BLOWN never hidden.
        assert!(md.contains("1 SAVE"));
        assert!(md.contains("1 BLOWN"));
        // The not-yet-captured spend section is declared, not faked.
        assert!(md.contains("Pending a capture extension"));
        // Mermaid surfaces render.
        assert!(md.contains("```mermaid"));
        assert!(md.contains("xychart-beta"));
    }

    #[test]
    fn empty_log_renders_honest_zero_report() {
        let r = fold_events(Vec::<&str>::new(), Period::Month);
        let md = render_markdown(&r, Period::Month);
        assert!(md.contains("no guard events yet"));
        // An empty rollup emits no Mermaid (an all-zero pie/trend would render blank).
        assert!(!render_mermaid(&r, &r.totals()).contains("mermaid"));
    }

    #[test]
    fn period_parse_and_log_blown_are_well_formed() {
        assert_eq!(Period::parse("day"), Some(Period::Day));
        assert_eq!(Period::parse("week"), Some(Period::Week));
        assert_eq!(Period::parse("month"), Some(Period::Month));
        assert_eq!(Period::parse("year"), None);
        assert_eq!(Period::Week.label(), "week");
        // log_blown emits a parseable kind:"blown" line the fold counts as a BLOWN.
        let line =
            serde_json::json!({"ts":0,"kind":"blown","class":"blown","reason":"x"}).to_string();
        assert_eq!(fold_events([line.as_str()], Period::Day).totals().blown, 1);
    }

    #[test]
    fn headline_pluralizes_saves_and_tally_is_back_compat() {
        // 2 saves → plural "SAVEs"; the headline never hides the BLOWN beside it.
        let b = Bucket {
            saves: 2,
            blown: 3,
            ..Default::default()
        };
        let s = saves_vs_blown(&b);
        assert!(s.contains("2 SAVEs"));
        assert!(s.contains("3 BLOWN"));
        // The legacy one-liner tally stays byte-stable for any back-compat caller.
        let body = "{\"kind\":\"gate\",\"class\":\"save\"}\n{\"kind\":\"blown\"}\nnot-json\n";
        assert_eq!(
            tally(body),
            "{\"saves\":1,\"blown\":1,\"procedural_denies\":0,\"fanout_allows\":0,\"other\":0}\n"
        );
    }
}
