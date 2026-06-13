# Proposed solutions — closing the Workflow fan-out cap-bypass

Starter design for the token-fairness maintainers. Three code fixes + one procedure fix, each with:
the root cause it closes, the **exact file/function** it touches, a **new flag/verb interface**, and
**frozen-vector test cases** in the repo's existing `crates/tf-cli/tests/cli.rs` style
(`assert_line(&[args], stdin, &[envs], want, code)`). The Rust below is **illustrative shape**, not a
compiled patch — signatures are real (verified against the tree), bodies are sketches.

> **Why these are safe to add.** They are all **new** verbs/flags or additive fields. The frozen bash
> differential in `tests/conformance.sh` pins the *existing* surface against the bash oracle; new
> Rust-only behaviour doesn't perturb it. Add coverage as new `cli.rs` frozen vectors. Keep every
> existing vector byte-identical. Commit as `feat(scheduler): …`.

Design invariant kept throughout: **every unknown still resolves to HALT or ASK, never silently
CONTINUE.** These fixes make the guard *enforce* and *learn*; they never loosen it.

---

## Start here (priority order)

1. **Fix 3 — `tf plan-close --actual <n>`** — smallest change, restores the convergence loop. ~20 LOC.
2. **Fix 2 — `tf ledger spend` + budget-aware backstop** — the cost-safety fix; a local-accounting cap
   that works with **zero** live signal (the exact condition that bit us).
3. **Fix 1 — `tf doctor` + `tf gate --on-no-signal`** — diagnostics + policy so a blind L1 is loud and
   a headless run can choose to halt rather than ASK.

Each is independently shippable and independently testable.

---

## Fix 3 — let an externally-measured actual feed convergence

**Closes:** P3 (`plan-close` reads only the main-session counter; subagent tokens are invisible →
`actual:0, convergence:null`).

**Touches:** `crates/tf-core/src/scheduler.rs::plan_close` (line 227); reuses
`crates/tf-core/src/calibrate.rs::close` (line 74) unchanged.

**Interface:**

```
tf plan-close [--actual <tokens>]
```

- No flag → today's behaviour exactly (session-delta), byte-for-byte. **No existing vector changes.**
- `--actual <n>` → use `<n>` as the actual, bypassing the `session.json` delta, then call
  `calibrate::close("plan:<class>", est, n)`. This is what an orchestrator passes from the Workflow
  tool's reported `subagent_tokens` (e.g. `986731`).
- Bonus: widen the existing `base==cur==0` warning to also fire when `actual==0 && est>0`, printing
  the remedy: *"pass `--actual <measured>` or run `tf calibrate close plan:<class> <est> <actual>`."*

**Shape:**

```rust
pub fn plan_close(argv: &[String]) -> Out {
    let (flags, _) = parse(argv);                 // same parser gate() uses
    // ... load plan-open file, read est + baseline ...
    let actual = match flags.get("actual") {
        Some(a) => state::digits_or(a, 0),        // explicit measured actual
        None => (session_tokens() - base).max(0), // unchanged legacy path
    };
    let conv = if pest > 0 && actual > 0 {
        let name = format!("plan:{}", pclass);
        let _ = calibrate::close(&name, &pest.to_string(), &actual.to_string());
        calibrate::confidence_string(&name)
    } else {
        if pest > 0 { eprintln!("scheduler: plan-close actual==0 — pass --actual <measured> or run `tf calibrate close plan:{} {} <actual>` to feed convergence.", pclass, pest); }
        "null".to_string()
    };
    // ... emit {class,est,actual,convergence} ...
}
```

**Frozen vectors** (`cli.rs`) — drive against a fixed `I2P_STATE_DIR`/plan-open fixture:

