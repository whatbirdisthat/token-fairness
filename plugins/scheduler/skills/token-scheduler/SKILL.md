---
name: token-scheduler
description: >
  Run a large, token-intensive, wide-reaching job SAFELY — without ever exhausting the usage limit or
  causing a paid lockout. Trigger with /concierge:schedule (or "this is a big fan-out", "review every
  reviewer/handler", "run this overnight", "schedule this off-peak", "will this blow my token budget?").
  Before any wide fan-out it estimates cost, REQUIRES an explicit +Xk budget, throttles waves (≤4, not
  130), and gates every wave against the LIVE 5-hour rate-limit window — halting and checkpointing
  before the ceiling so a paused job resumes cheaply from what's LEFT. Can run heavy work off-peak
  (22:00–08:00) while reserving a morning allowance from "what time will you log in tomorrow?".
  Also answers "how's the estimator doing?", "what's scheduled?", "show the convergence report" by
  running `tf report` (scheduled jobs + the estimate:actual convergence per profile/class).
  Durable & session-safe: jobs persist to .i2p/scheduled-jobs.json and a SessionStart report re-arms
  them after a crash/restart. Determinism lives in the tested `tf` binary (cargo frozen vectors +
  a byte-for-byte differential against the original bash); this skill is the discipline that calls it.
metadata:
  type: orchestrator
  output: a guarded job run + a resume ledger at .i2p/jobs/<job-id>.json; no lockout, ever
  model: inherit
---

# Token-aware job scheduler — the discipline

Enacts the operating model in
[`knowledge/token-aware-scheduling.md`](../../knowledge/token-aware-scheduling.md). The arithmetic is
in the tested `tf` binary; your job is to **sequence its verbs and obey the verdicts**. Never compute a
ceiling or estimate yourself — call `tf`. Protect the meter at all costs; sacrifice parallelism first.

`tf` is the scheduler binary shipped in this plugin's `bin/` (resolved per-arch by
`hooks/tf-hook.sh`). All examples below call it as `tf <verb>`; on a machine where `tf` is not on
`PATH`, invoke `bash ${CLAUDE_PLUGIN_ROOT}/hooks/tf-hook.sh <verb> …`.

## When this applies

Any job that fans out across many units (files, agents, routes) or could plausibly cost six figures of
tokens — "review all the reviewers and handlers", a repo-wide audit, a migration sweep. If it's one or
two agents, just do it. If it's wide, it MUST pass through here.

## 1. Pre-flight — estimate before you spend

Pick or write a job profile (`profiles/*.json`; project overrides in `.i2p/job-profiles/`).
A profile declares the unit, model, minimal tools/plugins/context (waste elimination), `max_parallel`,
and a token estimate. Then:

```bash
tf preflight --profile "$S/profiles/reviewer-fanout.json" --width 26
```

- **CONTINUE** (HIGH confidence) → you have a trustworthy estimate.
- **PROBE** (LOW confidence) → **run exactly ONE unit for real**, read its actual token cost, then
  re-estimate with `--measured-unit-tokens <N>`. This is the user's "run one of n, measure, then
  decide" — never fan out 26 agents on a guess.

Show the user the estimate (`est_total`) in plain terms before fanning out.

## 2. Budget gate — consent is mandatory, and it must become a MACHINE cap

A wide fan-out **must** carry an explicit `+Xk` budget — but a number chosen in a dialog is **not a
cap until it is one of these** (Issue #2: a UI-selected "+400k" enforced nothing; the run spent
2.47×). Establish an enforceable cap *before wave 1*:

```bash
tf doctor                                  # readiness: session writer? budget headroom? (exit≠0 ⇒ not armed)
tf budget set --per-fanout-cap <Xk>        # FIRST: raise the per-fan-out ceiling (default 150k) to your +Xk
tf budget arm <Xk>                         # the +Xk consent → preflight-spend DENIES an unarmed Workflow
```

`arm` **refuses an est above `per_fanout_cap`** (default 150k) — so a large fan-out (e.g. +400k) arms
nothing until you raise the cap with `tf budget set --per-fanout-cap` (and `--session-cap` if the
cumulative ceiling is tighter). Check `tf budget status` to see both.

`tf budget arm` makes the signal-INDEPENDENT cap real: the `PreToolUse(Workflow|Agent|Task)` hook
(`preflight-spend`) **denies an unarmed Workflow outright** and refuses an over-cap arm — it does not
need any live `.rate_limits` (there is none inside a Workflow). For an off-peak/scheduled job, the
ledger budget (step 3) + `tf ledger spend` is the equivalent local cap. One unit per wave at most
`profile.fanout.max_parallel` — never the 80–130 that caused the lockout.

## 3. Open the resume ledger

```bash
tf ledger init . "<job-id>" reviewer-fanout "unit1,unit2,…" <budget_total> <headroom>
tf ledger set-pointer . "<job-id>" cached_reviews_dir doc/cached-reviews
```

Point `context_pointers` at anything expensive you derive ONCE (repo map, cached reviews) so a resume
reuses it instead of re-deriving — the difference between a cheap resume and another full-cost re-fan.

## 4. The wave loop — gate every wave against the LIVE ceiling

