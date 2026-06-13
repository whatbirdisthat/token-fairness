# Incident report — Workflow fan-out exceeded its budget cap 2.47×

**Date:** 2026-06-13 · **Severity:** medium (no lockout, no data loss; the meter ran unguarded) ·
**System:** token-fairness `tf` scheduler (cached `0.1.0`) driving a Claude Code **Workflow**
fan-out · **Outcome:** all work completed and passed review; **~987k tokens spent against a +400k
cap**.

Nothing in this report is softened. The guard did not catch this run; here is exactly why.

## 1. What we were doing

Building four new value-handlers for the idea-to-production marketplace. The chosen method was a
multi-tier pipeline — **haiku** research → **sonnet** synthesis → **opus** authoring → **opus**
adversarial review — run as a single per-handler **Workflow** (the `Workflow` tool, which spawns and
orchestrates subagents deterministically in the background).

The TOKEN SAFETY protocol that ships in this environment prescribes, for *every* plan and especially
any fan-out: classify → stamp (`tf plan`) → bracket (`tf plan-open`/`plan-close`) → carry an explicit
`+Xk` budget → **gate every wave** through `tf gate` → halt before the live ceiling. We followed the
stamping and bracketing faithfully. The protective layers are where it broke.

## 2. Timeline (real `tf` output)

```
# Classify + stamp
$ tf plan --class large --now <epoch>
  💰 ~250k tokens · p95 ±60% · SEEDING (0 samples)
  🕒 Schedule: DEFER → off-peak 22:00–08:00 (now is peak; est is large)
  {"name":"plan:large","est_total":250000,"p95_band_pct":60.0,"tier":"SEEDING",
   "samples":0,"decision":"DEFER","in_offpeak":"false"}

# The user, present, overrode DEFER → "run now" under an explicit +400k cap.

# Bracket open (baseline captured from session.json .tokens)
$ tf plan-open large 376000
  {"opened":"plan:large","est":376000,"baseline_tokens":218015986}

$ tf ledger init . "build-4-handlers" handler-authoring-fanout "<4 units>" 400000 15
  job-ledger: initialised ./.i2p/jobs/build-4-handlers.json (4 units)

# Gate the first wave
$ tf gate --headroom 15
  {"verdict":"ASK","reason":"no-live-signal",
   "ceiling":{"verdict":"NO_SIGNAL","window":"seven_day","used_pct":null,
              "ceiling":85,"headroom":15,"resets_at":null}}
  exit=20

# Proceeded under the +400k consent cap (operator present). Fan-out ran.

# Bracket close
$ tf plan-close
  {"class":"plan:large","est":376000,"actual":0,"convergence":null}
```

Two of these lines are the whole story: `tf gate` → **`no-live-signal`**, and `tf plan-close` →
**`actual:0, convergence:null`**.

## 3. The three failures

### P1 — L1 (live ceiling) was blind: `no-live-signal`

**Symptom:** `tf gate --headroom 15` returned `ASK / no-live-signal` (exit 20) on the very first
wave, and on every subsequent check.

**Root cause:** `tf gate` (`crates/tf-core/src/scheduler.rs:272`) checks for a live `.rate_limits`
payload; absent one, it falls back to the disk snapshot only if it is fresh (`--snapshot-max-age`,
default 900s). With no piped payload and no fresh snapshot, `ceiling::check`
(`crates/tf-core/src/ceiling.rs:60`) classifies the window as `NoSignal` (exit 20), which `gate`
surfaces as `ASK`. Per `knowledge/token-aware-scheduling.md` §"The live→disk bridge", the harness
feeds `.rate_limits` **only to hooks** (SessionStart/PostToolUse/Stop) and the status line — *not* to
the ad-hoc Bash an orchestrator runs mid-turn, and (this event) not into the Workflow context either.
So there was nothing to gate against.

**Why it was invisible:** `ASK` is fail-closed *by design* — "never proceed blind." But to a present
operator already holding a `+400k` consent cap, an `ASK` reads as "I can't see; you decide," and the
job proceeded. There is no louder state, and no machine stop behind it, so the soft ASK was walked
past wave after wave.

### P2 — L2 (hard budget cap) was never armed

**Symptom:** ~987k tokens were spent under a stated +400k cap with no automated halt.

**Root cause:** L2 *is* the Workflow `budget` API, and that field is populated only when the user
**types** a `+Xk` directive in their turn. Here the user selected "+400k" via a question UI, so the
Workflow budget came through as **null** — no hard ceiling. Independently, `tf` itself stores
`budget_total` in the ledger (`crates/tf-core/src/ledger.rs`, `init`) but **never enforces it**: no
code path compares cumulative spend to `budget_total`. The PreToolUse(Agent|Task) backstop
`preflight_fanout` (`crates/tf-core/src/scheduler.rs:370`) denies a spawn **only when `tf gate`
returns HALT** (code 10) — never on budget exhaustion. With L1 blind (P1), gate never returned HALT,
so the backstop never fired.