```rust
#[test]
fn plan_close_explicit_actual_feeds_convergence() {
    // Given a plan opened at class=large est=376000 (fixture writes planopen.json)
    assert_line(
        &["plan-close", "--actual", "986731"],
        "", &[("I2P_STATE_DIR", FIX)],
        r#"{"class":"plan:large","est":376000,"actual":986731,"convergence":"1.10x (1 sample)"}"#,
        0,
    );
}

#[test]
fn plan_close_zero_actual_is_backward_compatible() {
    // No --actual, baseline==current → legacy actual:0, convergence:null (UNCHANGED)
    assert_line(
        &["plan-close"],
        "", &[("I2P_STATE_DIR", FIX_NODELTA)],
        r#"{"class":"plan:large","est":376000,"actual":0,"convergence":null}"#,
        0,
    );
}
```

---

## Fix 2 — make L2 a real machine cap (signal-independent)

**Closes:** P2 (budget API null when not typed; `tf` never enforces `budget_total`; backstop denies
only on L1 HALT).

Two complementary mechanisms — a **local-accounting cap** (works with no live signal) and a
**budget-aware backstop** (bites if the Workflow payload exposes `budget.*`).

### 2a. `tf ledger spend` — local accounting against `budget_total`

**Touches:** `crates/tf-core/src/ledger.rs::dispatch` (the `match cmd` arm set; ledger doc already
carries `budget_total`). Add a `spent` accumulator and a `spend` subcommand.

**Interface:**

```
tf ledger spend <dir> <job-id> <tokens>     # add to .units? no — to a new .spent_tokens; returns verdict
tf ledger remaining <dir> <job-id>          # unchanged output, but exits 10 (HALT) once spent >= budget_total
```

- `spend` adds `<tokens>` to a new `spent_tokens` field and returns
  `{"spent":N,"budget_total":B,"verdict":"CONTINUE|HALT"}`; **exit 10 when `spent >= budget_total`**.
- The orchestrator calls `tf ledger spend . <job> <unit_actual>` after each unit (it already calls
  `mark-done`). Once the cap is crossed, the next `spend`/`remaining` returns HALT → the wave loop
  stops and checkpoints. This is **pure local arithmetic** — no `.rate_limits`, no Workflow budget
  API, so it holds in exactly the blind condition that defeated L1/L2 here.

**Shape:**

```rust
"spend" => {
    let add = state::digits_or(arg(3), 0);
    let mut root = match load(&lf) { Ok(v) => v, Err(o) => return o };
    let spent = root.get("spent_tokens").and_then(|x| x.as_i64()).unwrap_or(0) + add;
    root["spent_tokens"] = json!(spent);
    let budget = root.get("budget_total").and_then(|x| x.as_i64()).unwrap_or(0);
    let _ = state::write_json(&lf, &root);
    let halt = budget > 0 && spent >= budget;
    Out::line(
        format!("{{\"spent\":{},\"budget_total\":{},\"verdict\":\"{}\"}}\n",
                spent, budget, if halt {"HALT"} else {"CONTINUE"}),
        if halt {10} else {0},
    )
}
```

**Frozen vectors:**

```rust
#[test]
fn ledger_spend_halts_at_budget() {
    // init with budget_total=400000
    assert_line(&["ledger","spend",DIR,"job","250000"], "", ENVS,
        r#"{"spent":250000,"budget_total":400000,"verdict":"CONTINUE"}"#, 0);
    assert_line(&["ledger","spend",DIR,"job","200000"], "", ENVS,
        r#"{"spent":450000,"budget_total":400000,"verdict":"HALT"}"#, 10);
}
```

### 2b. `tf gate-budget` — budget-aware spawn backstop

**Touches:** `crates/tf-core/src/scheduler.rs::preflight_fanout` (line 370) + a new `gate_budget`.

**Interface:**

```
tf gate-budget [--reserve <tokens>]     # reads budget.* from stdin (Workflow payload)
```

- Reads `budget.remaining` (or `budget.total − budget.spent`) from the stdin payload; returns
  `HALT` (exit 10) when `remaining <= reserve`. If the payload carries no `budget.*`, returns
  `NO_SIGNAL`/ASK (exit 20) — **fail-closed**, identical posture to `ceiling::check`.
- Extend `preflight_fanout` so the PreToolUse(Agent|Task) hook denies the spawn when **either** L1
  gate returns HALT **or** `gate-budget` returns HALT. Today it checks only the former.

