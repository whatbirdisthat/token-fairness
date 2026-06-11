//! Self-contained frozen-vector conformance for the STATEFUL + orchestration tier
//! (ledger / registry / snapshot / signal / report / gate / plan / preflight / oscron).
//! Vectors were captured from the bash oracle via `tests/conformance.sh` and frozen here so
//! `cargo test` is a standalone CI gate — no bash, no real crontab, no network.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn tf() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tf"))
}

/// Unique isolated dir per (test, pid) — auto-namespaced so tests don't collide.
fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("tf-stateful-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn run(args: &[&str], stdin: &str, envs: &[(&str, &str)]) -> (String, i32) {
    let mut cmd = tf();
    cmd.args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

fn line(args: &[&str], stdin: &str, envs: &[(&str, &str)], want: &str, code: i32) {
    let (got, rc) = run(args, stdin, envs);
    assert_eq!(got.trim_end_matches('\n'), want, "args={:?}", args);
    assert_eq!(rc, code, "exit for args={:?}", args);
}

#[test]
fn ledger_lifecycle() {
    let d = tmp("ledger");
    let ds = d.to_str().unwrap();
    line(
        &[
            "ledger", "init", ds, "j", "reviewer", "a,b,c", "500000", "15",
        ],
        "",
        &[],
        &format!("job-ledger: initialised {}/.i2p/jobs/j.json (3 units)", ds),
        0,
    );
    line(&["ledger", "mark-done", ds, "j", "b"], "", &[], "", 0);
    line(&["ledger", "remaining", ds, "j"], "", &[], "a\nc", 0);
    // status is the full ledger doc (pretty, jq-insertion-order); compare canonicalised.
    let (got, rc) = run(&["ledger", "status", ds, "j"], "", &[]);
    assert_eq!(rc, 0);
    let v: serde_json::Value = serde_json::from_str(&got).unwrap();
    assert_eq!(v["state"], "running");
    assert_eq!(v["units"]["done"], serde_json::json!(["b"]));
    assert_eq!(v["units"]["remaining"], serde_json::json!(["a", "c"]));
    assert_eq!(v["units"]["total"], 3);
    // missing ledger → exit 2
    let (_o, rc) = run(&["ledger", "status", ds, "ghost"], "", &[]);
    assert_eq!(rc, 2);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn ledger_status_is_pretty_on_disk() {
    let d = tmp("ledgerdisk");
    let ds = d.to_str().unwrap();
    run(&["ledger", "init", ds, "j", "p", "x,y", "0", "15"], "", &[]);
    let f = d.join(".i2p/jobs/j.json");
    let body = std::fs::read_to_string(&f).unwrap();
    assert!(
        body.ends_with('\n'),
        "state file ends with newline (jq parity)"
    );
    assert!(
        body.contains("  \"job_id\": \"j\""),
        "2-space pretty-print like jq"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn registry_dual_scope() {
    let d = tmp("registry");
    let ds = d.to_str().unwrap();
    let machine = d.join("machine.json");
    let env = [("I2P_MACHINE_REGISTRY", machine.to_str().unwrap())];
    line(
        &[
            "registry",
            "register",
            ds,
            "j1",
            "17 22 * * *",
            "300000",
            "./l.json",
            "./p.txt",
            "note",
        ],
        "",
        &env,
        "jobs-registry: registered j1 (project + machine index)",
        0,
    );
    line(
        &["registry", "list", ds],
        "",
        &env,
        r#"[{"id":"j1","cron":"17 22 * * *","recurring":true,"budget_total":300000,"ledger":"./l.json","prompt_file":"./p.txt","note":"note","armed":false}]"#,
        0,
    );
    line(
        &["registry", "get", ds, "j1"],
        "",
        &env,
        r#"{"id":"j1","cron":"17 22 * * *","recurring":true,"budget_total":300000,"ledger":"./l.json","prompt_file":"./p.txt","note":"note","armed":false}"#,
        0,
    );
    line(&["registry", "get", ds, "nope"], "", &env, "{}", 0);
    // arm oscron then reset-armed keeps it (durable); session arming would be cleared.
    run(&["registry", "arm", ds, "j1", "oscron"], "", &env);
    run(&["registry", "reset-armed", ds], "", &env);
    let (got, _) = run(&["registry", "get", ds, "j1"], "", &env);
    assert!(got.contains(r#""armed":true"#) && got.contains(r#""armed_via":"oscron""#));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn snapshot_only_on_signal() {
    let d = tmp("snapshot");
    let env = [
        ("I2P_COST_STATE_DIR", d.to_str().unwrap()),
        ("I2P_CLOCK", "1700000000"),
    ];
    // no rate_limits → no-op, no file
    run(&["snapshot"], r#"{"hello":1}"#, &env);
    assert!(!d.join("ratelimit-snapshot.json").exists());
    // with signal → writes the pinned-clock snapshot
    run(
        &["snapshot"],
        r#"{"hook_event_name":"PostToolUse","rate_limits":{"five_hour":{"used_percentage":42,"resets_at":1749635640}}}"#,
        &env,
    );
    let snap = std::fs::read_to_string(d.join("ratelimit-snapshot.json")).unwrap();
    assert_eq!(
        snap.trim_end(),
        r#"{"captured_at":1700000000,"rate_limits":{"five_hour":{"used_percentage":42,"resets_at":1749635640}},"cost":{}}"#
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn signal_probe_conclude_report() {
    let d = tmp("signal");
    let probe = d.join("payload-probe.jsonl");
    let findings = d.join("sf.json");
    let env = [
        ("I2P_COST_STATE_DIR", d.to_str().unwrap()),
        ("I2P_PAYLOAD_PROBE", probe.to_str().unwrap()),
        ("I2P_SIGNAL_FINDINGS", findings.to_str().unwrap()),
        ("I2P_CLOCK", "1700000000"),
    ];
    run(
        &["verify-payload"],
        r#"{"hook_event_name":"PreToolUse","rate_limits":{"five_hour":{"used_percentage":50}}}"#,
        &env,
    );
    line(
        &["signal", "conclude"],
        "",
        &env,
        &format!(
            "signal-probe: concluded → hook-signal-available (guard: live-ceiling); written to {}",
            findings.to_str().unwrap()
        ),
        0,
    );
    line(&["signal", "verdict"], "", &env, "hook-signal-available", 0);
    line(&["signal", "report"], "", &env,
        "🔎 Live-signal probe — verdict: hook-signal-available  (guard mode: live-ceiling)\n   PreToolUse: 1 fires · rate_limits in 1\n   At least one hook event carries .rate_limits — the snapshot bridge can feed the live ceiling guard.", 0);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn gate_verdicts() {
    let d = tmp("gate");
    let env = [(
        "I2P_COST_STATE_DIR",
        d.join("empty").to_str().unwrap().to_string(),
    )];
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    // CLEAR → CONTINUE
    line(
        &["gate"],
        r#"{"rate_limits":{"five_hour":{"used_percentage":42.5,"resets_at":1749635640},"seven_day":{"used_percentage":18.2,"resets_at":1750000000}}}"#,
        &env,
        r#"{"verdict":"CONTINUE","ceiling":{"verdict":"CLEAR","window":"five_hour","used_pct":42.5,"ceiling":85,"headroom":15,"resets_at":1749635640},"offpeak":null}"#,
        0,
    );
    // HALT (five breaches, seven clear)
    line(
        &["gate"],
        r#"{"rate_limits":{"five_hour":{"used_percentage":92,"resets_at":1750000000},"seven_day":{"used_percentage":10,"resets_at":1760000000}}}"#,
        &env,
        r#"{"verdict":"HALT","ceiling":{"verdict":"HALT","window":"five_hour","used_pct":92,"ceiling":85,"headroom":15,"resets_at":1750000000}}"#,
        10,
    );
    // no signal, no fresh snapshot → ASK (fail closed)
    line(
        &["gate"],
        "{}",
        &env,
        r#"{"verdict":"ASK","reason":"no-live-signal","ceiling":{"verdict":"NO_SIGNAL","window":"seven_day","used_pct":null,"ceiling":85,"headroom":15,"resets_at":null}}"#,
        20,
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn convergence_loop_advances() {
    let d = tmp("conv");
    let sess = d.join("s.json");
    let pop = d.join("po.json");
    let cal = d.join("c.json");
    let env = [
        ("I2P_SESSION_FILE", sess.to_str().unwrap()),
        ("I2P_PLANOPEN_FILE", pop.to_str().unwrap()),
        ("I2P_CALIBRATION_FILE", cal.to_str().unwrap()),
    ];
    std::fs::write(&sess, r#"{"tokens":1000}"#).unwrap();
    line(
        &["plan-open", "medium", "80000"],
        "",
        &env,
        r#"{"opened":"plan:medium","est":80000,"baseline_tokens":1000}"#,
        0,
    );
    std::fs::write(&sess, r#"{"tokens":85000}"#).unwrap();
    line(
        &["plan-close"],
        "",
        &env,
        r#"{"class":"plan:medium","est":80000,"actual":84000,"convergence":{"samples":1,"mean_ratio":1.0500,"sd":0.0000,"p95_band_pct":50.0,"tier":"CALIBRATING","prev_band":60.0,"trend":"improving"}}"#,
        0,
    );
    // the EWMA actually folded a sample
    let calbody = std::fs::read_to_string(&cal).unwrap();
    assert!(calbody.contains(r#""samples": 1"#));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn preflight_and_fanout() {
    let d = tmp("preflight");
    let env = [(
        "I2P_CALIBRATION_FILE",
        d.join("none.json").to_str().unwrap().to_string(),
    )];
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    line(
        &["preflight", "--class", "large"],
        "",
        &env,
        r#"{"verdict":"PROBE","estimate":{"name":"plan:large","per_unit":250000,"basis":"class","confidence":"low","fanout":1,"ratio":1.0,"est_total":250000,"convergence":{"samples":0,"mean_ratio":1.0000,"sd":0.0000,"p95_band_pct":60.0,"tier":"SEEDING","prev_band":-1.0,"trend":"flat"},"interval":[100000,400000]}}"#,
        3,
    );
    // PreToolUse deny JSON on a HALT payload
    let dempty = d.join("empty");
    let env2 = [("I2P_COST_STATE_DIR", dempty.to_str().unwrap())];
    line(
        &["preflight-fanout"],
        r#"{"rate_limits":{"five_hour":{"used_percentage":92,"resets_at":1750000000},"seven_day":{"used_percentage":10,"resets_at":1760000000}}}"#,
        &env2,
        r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"Token ceiling reached (live window at 92%). Spawning more agents now risks a lockout. Pause this job (job-ledger.sh pause) and resume when the window resets — /concierge:schedule."}}"#,
        0,
    );
    // clean payload → no deny, no output
    line(
        &["preflight-fanout"],
        r#"{"rate_limits":{"five_hour":{"used_percentage":40,"resets_at":1750000000},"seven_day":{"used_percentage":10,"resets_at":1760000000}}}"#,
        &env2,
        "",
        0,
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn cognition_routing() {
    let d = tmp("route");
    let env = [(
        "I2P_CALIBRATION_FILE",
        d.join("none.json").to_str().unwrap().to_string(),
    )];
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    // discernment → sonnet; large = 250k tokens; 0.7 in-frac → 175k·$3 + 75k·$15 = $1.65
    line(
        &["route", "--cognition", "discernment", "--class", "large"],
        "",
        &env,
        r#"{"name":"plan:large","cognition_class":"discernment","best_fit_tier":"sonnet","model":"claude-sonnet-4","est_total":250000,"interval":[100000,400000],"cost_usd":1.65,"cost_band":[0.66,2.64],"per_tier_usd":{"haiku":0.55,"sonnet":1.65,"opus":2.75},"in_frac":0.7}"#,
        0,
    );
    // escalation bumps discernment → opus (false-PASS propagates)
    let (got, _) = run(
        &[
            "route",
            "--cognition",
            "discernment",
            "--escalate",
            "--class",
            "large",
        ],
        "",
        &env,
    );
    assert!(got.contains(r#""best_fit_tier":"opus""#) && got.contains(r#""cost_usd":2.75"#));
    // mechanical → haiku
    let (got, _) = run(
        &["route", "--cognition", "mechanical", "--class", "medium"],
        "",
        &env,
    );
    assert!(got.contains(r#""best_fit_tier":"haiku""#));
    // determinative leaves the token economy: 0 tokens, 0 $ (no handler given → null)
    line(
        &["route", "--cognition", "determinative", "--class", "large"],
        "",
        &env,
        r#"{"name":"plan:large","cognition_class":"determinative","best_fit_tier":"none","model":null,"determinative_handler":null,"est_total":0,"cost_usd":0,"note":"determinative_handler — 0 model tokens; runs as a tested tf/client handler"}"#,
        0,
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// The shipped profiles dir, relative to this test crate.
fn profiles_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/scheduler/profiles")
}

fn pjson(v: &str) -> serde_json::Value {
    serde_json::from_str(v.trim()).unwrap()
}

// ── Track 2: profile-driven cognition routing ────────────────────────────────────────────
#[test]
fn route_reads_cognition_class_from_profile() {
    let rev = profiles_dir().join("reviewer-fanout.json");
    let rev = rev.to_str().unwrap();
    // No --cognition: the class comes from the profile (discernment → sonnet).
    let (got, _) = run(&["route", "--profile", rev, "--width", "26"], "", &[]);
    let v = pjson(&got);
    assert_eq!(v["cognition_class"], "discernment");
    assert_eq!(v["best_fit_tier"], "sonnet");
    assert_eq!(v["model"], "claude-sonnet-4");
    // The --cognition flag still overrides the profile.
    let (got, _) = run(
        &[
            "route",
            "--profile",
            rev,
            "--cognition",
            "mechanical",
            "--width",
            "26",
        ],
        "",
        &[],
    );
    assert_eq!(pjson(&got)["best_fit_tier"], "haiku");
}

#[test]
fn route_determinative_handler_from_profile() {
    let lint = profiles_dir().join("lint-fanout.json");
    let lint = lint.to_str().unwrap();
    // A determinative profile → tier none, the handler path, 0 tokens, $0.
    let (got, rc) = run(&["route", "--profile", lint, "--width", "8"], "", &[]);
    assert_eq!(rc, 0);
    let v = pjson(&got);
    assert_eq!(v["cognition_class"], "determinative");
    assert_eq!(v["best_fit_tier"], "none");
    assert_eq!(v["model"], serde_json::Value::Null);
    assert_eq!(v["determinative_handler"], "./scripts/lint-check.sh");
    assert_eq!(v["est_total"], 0);
    assert_eq!(v["cost_usd"], 0);
}

// ── Track 3: profile-by-name resolution (.i2p/job-profiles/ then ${CLAUDE_PLUGIN_ROOT}/profiles/) ──
#[test]
fn profile_by_name_resolution() {
    let plugin_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/scheduler");
    let plugin_root = plugin_root.canonicalize().unwrap();
    // Bare name → resolves the shipped ${CLAUDE_PLUGIN_ROOT}/profiles/<name>.json.
    let (got, _) = run(
        &["route", "--profile", "reviewer-fanout", "--width", "26"],
        "",
        &[("CLAUDE_PLUGIN_ROOT", plugin_root.to_str().unwrap())],
    );
    let v = pjson(&got);
    assert_eq!(v["name"], "reviewer-fanout");
    assert_eq!(v["cognition_class"], "discernment");

    // A project override at .i2p/job-profiles/<name>.json WINS over the shipped copy.
    let d = tmp("profover");
    std::fs::create_dir_all(d.join(".i2p/job-profiles")).unwrap();
    std::fs::write(
        d.join(".i2p/job-profiles/reviewer-fanout.json"),
        r#"{"name":"reviewer-fanout","cognition_class":"thought-intensive","estimated_unit_tokens":50000}"#,
    )
    .unwrap();
    let mut cmd = tf();
    cmd.args(["route", "--profile", "reviewer-fanout", "--width", "1"])
        .current_dir(&d)
        .env("CLAUDE_PLUGIN_ROOT", plugin_root.to_str().unwrap())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let out = cmd.output().unwrap();
    let v = pjson(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(v["cognition_class"], "thought-intensive"); // override applied
    assert_eq!(v["est_total"], 50000); // override's unit tokens (×1 ×ratio 1.0)
    let _ = std::fs::remove_dir_all(&d);
}

// ── Track 1: DEFER verdict (gate + plan), the off-peak hold ───────────────────────────────
const P_CLEAR_BOTH: &str = r#"{"rate_limits":{"five_hour":{"used_percentage":42,"resets_at":1750000000},"seven_day":{"used_percentage":10,"resets_at":1760000000}}}"#;

#[test]
fn gate_defer_when_not_offpeak() {
    let d = tmp("gatedefer");
    let empty = d.join("empty");
    let env = [("I2P_COST_STATE_DIR", empty.to_str().unwrap())];
    // Peak (09:00 local) + require-offpeak → DEFER (exit 4).
    let (got, rc) = run(
        &[
            "gate",
            "--require-offpeak",
            "--now",
            "1700038800",
            "--tz-offset-min",
            "0",
        ],
        P_CLEAR_BOTH,
        &env,
    );
    assert_eq!(rc, 4, "got={got}");
    assert_eq!(pjson(&got)["verdict"], "DEFER");
    // Off-peak (22:13 local) → CONTINUE (exit 0).
    let (got, rc) = run(
        &[
            "gate",
            "--require-offpeak",
            "--now",
            "1700000000",
            "--tz-offset-min",
            "0",
        ],
        P_CLEAR_BOTH,
        &env,
    );
    assert_eq!(rc, 0);
    assert_eq!(pjson(&got)["verdict"], "CONTINUE");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn plan_defer_large_in_peak() {
    // est_total for class large = 250000 ≥ 150000 threshold; peak → DEFER (exit 4).
    let d = tmp("plandefer");
    let calf = d.join("cal.json");
    let env = [("I2P_CALIBRATION_FILE", calf.to_str().unwrap())];
    let (got, rc) = run(
        &[
            "plan",
            "--class",
            "large",
            "--now",
            "1700038800",
            "--tz-offset-min",
            "0",
        ],
        "",
        &env,
    );
    assert_eq!(rc, 4, "got={got}");
    let last = got.lines().last().unwrap();
    assert_eq!(pjson(last)["decision"], "DEFER");
    // Off-peak → RUN NOW (exit 0).
    let (got, rc) = run(
        &[
            "plan",
            "--class",
            "large",
            "--now",
            "1700000000",
            "--tz-offset-min",
            "0",
        ],
        "",
        &env,
    );
    assert_eq!(rc, 0);
    assert_eq!(pjson(got.lines().last().unwrap())["decision"], "RUN NOW");
    let _ = std::fs::remove_dir_all(&d);
}

// ── Track 1: gate snapshot-freshness fail-closed (the live→disk bridge) ───────────────────
#[test]
fn gate_snapshot_freshness_fails_closed() {
    let cap: i64 = 1700000000;
    // both windows present + clear, so a FRESH snapshot yields CLEAR→CONTINUE under default --window both.
    let snap = format!(
        r#"{{"captured_at":{cap},"rate_limits":{{"five_hour":{{"used_percentage":42,"resets_at":1750000000}},"seven_day":{{"used_percentage":10,"resets_at":1760000000}}}}}}"#
    );
    // (age, expected verdict, exit)
    let cases = [
        (0i64, "CONTINUE", 0),
        (900, "CONTINUE", 0),
        (901, "ASK", 20),
        (-10, "ASK", 20),
    ];
    for (age, want, code) in cases {
        let d = tmp(&format!("fresh{}", age.unsigned_abs()));
        std::fs::write(d.join("ratelimit-snapshot.json"), &snap).unwrap();
        let clock = (cap + age).to_string();
        let env = [("I2P_COST_STATE_DIR", d.to_str().unwrap())];
        let (got, rc) = run(&["gate", "--clock", &clock], r#"{"none":1}"#, &env);
        assert_eq!(pjson(&got)["verdict"], want, "age={age} got={got}");
        assert_eq!(rc, code, "age={age}");
        let _ = std::fs::remove_dir_all(&d);
    }
}

// ── Track 1: L4 cheap resume (pause → checkpoint → set-pointer → resume → remaining) ──────
#[test]
fn ledger_cheap_resume() {
    let d = tmp("resume");
    let ds = d.to_str().unwrap();
    run(
        &[
            "ledger", "init", ds, "j", "reviewer", "a,b,c,d", "500000", "15",
        ],
        "",
        &[],
    );
    run(&["ledger", "mark-done", ds, "j", "a"], "", &[]);
    // pause records a checkpoint carrying the LIVE ceiling snapshot that triggered it.
    run(
        &[
            "ledger",
            "pause",
            ds,
            "j",
            "ceiling",
            "85",
            "1700003600",
            "99000",
            "1700000000",
        ],
        "",
        &[],
    );
    let (st, _) = run(&["ledger", "status", ds, "j"], "", &[]);
    let v = pjson(&st);
    assert_eq!(v["state"], "paused");
    let ck = &v["checkpoints"][0];
    assert_eq!(ck["reason"], "ceiling");
    assert_eq!(ck["five_hour_pct"], 85);
    assert_eq!(ck["resets_at"], 1700003600i64);
    assert_eq!(ck["spent_tokens"], 99000);
    assert_eq!(ck["units_done"], 1);
    assert_eq!(ck["units_remaining"], 3);
    // a context pointer so resume reuses derived context instead of re-deriving it.
    run(
        &[
            "ledger",
            "set-pointer",
            ds,
            "j",
            "cached_reviews_dir",
            "doc/cached-reviews",
        ],
        "",
        &[],
    );
    run(&["ledger", "resume", ds, "j"], "", &[]);
    let (st, _) = run(&["ledger", "status", ds, "j"], "", &[]);
    let v = pjson(&st);
    assert_eq!(v["state"], "running"); // resumed
    assert_eq!(v["checkpoints"].as_array().unwrap().len(), 1); // history preserved
    assert_eq!(
        v["context_pointers"]["cached_reviews_dir"],
        "doc/cached-reviews"
    ); // pointer survived
       // remaining = only what's LEFT (a is done) — never re-run done units.
    let (rem, _) = run(&["ledger", "remaining", ds, "j"], "", &[]);
    assert_eq!(rem, "b\nc\nd\n");
    let _ = std::fs::remove_dir_all(&d);
}

// ── Track 1: PROBE → measure → CONTINUE arc ──────────────────────────────────────────────
#[test]
fn probe_then_measured_continue() {
    let env = [("I2P_CALIBRATION_FILE", "/tmp/tf-none-probe.json")];
    // class-only estimate → LOW confidence → PROBE (exit 3).
    let (got, rc) = run(&["preflight", "--class", "large"], "", &env);
    assert_eq!(rc, 3);
    assert_eq!(pjson(&got)["verdict"], "PROBE");
    // after measuring one unit, re-estimate with --measured-unit-tokens → HIGH → CONTINUE (exit 0).
    let (got, rc) = run(
        &[
            "preflight",
            "--name",
            "reviewer-fanout",
            "--width",
            "26",
            "--measured-unit-tokens",
            "18000",
        ],
        "",
        &env,
    );
    assert_eq!(rc, 0);
    assert_eq!(pjson(&got)["verdict"], "CONTINUE");
}

// ── Track 1: registry ephemeral (session) vs durable (oscron) arming ─────────────────────
#[test]
fn registry_session_arming_cleared_on_reset() {
    let d = tmp("sessionarm");
    let ds = d.to_str().unwrap();
    let env = [(
        "I2P_MACHINE_REGISTRY",
        d.join("m.json").to_str().unwrap().to_string(),
    )];
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    run(
        &[
            "registry",
            "register",
            ds,
            "j1",
            "17 22 * * *",
            "0",
            "./l.json",
            "./p.txt",
            "",
        ],
        "",
        &env,
    );
    // session arming is EPHEMERAL — reset-armed (a fresh session) clears it.
    run(&["registry", "arm", ds, "j1", "session"], "", &env);
    run(&["registry", "reset-armed", ds], "", &env);
    let (got, _) = run(&["registry", "get", ds, "j1"], "", &env);
    assert_eq!(pjson(&got)["armed"], false);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn oscron_install_idempotent_via_fake_crontab() {
    let d = tmp("oscron");
    // a fake crontab honouring -l / - (buffer-then-replace, like the real one)
    let fake = d.join("fakecron");
    std::fs::write(&fake, "#!/usr/bin/env bash\nS=\"${FAKECRON_STORE:?}\"\ncase \"$1\" in (-l) [ -f \"$S\" ] && cat \"$S\" || exit 1 ;; (-) t=\"$(mktemp)\"; cat > \"$t\"; mv \"$t\" \"$S\" ;; esac\n").unwrap();
    let mut perm = std::fs::metadata(&fake).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perm.set_mode(0o755);
    std::fs::set_permissions(&fake, perm).unwrap();
    // a wrapper file that must exist for the readability check
    let wrapper = d.join("run-offpeak.sh");
    std::fs::write(&wrapper, "#!/usr/bin/env bash\n").unwrap();
    let store = d.join("store.cron");
    let repo = d.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let env = [
        ("I2P_CRONTAB", fake.to_str().unwrap()),
        ("FAKECRON_STORE", store.to_str().unwrap()),
        ("I2P_OFFPEAK_WRAPPER", wrapper.to_str().unwrap()),
    ];
    // install twice → idempotent (one line)
    run(
        &["oscron", "install", repo.to_str().unwrap(), "nightly"],
        "",
        &env,
    );
    run(
        &["oscron", "install", repo.to_str().unwrap(), "nightly"],
        "",
        &env,
    );
    let body = std::fs::read_to_string(&store).unwrap();
    assert_eq!(
        body.matches("# i2p-scheduler:nightly").count(),
        1,
        "idempotent"
    );
    assert!(
        body.contains("17 22,23,0-7 * * * bash"),
        "default cron + marker line"
    );
    // uninstall removes it
    run(&["oscron", "uninstall", "nightly"], "", &env);
    let body2 = std::fs::read_to_string(&store).unwrap();
    assert!(!body2.contains("# i2p-scheduler:nightly"));
    let _ = std::fs::remove_dir_all(&d);
}

// ── report: every mode rendered (was only proven by the now-retired bash differential) ──────
#[test]
fn report_all_modes() {
    let d = tmp("report");
    let ds = d.to_str().unwrap();
    let cal = d.join("cal.json");
    let machine = d.join("m.json");
    let sig = d.join("sig.json");
    std::fs::write(
        &sig,
        r#"{"verdict":"hook-signal-available","guard_mode":"live-ceiling","events":{}}"#,
    )
    .unwrap();
    let env = [
        ("I2P_CALIBRATION_FILE", cal.to_str().unwrap()),
        ("I2P_MACHINE_REGISTRY", machine.to_str().unwrap()),
        ("I2P_SIGNAL_FINDINGS", sig.to_str().unwrap()),
    ];
    // seed calibration (flat + hierarchical) and a registered+armed job with a ledger
    for i in 1..=6 {
        let act = (100000 + i * 500).to_string();
        run(
            &["calibrate", "close", "plan:medium", "100000", &act],
            "",
            &env,
        );
    }
    run(
        &[
            "calibrate",
            "close",
            "experiment/code-gen/opus",
            "100000",
            "120000",
        ],
        "",
        &env,
    );
    run(
        &[
            "registry",
            "register",
            ds,
            "nightly",
            "17 22 * * *",
            "1500000",
            "./.i2p/jobs/nightly.json",
            "./p.txt",
            "the big fan-out",
        ],
        "",
        &env,
    );
    run(&["registry", "arm", ds, "nightly", "oscron"], "", &env);
    run(
        &[
            "ledger", "init", ds, "nightly", "reviewer", "a,b,c,d", "1500000", "15",
        ],
        "",
        &env,
    );
    run(&["ledger", "mark-done", ds, "nightly", "a"], "", &env);
    run(
        &[
            "ledger",
            "pause",
            ds,
            "nightly",
            "ceiling",
            "85",
            "1700003600",
            "99000",
            "1700000000",
        ],
        "",
        &env,
    );

    let has = |args: &[&str], needles: &[&str]| {
        let (got, rc) = run(args, "", &env);
        assert_eq!(rc, 0, "args={args:?}");
        for n in needles {
            assert!(got.contains(n), "report {args:?} missing {n:?} in:\n{got}");
        }
    };
    has(
        &["report", ds, "--scheduled"],
        &["📋 Scheduled jobs", "nightly", "OS-cron armed"],
    );
    has(
        &["report", ds, "--estimator"],
        &["📈 Estimator convergence", "plan:medium"],
    );
    has(
        &["report", ds, "--kaizen"],
        &["KAIZEN", "champion", "MAPE", "plan:medium"],
    );
    has(
        &["report", ds, "--taxonomy"],
        &["taxonomy", "opus", "plan:medium"],
    );
    has(&["report", ds, "--brief"], &["🛡️", "nightly"]);
    has(
        &["report", ds],
        &["📋 Scheduled jobs", "📈 Estimator convergence"],
    );
    // empty repo + no calibration → --brief is silent
    let empty = tmp("reportempty");
    let env2 = [(
        "I2P_CALIBRATION_FILE",
        empty.join("none.json").to_str().unwrap().to_string(),
    )];
    let env2: Vec<(&str, &str)> = env2.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let (got, _) = run(&["report", empty.to_str().unwrap(), "--brief"], "", &env2);
    assert_eq!(got, "");
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&empty);
}

// kaizen/taxonomy on an EMPTY calibration render their "no data" branches
#[test]
fn report_kaizen_taxonomy_empty() {
    let d = tmp("reportkz");
    let env = [(
        "I2P_CALIBRATION_FILE",
        d.join("none.json").to_str().unwrap().to_string(),
    )];
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let (k, _) = run(&["report", d.to_str().unwrap(), "--kaizen"], "", &env);
    assert!(k.contains("legacy EWMA-0.4 champion"));
    let (t, _) = run(&["report", d.to_str().unwrap(), "--taxonomy"], "", &env);
    assert!(t.contains("no classes yet"));
    let _ = std::fs::remove_dir_all(&d);
}
