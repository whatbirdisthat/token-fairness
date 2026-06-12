# Experiment — cross-model seeding of the estimator (Phase 6)

**Run:** 2026-06-12 · branch `feat/ensemble-estimator-kaizen` · budget directive **+400k** (consent).
**Status:** complete. This is the experiment the user specified when commissioning the self-improving
estimator (commit `c975dd5`). The estimator *machine* shipped that night; this run finally fed it
**real cross-model token-spend data** instead of the synthetic `grind` samples used to prove the wiring.

## Why
The original ask: *"run the prompts against all model options to gain an initial sample to use for the
estimator… categorise jobs in an increasing-fidelity graph… run multiple algorithms concurrently,
tracking accuracy over time."* The build landed but Phase 6 (the actual cross-model run) was gated
pending an explicit `+Xk` budget per the TOKEN SAFETY protocol, and the late-night session ended at the
gate. This run executes it.

## Method
- **3 areas of interest × 4 models = 12 runs**, one shared closed-form prompt per area (no tool use, so
  spend is bounded and comparable):
  - `experiment/code-gen/<model>` — write `longest_common_prefix(strs: &[&str]) -> String` in Rust + an exhaustive test module.
  - `experiment/refactor/<model>` — preserve-behavior refactor of a gnarly Python filter function.
  - `experiment/reasoning/<model>` — classify 8 support tickets into {bug, feature-request, question, billing}.
- Models: **opus** (`claude-opus-4-8`), **sonnet** (`claude-sonnet-4-6`), **haiku** (`claude-haiku-4-5`), **fable** (`claude-fable-5`).
- Each run was a subagent with a model override; **actual tokens** taken from the harness's reported
  `subagent_tokens`; each fed to the ensemble via `tf calibrate close experiment/<area>/<model> 20000 <actual>`.
- Seed estimate was **20,000 tokens/cell** (the estimator's cold-start `per_unit`, SEEDING tier).
- Fitness judged by **3 fresh-context adversarial reviewers** (one per area), with the 4 outputs
  **anonymized C1–C4** to remove model-name bias, then de-anonymized here.

## Token-spend results (actual vs 20k estimate → ratio)

| Area | opus | sonnet | haiku | fable |
|---|---|---|---|---|
| code-gen | 20,057 (1.00) | 12,877 (0.64) | 12,765 (0.64) | 19,227 (0.96) |
| refactor | 17,980 (0.90) | 11,782 (0.59) | 11,921 (0.60) | 18,441 (0.92) |
| reasoning | 18,140 (0.91) | 11,870 (0.59) | 12,016 (0.60) | 18,139 (0.91) |
| **mean actual** | **18,726** | **12,176** | **12,234** | **18,602** |
| **mean ratio** | **0.94** | **0.61** | **0.61** | **0.93** |

**Headline:** for identical closed-generation work, **opus and fable spend ~1.5× the tokens of sonnet
and haiku** (~18.7k vs ~12.2k). The 20k cold-start seed *overestimated* sonnet/haiku by ~40% and was
about right for opus/fable — exactly the per-class correction the estimator now carries.

## Fitness results (adversarial review)

| Area | opus | sonnet | haiku | fable | Winner |
|---|---|---|---|---|---|
| code-gen | 2/10 | 2/10 | 6/10 | **9/10** | **fable** |
| refactor | 9/10 | 8/10 | 8/10 | **9/10** | **fable / opus (tie)** |
| reasoning | 10/10 | 10/10 | 10/10 | 10/10 | four-way tie (non-discriminating) |

- **code-gen** was the discriminating task. Both byte-comparing implementations — **sonnet** and **opus**
  — share a **latent UTF-8 panic**: they slice at the byte divergence index without a char-boundary
  guard, so an input like `["é","ê"]` (shared leading byte `0xC3`, differing continuation byte) slices
  mid-codepoint and panics. **opus was scored *worst*** because it paired the bug with a confidently
  **false** code comment ("identical leading bytes imply identical leading code points") that would
  survive review and propagate the defect. **fable** won outright: fast byte scan **plus** an explicit
  `is_char_boundary` repair, and its test suite included the exact `["é","è"]`/`["aé","aè"]` cases that
  *prove* the safety property. **haiku** was correct-but-quadratic (`chars().nth()` in a loop), 6/10.
- **refactor**: all four were behavior-preserving; they separated on style only (opus & fable used the
  tighter `matches or None`; sonnet kept the cryptic `r`). A near-tie.
- **reasoning**: degenerate — all four returned identical, fully-correct JSON. The reviewer's verdict:
  the task is too easy/short to discriminate models; the two genuinely arguable tickets (#4, #8) were
  resolved identically by all, collapsing the signal. **Replace with adversarially-ambiguous items next time.**

**Cross-cutting finding:** highest token spend ≠ highest fitness. **opus** cost the most yet produced
the worst code-gen artifact (buggy + falsely justified); **fable** matched opus's cost but topped fitness
on both discriminating tasks. **sonnet/haiku** were cheapest; haiku traded safety-by-construction for a
performance footgun, sonnet shipped the silent panic.

## Estimator state after the run
- All **12 classes** moved **SEEDING → CALIBRATING ↑** (`tf report --estimator`).
- 1 sample/class, so all 7 algorithms tie and the default champion `ewma@0.4` stands (≥3 samples needed
  to discriminate `linreg`/`sma`/`median`/…). The taxonomy now holds the real per-model ratios, so a new
  sibling class (e.g. a 5th model) inherits a sane prior via shrinkage backoff instead of the flat 1.0.
- Durable state (already HOME-rooted, **not** `/tmp`): `~/.claude/state/i2p-cost/calibration.json` and
  `…/estimator-accuracy.jsonl` (12 real entries). The stale `/tmp/estimator-accuracy.jsonl` is last
  night's synthetic `grind` log and can be deleted.

## Budget accounting
- Measured subagent spend: code-gen 64,926 · refactor 60,124 · reasoning 60,165 · reviews 61,042 =
  **~246k** of the **400k** authorized envelope. Central estimate was ~360k; the run came in **under**
  because the closed (no-tool) prompts were cheaper than a tool-using job.
- Live rate-limit gate returned `no-live-signal` in this environment, so the **+400k consent budget was
  the operative guard**; spend was tracked per wave against it.

## Limitations / next steps
1. **1 sample/class** can't rank the algorithm field. Re-run each cell ≥3× (varying the prompt per
   index) to let `linreg`/`median`/etc. compete and the champion diverge from the `ewma@0.4` default.
2. **reasoning task is non-discriminating** — swap for adversarially-ambiguous tickets, and grade a
   secondary axis (calibration/abstention/rationale) so it separates models.
3. **Fitness vs cost is the interesting axis** — a future run should emit a cost-adjusted fitness score
   (fitness per 1k tokens) so routing can prefer fable/haiku where they match opus.
