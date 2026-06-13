//! tf — the token-fairness scheduler binary.
//!
//! One static binary, two surfaces: a CLI (what hooks and the skill call — a drop-in for
//! the old `scheduler.sh …` verbs) and, later, an MCP stdio server. This is the CLI front
//! door; the determinism lives in `tf-core`.

mod offpeak_run;
mod oscron;

use std::collections::HashMap;
use std::io::{IsTerminal, Read};
use std::process::exit;
use tf_core::{
    budget, calibrate, ceiling, estimate, ledger, observe, offpeak, registry, report, routing,
    scheduler, signal, snapshot, spend, Out,
};

/// Lenient flag parser: accepts `--flag value` and `--flag=value`, collects bare
/// positionals in order, and ignores unknown leading dashes (as the bash `*) shift` did).
struct Args {
    flags: HashMap<String, String>,
    positional: Vec<String>,
}

impl Args {
    fn parse(argv: &[String]) -> Args {
        let mut flags = HashMap::new();
        let mut positional = Vec::new();
        let mut i = 0;
        while i < argv.len() {
            let a = &argv[i];
            if let Some(rest) = a.strip_prefix("--") {
                if let Some((k, v)) = rest.split_once('=') {
                    flags.insert(k.to_string(), v.to_string());
                    i += 1;
                } else if i + 1 < argv.len() && !argv[i + 1].starts_with("--") {
                    flags.insert(rest.to_string(), argv[i + 1].clone());
                    i += 2;
                } else {
                    flags.insert(rest.to_string(), String::new());
                    i += 1;
                }
            } else {
                positional.push(a.clone());
                i += 1;
            }
        }
        Args { flags, positional }
    }
    fn flag(&self, k: &str) -> Option<&str> {
        self.flags.get(k).map(|s| s.as_str())
    }
    fn pos(&self, i: usize) -> Option<&str> {
        self.positional.get(i).map(|s| s.as_str())
    }
}

fn read_stdin() -> String {
    if std::io::stdin().is_terminal() {
        return String::new();
    }
    let mut s = String::new();
    let _ = std::io::stdin().read_to_string(&mut s);
    s
}

fn emit(out: Out) -> ! {
    if !out.stdout.is_empty() {
        print!("{}", out.stdout);
    }
    if !out.stderr.is_empty() {
        eprintln!("{}", out.stderr);
    }
    exit(out.code);
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cmd = argv.first().cloned().unwrap_or_default();
    let rest = if argv.is_empty() { &[][..] } else { &argv[1..] };
    let a = Args::parse(rest);

    let out = match cmd.as_str() {
        "calibrate" => {
            let verb = a.pos(0).unwrap_or("ratio");
            let name = a.pos(1).unwrap_or("");
            if name.is_empty() {
                Out::err(
                    "usage: tf calibrate {ratio <name>|close <name> <estimate> <actual>|confidence <name>}",
                    2,
                )
            } else {
                match verb {
                    "ratio" => calibrate::ratio(name),
                    "confidence" => calibrate::confidence(name),
                    "kaizen" => calibrate::kaizen(name),
                    "close" => calibrate::close(name, a.pos(2).unwrap_or(""), a.pos(3).unwrap_or("")),
                    _ => Out::err(
                        "usage: tf calibrate {ratio <name>|close <name> <estimate> <actual>|confidence <name>|kaizen <name>}",
                        2,
                    ),
                }
            }
        }
        // The self-improving estimator surface: `tf estimator <key>` shows the champion + blend +
        // per-algorithm scoreboard; `tf estimator backtest <key>` replays the recorded history to
        // prove which formula would have been most accurate (the bounded "best formula" hunt).
        "estimator" => match a.pos(0).unwrap_or("") {
            "" => Out::err("usage: tf estimator <key> | tf estimator backtest <key>", 2),
            "backtest" => match a.pos(1) {
                Some(key) if !key.is_empty() => calibrate::backtest(key),
                _ => Out::err("usage: tf estimator backtest <key>", 2),
            },
            key => calibrate::kaizen(key),
        },
        "ceiling-check" => {
            let payload = read_stdin();
            ceiling::check(
                a.flag("headroom").unwrap_or("15"),
                a.flag("window").unwrap_or("both"),
                &payload,
            )
        }
        "estimate" => estimate::estimate(estimate::Args {
            profile_path: a.flag("profile"),
            width: a.flag("width"),
            name: a.flag("name"),
            measured: a.flag("measured-unit-tokens"),
            history: a.flag("history-tokens"),
            class: a.flag("class"),
        }),
        "offpeak-window" => offpeak::window(offpeak::WindowArgs {
            now: a.flag("now").unwrap_or(""),
            start: a.flag("start").unwrap_or("22:00"),
            end: a.flag("end").unwrap_or("08:00"),
            reset: a.flag("reset"),
            tz_offset_min: a.flag("tz-offset-min"),
        }),
        "offpeak-budget" => offpeak::budget(offpeak::BudgetArgs {
            now: a.flag("now").unwrap_or(""),
            login: a.flag("login").unwrap_or(""),
            reset: a.flag("reset").unwrap_or(""),
            headroom: a.flag("headroom").unwrap_or("15"),
            reserve: a.flag("morning-reserve").unwrap_or("60"),
            window_hours: a.flag("window-hours").unwrap_or("5"),
        }),
        "ledger" => ledger::dispatch(rest),
        "registry" => registry::dispatch(rest),
        "snapshot" => snapshot::dispatch(&read_stdin()),
        "verify-payload" => signal::verify_payload(rest.first().map(|s| s.as_str()), &read_stdin()),
        "signal" => signal::dispatch(rest),
        "report" => report::dispatch(rest),
        "gate" => scheduler::gate(rest, &read_stdin()),
        "plan" => scheduler::plan(rest),
        "plan-open" => scheduler::plan_open(rest),
        "plan-close" => scheduler::plan_close(rest),
        "preflight" => scheduler::preflight(rest),
        "preflight-fanout" => scheduler::preflight_fanout(&read_stdin()),
        "doctor" => scheduler::doctor(rest),
        "oscron" => oscron::dispatch(rest),
        "run-offpeak" => offpeak_run::run(rest),
        "route" => routing::route(rest),
        "budget" => budget::dispatch(rest),
        "preflight-spend" => budget::preflight_spend(&read_stdin()),
        "session-boundary" => budget::session_boundary(&read_stdin()),
        "spend" => spend::dispatch(rest),
        "observe" => observe::dispatch(rest),
        "" => Out::err("usage: tf <command> [args]", 2),
        "--help" | "-h" | "help" => Out::ok(
            "usage: tf <command> [args]\n\n\
             Core:      budget  gate  plan  plan-open  plan-close  doctor  snapshot  session-boundary\n\
             Reporting: report  spend  signal  verify-payload  observe\n\
             Fanout:    preflight  preflight-spend  preflight-fanout  estimate\n\
             Estimator: calibrate  estimator  route\n\
             Offpeak:   offpeak-window  offpeak-budget  run-offpeak\n\
             Durable:   ledger  registry  oscron\n\n\
             Run `tf <command>` with no args for per-command usage.\n"
                .to_string(),
        ),
        other => Out::err(format!("tf: unknown command '{}'", other), 2),
    };
    emit(out);
}