**Shape (the added branch in `preflight_fanout`):**

```rust
let g = gate(&[], payload);
let b = gate_budget(&[], payload);          // new
if g.code == 10 || b.code == 10 {
    let reason = if b.code == 10 {
        "Budget cap reached (workflow budget.remaining ≤ reserve). Pause and resume from the ledger.".into()
    } else { /* existing ceiling message */ };
    return deny(reason);
}
```

**Frozen vectors:**

```rust
const B_OK:  &str = r#"{"budget":{"total":600000,"spent":120000,"remaining":480000}}"#;
const B_LOW: &str = r#"{"budget":{"total":600000,"spent":560000,"remaining":40000}}"#;
const B_NONE:&str = r#"{"rate_limits":{}}"#;

#[test]
fn gate_budget_vectors() {
    assert_line(&["gate-budget","--reserve","50000"], B_OK,  &[], r#"{"verdict":"CONTINUE","remaining":480000,"reserve":50000}"#, 0);
    assert_line(&["gate-budget","--reserve","50000"], B_LOW, &[], r#"{"verdict":"HALT","remaining":40000,"reserve":50000}"#, 10);
    assert_line(&["gate-budget","--reserve","50000"], B_NONE,&[], r#"{"verdict":"ASK","reason":"no-budget-signal"}"#, 20);
}
```

---

## Fix 1 — make a blind L1 loud, and let a headless run choose HALT

**Closes:** the *ergonomic* half of P1 — a soft `ASK/no-live-signal` that a present (or headless)
orchestrator walks past, and the absence of any pre-flight readiness check.

### 1a. `tf doctor` — pre-fan-out readiness

**Touches:** new verb in `crates/tf-cli/src/main.rs` dispatch; folds in the existing
`verify-payload` diagnostic.

**Interface:**

```
tf doctor [--snapshot-max-age <s>]
```

Reports a ✓/✗ checklist and exits non-zero if the guard is not armed:

```
$ tf doctor
  ✓ session-tokens writer installed (Stop hook present)
  ✗ ratelimit-snapshot.json STALE (age 5400s > 900s) — L1 will gate blind
  ✓ PreToolUse(Agent|Task) backstop wired
  ✗ no active +Xk budget directive detected
  → before a wide fan-out: refresh the snapshot from the orchestrator turn, and
    declare a ledger budget (tf ledger init … <budget>) so Fix-2 enforces it.
```

This directly answers the doc's §"Needs empirical verification" at runtime instead of by hand.

### 1b. `tf gate --on-no-signal=halt|ask|defer`

**Touches:** `crates/tf-core/src/scheduler.rs::gate` (line 272 — same `flags.get(...)` parser; add one
key). Default `ask` → **today's behaviour unchanged**.

**Interface:**

```
tf gate --headroom 15 --on-no-signal halt     # cron/headless: treat blind as HALT, not ASK
```

- `ask` (default): emit `{"verdict":"ASK","reason":"no-live-signal",…}` exit 20 — unchanged.
- `halt`: emit `{"verdict":"HALT","reason":"no-live-signal",…}` exit 10 — so `preflight_fanout` denies
  the spawn and the wave loop checkpoints. The right default for an *unattended* job that must never
  run blind.
- `defer`: exit with the DEFER code so an off-peak runner holds rather than spending.

**Shape (added to `gate`):**

```rust
let on_no_signal = flags.get("on-no-signal").map(|s| s.as_str()).unwrap_or("ask");
// ... after ceiling::check yields NO_SIGNAL ...
return match on_no_signal {
    "halt"  => Out::line(format!(r#"{{"verdict":"HALT","reason":"no-live-signal","ceiling":{}}}"#, cj), 10),
    "defer" => Out::line(format!(r#"{{"verdict":"DEFER","reason":"no-live-signal","ceiling":{}}}"#, cj), 30),
    _       => Out::line(format!(r#"{{"verdict":"ASK","reason":"no-live-signal","ceiling":{}}}"#, cj), 20),
};
```

**Frozen vectors** (reuse a payload with no `.rate_limits`):

