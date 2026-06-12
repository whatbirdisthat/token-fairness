# Design — router uplift: fit-for-purpose model selection (build spec)

**Status:** design only. This spec is the output of the spike; **no code is built here.** It names the
exact attach points against today's tree so a follow-on plan can execute phase by phase. Prior-art
evidence lives in `doc/research/llm-routing-prior-art.md` (mapping in §6); experiment evidence in
`doc/experiment-discriminating-v2.md`.

## 1. Goal & shape
Today `tf route` maps a *given* `cognition_class` → tier/model/cost (`routing.rs:90-104`, `:201`). The
class is hardcoded in a profile or passed via `--cognition` (`routing.rs:157-162`). **Nothing classifies
a raw prompt.** The uplift adds a front-end and a strategy layer:

```
user prompt ─▶ [classify] ─▶ cognition_class + confidence + rule_source
                                     │
                                     ▼
                        [route] ─▶ tier/model/$band      (exists: routing.rs)
                                     │
                                     ▼
                  [cascade] cheap-first ─▶ adversarial judge ─▶ escalate?   (new strategy loop)
                                     │
                            outcome folded back ─▶ converging ruleset ─▶ clamp when proven
```

Four cognition classes are the stable taxonomy (`routing.rs:8-13`) — the user's model maps cleanly:
`mechanical→haiku` (retrieval/enumeration), `discernment→sonnet` (synthesis, escalate→opus),
`thought-intensive→opus` (hard codegen/planning), `determinative→none` (0 tokens). `fable` is the
"big/long/knowledge-heavy" lane — see §7 (it needs a 5th tier, today's table is haiku/sonnet/opus only).

## 2. `tf classify` — the classification pass (new)
**New module** `crates/tf-core/src/classify.rs`; **new dispatch arm** in `main.rs` beside `route`
(`main.rs:161` — add `"classify" => classify::classify(rest, &read_stdin()),`). Lib export in `lib.rs`.

Layered, cheapest-first (decision locked: rules → embeddings → haiku fallback):
1. **Deterministic rules** (0 tokens, <1ms) — keyword/regex/shape heuristics (e.g. fenced code + "fix"/
   "implement" → `thought-intensive`; "list/enumerate/which libraries" → `mechanical`; "design … then
   review" → `thought-intensive`; lint/format/exact-transform → `determinative`). Transparent, frozen-vector testable.
2. **Embeddings + kNN** over cached task archetypes — for the ambiguous middle. Deterministic once vectors
   are cached; needs an embedding source (decide at build: local model vs cached API vectors).
3. **Haiku fallback** — only for genuinely ambiguous prompts. Emits a class + writes a learning sample.

Output (one JSON line, like `route`): `{"cognition_class":"…","confidence":0-1,"rule_source":"rule|knn|haiku"}`.
Confidence tier gates which layer is allowed to answer (mirror `calibrate::confidence` tiers
SEEDING/CALIBRATING/CONVERGED).

## 3. The converging ruleset (reuse existing convergence)
The ruleset hardens from probabilistic→deterministic using the **same plumbing as the estimator**, not new
machinery:
- **State:** new `classify-cache.json` in `state::state_dir()` (same atomic write/read as `calibration.json`).
- **Sample:** each `(prompt-bucket, predicted_class, confirmed_class)` — confirmed by the cascade outcome
  or a human override. The "ratio" analogue is a class-match in {0,1}, folded with the **Welford band +
  EWMA + shrinkage backoff** already in `calibrate.rs:94-107` (`resolve_node`, `BACKOFF_K0=3`): a new
  prompt-bucket shrinks toward its parent bucket's behaviour until it has its own samples — increasing
  fidelity, identical to the taxonomy story.
- **Promotion:** when a bucket's agreement is high and band tight (CONVERGED), the haiku layer is bypassed
  — the deterministic rule answers for 0 tokens. This is "tokens → cpu-cycles" made measurable.

## 4. Cascade / escalation harness (new strategy)
Decision locked: **adversarial judge now, distill to heuristics later.**
- Ladder: `haiku → sonnet → opus` (→ `fable` only for the big/knowledge-heavy lane, §7). Start at the
  classified floor, not always haiku — never downgrade below the cognition floor (`routing.rs:4-6` rule).
- Loop: run cheapest fit model → **anonymized adversarial judge agent** scores fitness against the ask's
  acceptance criteria → if it passes, keep; if it fails, escalate one tier and repeat. (This is the v1/v2
  experiment's own judge pattern, promoted to a routing primitive.)
- **Acceptance contract** (define precisely at build): per-class criteria + a pass threshold; record
  `(start_tier, escalations, final_tier, judge_verdicts, tokens_each)` so savings are measurable and the
  recurring failure shapes can later be distilled into cheap deterministic acceptance checks.
- **Where it lives:** orchestrator discipline (a skill) initially; a `tf cascade` planning helper can
  compute the ladder + budget but the model calls stay in the agent harness. Decide at build; lean skill-first.

## 5. Enforcement — `UserPromptSubmit` hook + agent backstop
Decision locked. Claude Code **does** support `UserPromptSubmit` (the scheduler plugin's `hooks.json` only
wires SessionStart/PreToolUse/PostToolUse/Stop today — add the new event).
- **Hook:** `plugins/scheduler/hooks/classify-prompt.sh`, wired under `UserPromptSubmit` in `hooks.json`.
  It calls the **deterministic cheap path only** (`tf classify`, rules/knn layers) so a fired rule costs 0
  tokens and negligible latency; it surfaces the class as `additionalContext` to steer the turn. It must
  NOT block on a haiku call inline — if no rule/knn fires, it defers (let the agent decide), keeping every
  prompt fast.
- **Backstop:** extend `inject-token-safety.sh` (SessionStart) with a short routing protocol so the agent
  applies fit-for-purpose selection even when a rule doesn't fire.
- **Latency budget:** the hook's deterministic path must stay within a few ms; haiku-fallback is async/opt-in,
  never on the prompt's critical path.
- **Worked example (caught in-flight):** the `deep-research` workflow fanned out ~116 agents that silently
  inherited the session model (opus) for web read/summarize/refute — the exact waste this plugin targets.
  Fix = route each stage by cognition class (search/fetch→haiku, verify/synthesis→sonnet), ~47% blended
  cut (verify stays at its `discernment` floor — never lower). **Lesson for the build:** fan-out harnesses
  (Workflow, multi-Agent profiles) must route per-stage BY DEFAULT, not inherit the session model. See
  memory `fanout-model-routing-discipline`.

## 6. Prior-art mapping  *(fill from `doc/research/llm-routing-prior-art.md` when research lands)*
- Cascade + verify → **FrugalGPT** lineage (cheap-first, escalate on a verifier). Borrow: the verifier-gated ladder.
- Learned router vs our classes → **RouteLLM**/**Hybrid-LLM**. Borrow: confidence-gated fallthrough thresholds.
- Cheap classification → tiny-classifier vs embeddings+kNN vs heuristics. Borrow: the layered cheapest-first order.
- Router distillation → learned router → decision tree. Borrow: distill haiku decisions into our deterministic rules.
- Semantic caching (**GPTCache**) → a 0-token answer for near-duplicate asks. Borrow: as a pre-classify cache layer.
- *(This section is a stub pending the cited research synthesis; numbers/URLs added there.)*

## 7. Open build decisions (surface before coding)
1. **`fable` tier:** `route_class`/`tier_prefix`/`default_prices` (`routing.rs:65-104`) know only haiku/sonnet/opus.
   Adding fable as the "big/long/knowledge-heavy" lane needs a 5th class+tier+price row, OR keep fable
   out of the auto-router and select it only by explicit strategy. **Recommend:** explicit-only first.
2. **Embedding source** for the kNN layer (local vs cached API). Affects determinism + deps.
3. **Cascade home:** skill vs `tf cascade` helper (lean skill-first; `tf` computes the ladder/budget only).
4. **Determinism proof:** the clamp registry (`determinism-transfer.md`) requires a differential test
   (handler == model on a corpus, 100% match) before any classification rule is trusted as deterministic —
   honour that gate; show spend-drop next to the passing oracle, never alone.

## 8. Test/CI plan
- Frozen-vector cases in `crates/tf-cli/tests/cli.rs` for `tf classify` (each layer; byte-exact JSON + exit code).
- Stateful tests in `tests/stateful.rs` for ruleset convergence SEEDING→CALIBRATING→CONVERGED (seed
  `classify-cache.json` via env-pointed path, assert the layer that answers).
- Honour the llvm-cov gate (`.github/workflows/verify.yml`, `--fail-under-lines 83`, ratcheting): new
  modules must clear it or be path-excluded pending injection (the `offpeak_run.rs` precedent).

## 9. Phased build sequence (for the follow-on plan)
1. `tf classify` rules layer + frozen vectors (no model calls) — pure cpu, immediate 0-token wins.
2. `classify-cache.json` convergence (reuse `resolve_node`) + `tf report` surfacing.
3. `UserPromptSubmit` hook (deterministic path) + SessionStart backstop.
4. Cascade skill (haiku→judge→escalate) with the acceptance contract + savings ledger.
5. Embeddings+kNN middle layer; then haiku fallback + learning loop.
6. Clamp the first proven rule via `clamped-processes.json` (differential test) — the audited tokens→cpu transfer.