Before spawning **each** throttled wave, gate it. The harness feeds the live signal to hooks (mirrored
to a fresh snapshot by `tf snapshot` (the hook)), so the gate reads stdin-or-fresh-snapshot:

```bash
tf gate --headroom 15        # add --require-offpeak --now $(date +%s) for overnight
```

Act on the verdict — **do not override it**:

- **CONTINUE** → spawn the next wave (≤ `max_parallel`). After each unit completes, `tf ledger
  mark-done` **and `tf ledger spend . <job-id> <unit_actual>`** (the local cap — it returns HALT once
  cumulative spend reaches `budget_total`, signal-independent); record real cost via `tf calibrate
  close <profile> <est> <actual>` so the next estimate sharpens.
- **HALT** → the live window hit the ceiling (or `ledger spend` crossed the budget). `tf ledger pause
  . <job-id> ceiling <pct> <reset> <spent>` and **STOP**. Do not retry, do not "push through".
- **ASK** → no fresh live signal (fail closed). Surface to the user; never spawn blind. For an
  **unattended** run that must not pause for input, gate with `tf gate --on-no-signal halt` so a blind
  L1 becomes a HALT (the budget arm/ledger remains the real backstop).
- **DEFER** (with `--require-offpeak`) → not in the quiet hours; go to step 5.

A rate-limit error returned by any agent is itself a HALT: stop, checkpoint, do not retry the unit.

## 5. Off-peak — supervised-autonomous, with a morning reserve

For heavy jobs the user wants run while they sleep, ask **once, at inception**:
*"What time will you log in tomorrow? I'll work through the night but leave you a morning allowance."*
Convert their answer to an epoch and compute the per-window plan:

```bash
tf offpeak-budget --now $(date +%s) --login <login-epoch> --reset <5h-reset-epoch> \
     --headroom 15 --morning-reserve 60
```

Use each window's `headroom` as the `--headroom` you pass to `tf gate` for that window:
unattended windows run to 85%; the window the user wakes into is held to 40% so their morning isn't
spent. Raise `--morning-reserve` when login lands in their peak day. Run **in-session** from the job
the user launched with consent; pause at the live ceiling; resume when the window resets. A cron, if
any, only **alerts** from the ledger's `checkpoints[]` — it must **never** resume autonomously.

## 6. Resume — only what's left

```bash
tf ledger remaining . "<job-id>"   # the worklist; everything else is already done
tf ledger resume . "<job-id>"
```

Re-enter the wave loop over `remaining` only, reusing `context_pointers`. Never re-run done units,
never re-derive pointed-to context.

## 7. Close out

When `remaining` is empty: report what was done, the actual vs estimated cost, and any `failed` units.
**Feed convergence the MEASURED actual** — a fan-out's subagent tokens never moved `session.json`, so
the auto-delta is 0. Close with the ground truth from `tf spend` (which reconciles every subagent /
workflow transcript):

```bash
tf plan-close --actual "$(tf spend --session <sid> | jq .total_tokens)"
```

If the job cleared without tripping the meter — that's the green gate. *"Light is green, trap is clean."*

## Durable & session-safe (survive a crash)

Scheduled jobs persist to `.i2p/scheduled-jobs.json` (project) + `~/.claude/state/i2p-cost/
scheduled-jobs.json` (machine index) via `tf registry`; the job's prompt is stored under
`.i2p/scheduled-jobs/<id>.prompt.txt`. Because `CronCreate` is **session-only**, after a crash or
restart the cron is gone but the *definition* survives. The SessionStart hook (`startup-report.sh`)
resets armed-state, reports what's pending, and asks you to **re-arm**: for each registered job, run
`CronList` (avoid dupes), then `CronCreate` with its stored `cron` + `prompt_file`, then
`tf registry arm . <id>`. Nothing is ever silently lost.

## "How's the estimator doing?"  (continuous improvement, made visible)

When the user asks how the estimator is doing / what's scheduled / for the convergence report, run:

```bash
tf report .            # both sections
tf report . --estimator   # just convergence
```

Read it back plainly: per profile/class — samples, learned mean ratio, p95 band, tier
(SEEDING→CALIBRATING→CONVERGING→CONVERGED). The band **tightening over time** is the SOLID
self-improvement covenant in numbers: every job that passes through makes the next estimate sharper.
This is why **every** plan brackets `plan-open`/`plan-close` — no sample, no convergence.

## The non-negotiables

1. **Read the LIVE signal, never a stale proxy.** The guard reads `rate_limits.*`; it never trusts the
   old session.json token-count.
2. **Fail closed.** No signal → ASK/HALT, never CONTINUE.
3. **Throttle, don't flood.** Waves of ≤ `max_parallel`.
4. **Budget is consent, and consent must be a machine cap.** No `+Xk` → no autonomous wide fan-out.
   A UI number enforces nothing — `tf budget arm <Xk>` (or a ledger budget + `tf ledger spend`) is
   what actually denies the spawn. An **unarmed Workflow is auto-denied**; run `tf doctor` first.
5. **A HALT is cheap** because the ledger makes resume cheap. Halting early is always correct.
6. **Feed convergence the measured actual** (`tf plan-close --actual $(tf spend …)`) — subagent
   tokens are invisible to `session.json`, so the auto-delta would silently log nothing.