```rust
const P_NONE: &str = r#"{}"#;
#[test]
fn gate_on_no_signal_policy() {
    assert_line(&["gate","--on-no-signal","ask"],  P_NONE, &[], r#"{"verdict":"ASK","reason":"no-live-signal", … }"#, 20);
    assert_line(&["gate","--on-no-signal","halt"], P_NONE, &[], r#"{"verdict":"HALT","reason":"no-live-signal", … }"#, 10);
}
```

---

## Procedure fix — the upstream cause (no code, ships first)

**Closes:** the real root cause — a UI-selected "+400k" never became a machine cap.

**Touches:** `skills/token-scheduler/SKILL.md` (and any orchestration runbook that fans out).

Add a hard rule: **a fan-out under a cap MUST establish an enforceable cap before wave 1**, by one of:

1. **Typed directive** — launch with a literal `+Xk` in the turn so the Workflow `budget` API hard-stops
   (and Fix-2b's backstop has a signal); **or**
2. **Ledger budget** — `tf ledger init … <budget>` and call `tf ledger spend . <job> <unit_actual>`
   after each unit so Fix-2a enforces the cap **with no live signal**.

And: **run `tf doctor` before fanning out**; if it reports L1 blind, set `--on-no-signal halt` (cron)
or keep a human present. A consent number chosen in a dialog is **not** a cap until it is one of the
two forms above.

---

## Test & contribution checklist

- [ ] New behaviour covered by frozen vectors in `crates/tf-cli/tests/cli.rs`; `cargo test` green.
- [ ] No existing vector altered; `bash tests/conformance.sh` differential stays byte-identical to the
      bash oracle (new verbs are Rust-only and outside the frozen surface).
- [ ] `tf plan-close` with no `--actual` is byte-for-byte unchanged (backward-compat vector present).
- [ ] Commits `feat(scheduler): …`; version bump per the repo's convention.
- [ ] `knowledge/token-aware-scheduling.md` §"Needs empirical verification" updated with this event's
      findings, and `skills/token-scheduler/SKILL.md` carries the procedure fix.

---

## Implemented (resolution, 2026-06-13)

Triaged against the **merged CORE-A/B/C** work (which postdates this brief). What shipped on this PR:

- **Fix 3 — DONE.** `tf plan-close --actual <n>`; the documented path feeds it from **`tf spend`**
  (CORE-C, subagent-aware) rather than an orchestrator self-report.
- **Fix 2a — DONE (slim).** `tf ledger spend <dir> <job> <tokens>` → HALT (exit 10) at `budget_total`.
  Scoped to the off-peak/scheduled-job ledger.
- **Fix 2b — DROPPED.** `gate-budget` reading `budget.*` from the Workflow payload is **redundant**
  with CORE-B `budget.rs::preflight_spend` (already wired into `PreToolUse(Workflow|Agent|Task)`,
  signal-independent, denies an unarmed Workflow = INV-1) **and** leans on `budget.*`, which was
  **null** in this very incident. Not implemented.
- **Fix 1a — DONE.** `tf doctor` — readiness checklist redefined around the now-existing
  `tf budget`/arm (session writer? budget headroom? snapshot fresh? armed?), exit≠0 when not ready.
- **Fix 1b — DONE.** `tf gate --on-no-signal=halt|ask|defer` (default `ask`, unchanged; DEFER uses
  the repo's exit-4 convention, not the spec's 30).
- **Procedure — DONE.** `SKILL.md` + `knowledge/token-aware-scheduling.md` updated (the four open
  questions are now answered empirically; the cap must be `tf budget arm`/ledger, not a UI number).

### New finding (not in the original brief)
The session cap counted cumulative `session.json.tokens` **including cache-reads** — a live session
showed **71.6M tokens for $61.81**, tripping the 2M cap so the gate hard-denied *all* fan-out at
trivial real cost. Fixed: `session-tokens.sh` now also writes **`billable_tokens`** (in+out+cache_write,
cache-reads excluded) and `budget.rs` reads it for the cap. `.tokens`/`.usd` stay full for the spend
audit and convergence.
