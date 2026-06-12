//! Shared state-file plumbing: path resolution (honouring the same env overrides as the
//! bash), JSON reads, and atomic write-then-rename (the bash `>$tmp && mv -f` pattern).

use serde_json::Value;
use std::path::Path;

pub fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

/// Wall-clock epoch seconds — the bash `date +%s`. `I2P_CLOCK` overrides it so the
/// snapshot/signal writers can be pinned deterministically in tests (the bash has no such
/// flag; conformance normalises `captured_at`, frozen-vector tests inject `I2P_CLOCK`).
pub fn now_epoch() -> i64 {
    if let Ok(c) = std::env::var("I2P_CLOCK") {
        if let Ok(n) = c.parse() {
            return n;
        }
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Resolve a `--profile` value to a path. An existing file is returned verbatim (so explicit
/// paths stay byte-identical to the bash oracle — conformance unaffected). Otherwise the value
/// is treated as a NAME and resolved, in order: a project override at
/// `.i2p/job-profiles/<name>.json` (cwd-relative, the SKILL's override location), then the
/// shipped `${CLAUDE_PLUGIN_ROOT}/profiles/<name>.json`. If nothing resolves, the value is
/// returned unchanged (a non-existent literal path is still a no-op load, exactly as before).
pub fn resolve_profile(value: &str) -> String {
    if Path::new(value).is_file() {
        return value.to_string();
    }
    let name = value.strip_suffix(".json").unwrap_or(value);
    let mut candidates = vec![format!(".i2p/job-profiles/{}.json", name)];
    if let Ok(root) = std::env::var("CLAUDE_PLUGIN_ROOT") {
        candidates.push(format!(
            "{}/profiles/{}.json",
            root.trim_end_matches('/'),
            name
        ));
    }
    for c in candidates {
        if Path::new(&c).is_file() {
            return c;
        }
    }
    value.to_string()
}

/// `~/.claude/state/i2p-cost` — overridable by `I2P_COST_STATE_DIR` (as in the bash).
pub fn state_dir() -> String {
    if let Ok(d) = std::env::var("I2P_COST_STATE_DIR") {
        return d;
    }
    format!("{}/.claude/state/i2p-cost", home())
}

/// The fixed `~/.claude/state/i2p-cost` dir, **ignoring** `I2P_COST_STATE_DIR`. The bash
/// `signal-probe.sh`/`report.sh` defaults are HOME-rooted and only overridable by their own
/// `I2P_PAYLOAD_PROBE`/`I2P_SIGNAL_FINDINGS`/`I2P_CALIBRATION_FILE` vars — not the cost dir.
pub fn home_cost_dir() -> String {
    format!("{}/.claude/state/i2p-cost", home())
}

/// The calibration ledger path — `I2P_CALIBRATION_FILE` overrides (tests rely on this).
pub fn calibration_file() -> String {
    if let Ok(p) = std::env::var("I2P_CALIBRATION_FILE") {
        return p;
    }
    format!("{}/calibration.json", state_dir())
}

/// The estimator's own accuracy ledger (append-only JSONL) — a sibling of the calibration file,
/// so it honours `I2P_CALIBRATION_FILE`. Backs the KAIZEN accuracy-over-time graph.
pub fn accuracy_ledger() -> String {
    let cal = calibration_file();
    match Path::new(&cal).parent() {
        Some(dir) if !dir.as_os_str().is_empty() => {
            format!("{}/estimator-accuracy.jsonl", dir.to_string_lossy())
        }
        _ => "estimator-accuracy.jsonl".to_string(),
    }
}

pub fn read_json(path: &str) -> Option<Value> {
    let s = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}

/// Atomic write: create parent dir, write a pid-tagged temp, rename over the target.
pub fn write_atomic(path: &str, content: &str) -> std::io::Result<()> {
    if let Some(dir) = Path::new(path).parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let tmp = format!("{}.tmp.{}", path, std::process::id());
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)
}

/// Append one newline-terminated line, creating parents. Best-effort O_APPEND (atomic for small
/// writes on local fs) — the append-only ledger pattern shared by estimator-accuracy.jsonl and
/// the honesty event log. A lost line is acceptable; a corrupt file is not.
pub fn append_line(path: &str, line: &str) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = Path::new(path).parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(line.as_bytes())?;
    if !line.ends_with('\n') {
        f.write_all(b"\n")?;
    }
    Ok(())
}

/// `obj.key` as f64 with a default (mirrors jq `(.key // default)` for numbers).
pub fn num(v: &Value, key: &str, default: f64) -> f64 {
    v.get(key).and_then(|x| x.as_f64()).unwrap_or(default)
}

/// `obj.key` as i64 with a default.
pub fn int(v: &Value, key: &str, default: i64) -> i64 {
    v.get(key).and_then(|x| x.as_i64()).unwrap_or(default)
}

/// Atomically write a JSON value as jq-style pretty (2-space) + trailing newline — the
/// on-disk shape the bash's `jq … > tmp && mv` produces for every state file.
pub fn write_json(path: &str, v: &Value) -> std::io::Result<()> {
    let s = serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".into());
    write_atomic(path, &(s + "\n"))
}

/// `tr -c 'A-Za-z0-9._-' '_'` — replace every char outside the safe set with `_`
/// (per-char, not squeezed). The job-id → filename sanitiser.
pub fn safe_id(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// jq's `(.tokens // 0)`-style numeric read of a string that must be all digits, else default.
pub fn digits_or(s: &str, default: i64) -> i64 {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        s.parse().unwrap_or(default)
    } else {
        default
    }
}

/// The RAW literal token for `"key":` in a flat one-line JSON object — what jq 1.6+ passes
/// through verbatim (number literals are preserved: `1.0500`/`50.0` are NOT normalised).
/// Keys must be unique in the text. Strips quotes for string values; "" if absent. The bare-value
/// branch captures any unquoted JSON literal — numbers AND `true`/`false`/`null` (alphanumerics) —
/// so a boolean like `in_offpeak` reads as `"true"`/`"false"` exactly as the bash `jq -r` did
/// (the numeric-only version returned "" for booleans, wrongly DEFERRING every off-peak gate).
pub fn raw_field(json: &str, key: &str) -> String {
    let needle = format!("\"{}\":", key);
    let Some(p) = json.find(&needle) else {
        return String::new();
    };
    let rest = json[p + needle.len()..].trim_start();
    if let Some(after) = rest.strip_prefix('"') {
        after.split('"').next().unwrap_or("").to_string()
    } else {
        rest.chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
            .collect()
    }
}

/// Parse a string as a JSON number (int or float), else `null` — the bash `numornull`
/// helper used for checkpoint fields (accepts digits and `.`, rejects everything else).
pub fn num_or_null(s: &str) -> Value {
    if s.is_empty() || s == "null" || !s.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return Value::Null;
    }
    serde_json::from_str(s).unwrap_or(Value::Null)
}
