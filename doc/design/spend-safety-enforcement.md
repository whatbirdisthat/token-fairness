# Spend-safety enforcement — the prevention spec (HARDENED, adversarially reviewed)

> The machinery that makes [`STUPID_LESSONS_PAY_STUPID_PRICES.md`](../STUPID_LESSONS_PAY_STUPID_PRICES.md)
> un-repeatable. Adversarially reviewed against one question: *can an ungoverned, expensive, or hidden
> fan-out still happen?* The review found four holes in the first draft; this version closes them.
> **Nothing here is built yet** — each piece returns for its own approval. The CORE three are non-negotiable
> prerequisites; until they exist and pass the red-team, a **HARD FAN-OUT FREEZE** is in force.

**The principle:** advisory safety is theatre. Spend must be **enforced** (the system refuses to overspend)
and **auditable** (every model call and its cost is visible). The user must answer *"how do I KNOW it can't
happen again?"* mechanically, not on trust. **"Certain" = we tried to break each guarantee and could not.**

---

## What the adversarial review found (grounded in the code)
1. **The gate has no live signal, and may never.** `tf gate` → `no-live-signal`; no `ratelimit-snapshot.json`.
   And `session.json` is written **only by a Stop hook** (`session-tokens.sh:1-2`) — stale mid-turn. There is
   **no live in-flight spend signal**. → The primary guard must be a **self-tracked budget** enforced through
   the Workflow engine's own `budget.total` (the one live, enforceable lever) — never a harness signal.
2. **The hooks physically cannot block.** `tf-hook.sh:22-25` — every hook fails soft (`|| true`), "never
   blocks the session." The `PreToolUse(Agent|Task)` backstop **cannot deny**, and doesn't match `Workflow`.
   → Need a deliberately **blocking** `PreToolUse(Workflow|Agent|Task)` hook (no `|| true`).