**Why it was invisible:** the protocol treats L2 as "the Workflow tool enforces its own budget" and
does not re-implement it in `tf`. When the budget API is null, there is simply no L2 — and nothing in
`tf` notices its absence.

### P3 — convergence learned nothing (`actual:0`)

**Symptom:** `tf plan-close` recorded `actual:0, convergence:null`; the estimator stayed at SEEDING.

**Root cause:** `plan_open`/`plan_close` (`crates/tf-core/src/scheduler.rs:208`/`:227`) compute the
actual as `session_tokens() − baseline_tokens`, reading `.tokens` from `session.json` — a counter
written **only by the main-session Stop hook**. A Workflow's tokens are spent by **subagents** in
their own sessions and never move the main counter, so `cur − base = 0`. `plan_close` then skips
`calibrate::close` (it requires `actual > 0`) and reports `null`. The one genuinely expensive run we
had taught the estimator nothing — the exact opposite of the convergence the bracketing exists to
feed.

**Why it was invisible:** `plan-close` already warns when `base == cur == 0` (the session writer
isn't installed), but here `base` was a large real number and `cur` was unchanged, so `actual` was a
legitimate-looking `0`. No warning path covers "baseline moved, current didn't, because the spend was
in subagents."

## 4. Metrics (the full numbers)

| Measure | Value |
|---|---|
| Agents spawned | **30** |
| Subagent tokens | **986,731** |
| Stated cap | 400,000 (`+400k`) |
| Overrun | **2.47×** |
| Tool uses | 297 |
| Wall-clock | ~21 min (1,278,514 ms) |
| Handlers produced | 4 |
| Final verdicts | 4 × PASS, 0 open CRITICAL/HIGH |
| Revision rounds | **3 of 4** handlers needed a re-author+re-review (30 agents vs 24 baseline) |

The last row matters and is reported in full honesty *for* the system: the adversarial
DOCUMENT-REVIEWER pass **did its job** — it caught real issues in three of the four handlers and
forced a revision before PASS. The failure here is purely in the *cost guard*, not in the work
quality.

## 5. Five-layer scorecard

Mapped to `token-aware-scheduling.md` §"Five layers":

| Layer | Guarantee | This run |
|---|---|---|
| **L0** Pre-flight estimate | predict before fanning out | ✅ ran (`tf plan` → SEEDING ±60%); estimate was low but present |
| **L1** Live ceiling guard | halt the next wave at the live ceiling | ❌ **blind** — `no-live-signal`, no payload reached `tf` |
| **L2** Hard budget cap | refuse/stop without an explicit `+Xk` | ❌ **never armed** — budget API null; `tf` doesn't enforce `budget_total` |
| **L3** Off-peak scheduler | run in quiet hours, reserve morning | n/a — operator chose run-now |
| **L4** Cheap resume | resume only what's left | ✅ ledger intact (no halt was needed/possible) |
| L0 feedback | actual feeds convergence | ❌ **broken** — `actual:0`, subagent tokens invisible |

Two of the three protective layers that should have bounded this run were inoperative in the Workflow
context, and the feedback loop that would have made the next estimate honest got a null sample.

## 6. The four open questions — answered, with caveats

`token-aware-scheduling.md` §"Needs empirical verification" listed four unknowns. This event resolves
three and points at the fourth:

1. **Does a `PreToolUse(Agent|Task)` hook receive `.rate_limits`?** Not usefully in this run — the
   gate was blind throughout, so the hook never had a ceiling to deny on. (Whether the hook *fires*
   at all in a Workflow is the residual unknown.)
2. **Do subagents receive the live payload?** **No** — corroborated by P3 (their tokens never touched
   the main counter; they are isolated sessions).
3. **Inside a Workflow, is `.rate_limits` readable, or only `budget.*`?** Behaviour is consistent
   with **only `budget.*`** being available in-Workflow — which makes L2, not L1, the in-Workflow
   guard. And L2 was null. (This is the single highest-value thing to confirm next.)
4. **Does `.cost.total_cost_usd` update intra-turn?** **Not probed.**

**Honest caveats.** This is **one** run in **one** environment. This session may not deliver
`.rate_limits` to *any* surface (so P1 may overstate the Workflow-specific gap). We did **not**
directly dump the Workflow payload to confirm whether `budget.*` was present — the clean follow-up is
to run `tf verify-payload` as a temporary hook and inspect what each surface actually receives, per
the doc's own recommendation. The fixes in [`proposed-solutions.md`](./proposed-solutions.md) are
written to hold **regardless** of how those probes land — each fails safe whether or not the live
signal ever appears.