3. **The first-draft ledger had an interception gap** (assumed `tf` is told of every spawn — it isn't). →
   **Audit reads ground truth: transcripts.** `session-tokens.sh:47-58` already prices spend per-model from
   the transcript, and every agent (incl. subagents) has one. `tf spend` derives complete per-model/per-job
   spend and **flags any gap vs the authoritative total** — nowhere to hide.
4. **Phased build = windows of false safety.** → an explicit FAN-OUT FREEZE + a precise definition of what lifts it.

---

## The guarantees (invariants · mechanism · test · residual risk)

| # | Guarantee | Mechanism | Test | Residual risk |
|---|---|---|---|---|
| **INV-1** | No ungoverned fan-out | blocking `PreToolUse(Workflow\|Agent\|Task)` denies without budget evidence; every launch sets `budget.total` | launch w/o budget → **denied** | if harness lacks a `Workflow` matcher → lean on mandatory `budget.total` + reconcile; escalate upstream |
| **INV-2** | Hard spend ceiling | self-tracked cap (live: `budget.total`; post-hoc: transcript/`session.json`); hook refuses past cap | seed over-cap → next fan-out denied | approximate vs provider meter; monthly USD cap not machine-readable → consent + conservative caps |
| **INV-3** | Full auditability (no secret spend) | `tf spend` parses transcripts → per-model/per-job; flags ledger-vs-transcript gaps | known run reports exactly; injected untracked call → **flagged** | none structural (transcripts are ground truth) |
| **INV-4** | Fable never runs | `route` + blocking spawn guard refuse `fable`/`claude-fable-*`; one constant re-enables | fable request → **denied**, logged | none |
| **INV-5** | Cheap-by-default routing | templates default gather→haiku, synth/verify→sonnet, final→opus (one call, distilled input); audited | deep-research stage→model map asserted | tier override to opus is **visible, not prohibited** (opus sometimes needed) |

**Prohibition vs visibility (be precise):** over-budget and fable are **physically refused**. Model *tier*
is **defaulted-cheap and audited**, not prohibited — because opus is legitimately needed sometimes, and the
honest guarantee is that every such choice is *visible* in `tf spend`, never hidden.

---

## The minimal safe CORE (prerequisite to lifting the FAN-OUT FREEZE)
Until all three land and pass the red-team: **no `Workflow`, no multi-Agent fan-out** — single-threaded local
work only. The freeze becomes self-enforcing the moment CORE-B exists.

- **CORE-A — Self-tracked budget + `budget.total` discipline.** A user-set session/job token cap; every
  sanctioned fan-out launches *through a wrapper* that always sets `budget.total` from `tf ledger remaining`.
- **CORE-B — Blocking `PreToolUse(Workflow|Agent|Task)` hook.** Deliberately NOT fail-soft: denies fan-outs
  with no budget evidence or past the cap. **Verify the harness supports the `Workflow` matcher FIRST**; if
  not, the INV-1 residual path applies and we file the gap upstream.
- **CORE-C — Transcript-based `tf spend` audit + reconciliation.** Per-model/per-job report + discrepancy flag.
  Reuses the transcript-pricing logic already in `session-tokens.sh` and the cost model (`routing.rs:106-117`).

## Remaining pieces (after the CORE lifts the freeze)
- **P-D — Fable hard-ban** (INV-4) — cheap immediate win.
- **P-E — Rewrite `deep-research` to the gated 3-tier pattern** (INV-5): haiku gather · sonnet verify+synth ·
  **opus final-compile (one call, on distilled claims)**; carries `+Xk`; bounds the verify fan-out by
  `budget.remaining()`; `log`s dropped work; emits the spend banner. Per-stage routing baked in as the
  DEFAULT — kills the run-1 root cause (agents inheriting the session model).
- **P-F — Make the gate "see" when it can** (optional): wire the rate-limit snapshot IF the harness exposes
  the payload; else say so loudly. The CORE does not depend on this.
- **P-G — Model-Routing Doctrine doc** (`doc/policy/model-routing-doctrine.md`) + reflect in
  `knowledge/cognition-routing.md`.
- **P-H — Ethos** (`doc/ETHOS.md` + SessionStart line + SOUL): the *why* — protect the solo operator's limits
  so they keep building and growing toward capable, enterprise-grade use. The reason no future reviewer
  relaxes a guard without cause.
- **P-I — The Honesty Observatory** (longitudinal efficacy analytics — build immediately after the CORE).
  `tf spend` (CORE-C) is per-session; the Observatory is the **cross-time** record that proves the system
  works (or admits it doesn't). An append-only event log (`<state>/honesty-events.jsonl`) captures, per
  event: timestamp, session, **type of work** (cognition class / task), **model**, tokens (in/out/cache),
  cost USD, fan-out width, and the *outcome of every guard decision* — **SAVE** (a fan-out the gate DENIED
  before it overspent — "the system actually worked"), **BLOWN** (a limit/lockout we hit anyway), OK, and
  deny-reason. A generator (`tf observe` / `tf report --honesty`) rolls these up **per session · day · week
  · month** into `doc/honesty/`:
  - Rich tabular markdown: spend by model × period; #SAVES vs #BLOWN (the headline efficacy ratio); denies
    by reason; estimate-vs-actual accuracy (MAPE) over time; fan-out count & avg width; cost trend.
  - Charts/graphs: Mermaid `xychart-beta` (tokens/cost over time), `pie` (model mix, work-type mix), and a
    SAVES-vs-BLOWN trend — regenerated each run so the folder is always current.
  - The honesty rule applies here above all: **report the BLOWN count as prominently as the SAVES** — a tool
    that hides its own failures is the thing we are replacing. Data sources: the CORE-C spend ledger, every
    `tf budget check` DENY, every `tf ledger pause`, and `session.json` over time.
  - *Wiring:* a Stop/PostToolUse hook appends the period's events; the report is idempotently regenerated.

  **Status (renderer increment, 2026-06-12).** The capture foundation (commit `9879515`) plus the
  **efficacy rollup renderer** have landed in `crates/tf-core/src/observe.rs`:
  - `tf observe [--period day|week|month] [--write <dir>]` and `tf report --honesty` fold the event
    log into a longitudinal markdown report — the **#SAVES vs #BLOWN headline (equal prominence,
    HON-1)**, per-period table (saves/blown/procedural/allows/est-guarded), denies-by-reason
    histogram, and Mermaid (`pie` decision-mix + `xychart-beta` SAVES-vs-BLOWN trend). `--write`
    regenerates `<dir>/honesty.md` idempotently. No-arg `tf observe` keeps its JSON tally (back-compat).
  - **BLOWN wired:** `observe::log_blown(reason)` now fires from `snapshot::dispatch` when a live
    rate-limit window reads ≥100% (a genuine lockout the guard failed to prevent), deduped by the
    window's `resets_at` so one episode logs exactly one BLOWN.
  - **Spend-by-model × period + MAPE now land** (capture extension shipped):
    - `tf spend --capture` appends a `kind:"spend"` event (per-model tokens+cost) to the honesty
      log; wired into the **Stop hook**. Because Stop re-emits *cumulative* spend every turn, the
      renderer dedups to the **latest reading per session** before bucketing — never double-counts.
    - MAPE-over-time is folded from the existing **estimator-accuracy ledger** (`{at,est,actual}`):
      per-period mean APE = `mean(|est−actual|/actual)`. New pure `fold_accuracy`.
    - The report renders a spend-by-model × period table, a MAPE-over-time table, and a model-cost
      `pie` — each showing an honest "no data yet" line when empty.
  - Pure fold/render functions (`fold_events`, `fold_accuracy`, `render_markdown`) are frozen-vector
    unit-tested; dispatch/capture IO follows the project convention (covered by the e2e bash gates,
    like `spend.rs`).

## Red-team acceptance — how we PROVE it
`tests/red-team-spend.sh` actively tries to violate each invariant and MUST be blocked/flagged:
1. Fan-out with no budget → **denied** (INV-1).
2. Spend seeded over cap → fan-out **denied** (INV-2).
3. Known haiku+sonnet+opus run → `tf spend` totals match; injected unlogged opus call → **flagged** (INV-3).
4. `fable` requested (route + spawn) → **refused** (INV-4).
5. deep-research script → stage→model map is haiku/sonnet/opus, never fable; refuses w/o budget (INV-5).
Plus frozen-vector unit tests per piece, honouring the llvm-cov gate (`.github/workflows/verify.yml`).

## Build order
**CORE-A → CORE-B → CORE-C** (lifts freeze) → **P-D → P-E → P-F → P-G → P-H** — red-team run as the gate
before the freeze lifts. Each piece independently approvable; the CORE three are mandatory prerequisites.

## Freeze-lift checklist (status)
The FAN-OUT FREEZE lifts only when all are ✓:
- [x] CORE-A `tf budget` (caps + arm/check) — built, on master.
- [x] CORE-B `tf preflight-spend` + `PreToolUse(Workflow|Agent|Task)` hook — logic built & wired, on master.
- [x] CORE-C `tf spend` transcript audit + reconciliation — built, on master.
- [x] `tests/red-team-spend.sh` — INV-1/2/3 pass (7/7); INV-4/5 tracked as pending P-D/P-E.
- [ ] **Live matcher probe (NEXT SESSION, the one unproven link).** The new hook loads only at
      SessionStart, so it cannot be exercised in the session that wrote it. First action of a fresh session:
      with caps set and **nothing armed**, launch a **no-op workflow** (a script with ZERO `agent()` calls —
      safe even if the gate misses: nothing can fan out). If the launch is **denied** → `PreToolUse` fires
      for `Workflow`, INV-1 is hook-enforced, **lift the freeze**. If it **runs** → the matcher does not
      cover `Workflow`; keep the freeze and fall back to mandatory `budget.total` + the `tf spend` audit,
      and raise the gap upstream.


1. **Harness `Workflow` matcher** — unverified; checked FIRST in CORE-B. If absent, INV-1 leans on mandatory
   `budget.total` + reconciliation, gap filed upstream.
2. **Model tier is audited, not prohibited** (except fable) — opus-on-cheap-work is visible, not impossible.
3. **No machine-readable monthly USD cap** — guarded by consent + conservative caps, never false sensing.
