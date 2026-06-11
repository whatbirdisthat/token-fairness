# Token-Fairness — Remaining Phases: Features & Upgrades Plan
*(adversarially reviewed — destined for `~/Code/token-fairness/doc/plans/features-upgrades-plan.md`)*

## 0. How to read this

This is a **cold-start execution plan** for finishing the port of the token-aware scheduler
from bash (idea-to-production CONCIERGE) to the standalone Rust `tf` binary, then layering the
model-routing upgrade. It assumes no memory of the session that began it. The **arithmetic core
is done and proven**; everything stateful, side-effecting, and packaged is ahead.

**The bash original is the ORACLE** (`~/Code/idea-to-production/plugins/concierge/scheduler/`).
Every remaining module is ported by reproducing the bash's observable contract byte-for-byte.
Pin the oracle to a commit SHA before you start (see §2.6) — it still lives in an evolving repo.

---

## 1. Where we are (the foundation — real, but partial)

Built and committed in `~/Code/token-fairness/` (commit `0782fbc`):

- **Workspace:** `tf-core` (pure lib) + `tf-cli` (the `tf` binary). `serde_json` for *parsing*;
  **all output hand-formatted** in `tf-core/src/fmt.rs` to match bash/jq byte-for-byte.
- **5 arithmetic verbs ported & conformance-proven:** `calibrate {ratio,close,confidence}`,
  `ceiling-check`, `estimate`, `offpeak-window`, `offpeak-budget`.
- **Two test gates, both green:** `tests/conformance.sh` (64/64 differential cases — *same inputs
  through bash and `tf`*, byte-exact stdout + exit codes), and `cargo test` (frozen-vector,
  self-contained, no bash needed → the CI gate).
- **One documented FP caveat:** deep-accumulated EWMA can differ in the final ULP (jq C-arith vs
  Rust); changes **no** observable output; the gate collapses noise beyond 12 sig-figs and flags
  such cases `ulp`.

> **Honest scope (per review W1):** "Phase 1 done" = **arithmetic core done**. The 5 done verbs
> are stateless pure functions. The **9 remaining modules are stateful and side-effecting**
> (~745 LoC of bash) and carry the hard correctness problems — atomic writes, set-merge
> semantics, freshness windows, crontab idempotence, flock, fallbacks. They have **zero**
> conformance coverage today. Do not under-scope the remainder.

---

## 2. Remaining Phase 1 — the stateful + orchestration tier

Port each to a `tf` subcommand preserving **exact** CLI, exit codes, state-file paths/shapes, and
the `tmp.$$ + rename` atomic-write discipline. Honour all `I2P_*` env overrides (tests depend on
them). Reuse the existing `tf-core::state` helpers (path resolution, `read_json`, `write_atomic`).

### 2.1 `tf ledger` — port of `job-ledger.sh`
Verbs: `init <dir> <job-id> <profile> <units_csv> [budget] [headroom]`, `mark-done`, `mark-failed`,
`remaining`, `pause <…> <reason> [used_pct] [resets_at] [spent_tokens] [at]`, `resume`,
`set-offpeak <start> <end> <tz_offset_min>`, `set-pointer <key> <value>`, `status`. Exit `0`/`2`(no ledger).
- **Path:** `<dir>/.i2p/jobs/<safe_jid>.json`, where `safe_jid = tr -c 'A-Za-z0-9._-' '_'`.
- **Schema:** `{schema_version,"1.0", job_id, profile, budget_total, headroom_pct, offpeak_window(null|{start,end,tz_offset_min}), state("running"|"paused"), units:{total,done[],remaining[],failed[]}, context_pointers:{}, checkpoints:[{at,reason,five_hour_pct,resets_at,spent_tokens,units_done,units_remaining}]}`.
- **Set semantics:** `mark-done` → `remaining-=[u] | failed-=[u] | done+=[u] | unique`; `mark-failed` → `remaining-=[u] | failed+=[u] | unique`. **`unique` sorts** — reproduce jq's sort order exactly.
- `remaining` emits **newline-separated** units (`.units.remaining[]?`). `pause` appends a checkpoint; `resume` only flips state (history preserved).

### 2.2 `tf registry` — port of `jobs-registry.sh`
Verbs: `register <dir> <id> <cron> <budget> <ledger-rel> <prompt-rel> <note>`, `list`, `get`, `arm <id> [method]`, `reset-armed <dir>`, `remove`.
- **Dual registries**, kept in sync: project `<dir>/.i2p/scheduled-jobs.json` (full object incl. `armed`, `armed_via`) and machine `$I2P_MACHINE_REGISTRY | ~/.claude/state/i2p-cost/scheduled-jobs.json` (keyed by **repo+id**: `{repo,id,cron,budget_total,note}`).
- `register` upserts both. `remove` deletes from both (machine match = repo AND id).
- **`reset-armed` (SessionStart semantics):** `map(if (.armed_via // "session")=="oscron" then . else (.armed=false) end)` — session arming is ephemeral, oscron arming is durable.

### 2.3 `tf snapshot` — port of `ratelimit-snapshot.sh`
Reads stdin payload. **Only acts when a rate_limits signal is present.** Writes
`ratelimit-snapshot.json` `{captured_at, rate_limits, cost}` (atomic) AND upserts
`signal-findings.json` (`verdict:"hook-signal-available", guard_mode:"live-ceiling", concluded_at,
events[evt].present=true`). State dir honours `I2P_COST_STATE_DIR`.

### 2.4 `tf signal {conclude,verdict,report}` + `tf verify-payload`
`verify-payload` appends one JSONL line to `payload-probe.jsonl` (`{at,top_level_keys,has_rate_limits,
five_hour_pct,has_cost,cost_usd,has_transcript,tool,hook_event}`). `signal conclude` groups the log
by `hook_event`, tallies fires/with_rate_limits, writes `signal-findings.json` (verdict
`hook-signal-available` iff any capture had rate_limits, else `no-hook-signal`/`budget-cap`).

### 2.5 `tf report [dir] [--scheduled|--estimator|--brief]` — port of `report.sh`
Composes `registry list` + per-job `ledger status` + `calibrate confidence` + `signal-findings.json`.
Two sections; `--brief` is silent when nothing to report. Reproduce the exact human-format strings.

### 2.6 `tf plan|gate|plan-open|plan-close|preflight` — port of `scheduler.sh` dispatch
- **`plan-open <class> <est>`:** read baseline from `$I2P_SESSION_FILE | …/session.json` `.tokens`; write `$I2P_PLANOPEN_FILE | …/plan-open.json` `{class,est,baseline_tokens}`.
- **`plan-close`:** read plan-open.json; `cur` from session.json `.tokens`; `actual = max(cur-base, 0)`; feed `calibrate close plan:<class> <est> <actual>` **only when `est>0 AND actual>0`**; echo convergence.
- **`gate [--headroom][--window][--require-offpeak --now --start --end --tz-offset-min][--snapshot-max-age 900][--clock]`:** payload from stdin; if no `.rate_limits`, fall back to snapshot **iff** `captured_at>0 AND 0≤(clock-captured_at)≤max_age`; call `ceiling-check` (rc 10→HALT exit10, 20→ASK exit20); else if `require-offpeak` and not in window → DEFER exit4; else CONTINUE exit0. **Output:** `{"verdict":…,"ceiling":<full ceiling-check JSON>,"offpeak":<ow JSON|null>}` — note `preflight-fanout` reads `.ceiling.used_pct`.
- **`preflight`** = estimate branch on confidence (exit 0/3). **`preflight-fanout`** (the hook) calls `gate`, and on rc 10 emits the PreToolUse deny JSON.

### 2.7 `tf oscron {install,uninstall}` + `tf run-offpeak` — port of `install-oscron.sh` / `run-offpeak-job.sh`
- Crontab line **byte-identical incl. marker**: `<cron> bash <wrapper> <repo> <job> >> <log> 2>&1  # i2p-scheduler:<job>`; default cron `17 22,23,0-7 * * *`; idempotent replace by marker; `$I2P_CRONTAB` override; exit 2 if no crontab.
- `run-offpeak`: `flock` FD-9 single-instance (degrade to bare `exec 9>lock` if absent); off-peak guard via `offpeak-window`; headless `claude -p "$(cat <prompt>)" --permission-mode acceptEdits --allowedTools Read Edit Glob Grep Bash`; prompt at `<repo>/.i2p/scheduled-jobs/<job>.prompt.txt`; log `…/offpeak-job.log`. **Cron has a minimal env — use absolute paths to `claude`, `tf`, `flock`** (review S3).

### 2.8 Conformance for the stateful tier (review W2/W3/S4 — REQUIRED)
The pure-stdout differential model is insufficient for side-effecting verbs. For each:
1. Run bash and `tf` in **two fresh `mktemp` HOME/dirs**, with a **pinned `--clock`/`--now`**.
2. Diff **both** stdout+rc **and a canonicalized dump of every written state file** (key order, jq
   `unique` sort, trailing newlines, `.tokens` precision).
3. **Pin the non-deterministic fields** explicitly: `captured_at` (inject clock), `tr`-sanitized
   jids, crontab → redirect via `$I2P_CRONTAB` to a temp file (**never** the real crontab).
4. Add a **full-loop integration test** (review C3): `plan-open → simulate token delta in
   session.json → plan-close → assert the EWMA actually advanced` — not just per-verb I/O.
5. Boundary cases for gate freshness (review W4): age = 0, 900, 901, negative skew.
6. **Pin the oracle** to a commit SHA in `conformance.sh` (vendor a snapshot or record the SHA).
The bash `scheduler/test/*.test.sh` are the **oracle source, not proof of `tf`**.

---

## 3. Packaging & cutover — the dangerous part (sequenced for safety)

### 3.1 The new plugin `plugins/scheduler/`
- `plugin.json` (auto-discovers `skills/`, `commands/`, `agents/`, `hooks/hooks.json`).
- Move from concierge: `skills/token-scheduler/SKILL.md`, `commands/schedule.md`,
  `knowledge/token-aware-scheduling.md`, `profiles/*` — repoint every `${CLAUDE_PLUGIN_ROOT}/scheduler/X.sh`
  reference at the `tf` CLI.
- `knowledge/cognition-routing.md` (new, Phase 2).

### 3.2 Hooks must call a **bash shim**, not the binary (review C2)
`verify-prereqs` Check L runs `bash -n` + smoke-exec on every hooks.json command. A compiled binary
fails `bash -n`. So `hooks.json` invokes `bash ${CLAUDE_PLUGIN_ROOT}/hooks/tf-hook.sh <verb>`, and
`tf-hook.sh` resolves the correct per-arch binary and `exec`s it. Hook events to register (mirror
concierge): PreToolUse(Agent|Task)→preflight; SessionStart/PostToolUse/Stop→snapshot; **plus the
Stop session-token writer (see C3)**.

### 3.3 Multi-arch binary distribution (review C2)
A single committed `bin/tf` runs on one OS/arch only. Choose a real distribution (see Open
Decision D2): ship per-target binaries (`bin/tf-x86_64-linux`, `bin/tf-aarch64-darwin`, …) selected
by `tf-hook.sh` via `uname -ms`, **or** a release-download/`cargo install` install step. CI
(`.github/workflows/verify.yml`) cross-compiles the targets (musl for linux static).

### 3.4 Move the `session.json` writer WITH the scheduler (review C3 — CRITICAL)
`session.json .tokens` (the ACTUAL-spend signal that makes convergence work) is written by
concierge's `statusline/capture-cost.sh` at the Stop hook — **not** in the move list. If it stays in
concierge and concierge is later removed/decoupled, `plan-close` silently sees `cur==base==0`,
`actual==0`, and `calibrate close` **never fires** — the estimator never converges, silently. Port a
`tf` Stop-hook writer for `.tokens` into the scheduler plugin, and **emit a visible warning when
`plan-close` sees `base==cur==0`**.

### 3.5 State-file coexistence during half-migration (review C4)
Both plugins may be installed at once (the realistic state, given manual cross-install). They write
the same `~/.claude/state/i2p-cost/{ratelimit-snapshot,signal-findings}.json`. Decide the contract
(Open Decision D3): **(a)** namespace token-fairness state under `~/.claude/state/token-fairness/`
with a one-time migration, or **(b)** keep the path and have installing the scheduler plugin disable
concierge's snapshot hooks. Add a conformance case with **both writers active**.

### 3.6 `verify-prereqs.sh` for a single-plugin marketplace (review W6)
The i2p script asserts byte-parity of `check.sh`/`SOUL.md`/`inject-soul.sh` **across ≥2 plugins** —
which fails by construction with one plugin. Adapt explicitly: drop/relax the multi-copy parity
checks; replace with a **`bin/tf` ↔ build-target parity / version-pin** invariant; document the
Check L carve-out for binary-backed hooks; confirm MCP Checks (C/K) pass with **no `.mcp.json`**
(the scheduler ships none — review W5; state this explicitly rather than leaving it silent).

### 3.7 Cutover sequencing — do NOT delete bash in the same release (review C1 — CRITICAL)
1. **Release 1:** ship token-fairness (binary + plugin + marketplace). i2p **keeps** concierge's
   bash. Make `CLAUDE.md §TOKEN SAFETY` call a **resolver**: `command -v tf >/dev/null && tf … ||
   bash plugins/concierge/scheduler/scheduler.sh …`. Both work; `tf` preferred when present.
2. **Release 2+:** once adoption is real, retire the bash. Until then it is the graceful-degradation
   fallback (Claude Code has **no cross-marketplace dependency resolution** — token-fairness is a
   manual, optional `/plugin marketplace add` + `/plugin install`).
3. Update the foundry `knowledge/orchestration/tier-assignment.md` reference and the glossary.

---

## 4. Phase 2 — cognition routing (spec made executable; review S1)

Make the estimator predict **best-fit model** + **$-cost**, not just tokens. Extend foundry's
`policy/model-selection.md` tier table — do not duplicate it.

**Class → tier mapping (the source of truth this plan adds):**

| cognition_class | executes on | rationale |
|---|---|---|
| `determinative` | **no model** — a `determinative_handler` (a `tf` subcommand or client script), **0 tokens** | one correct output; transfer to the client, pay once |
| `mechanical` | **haiku** | high-volume, low-judgement |
| `discernment` | **sonnet** (→ **opus** when a false PASS propagates: gates, security) | recoverable judgement |
| `thought-intensive` | **opus** | one error cascades |

- **Routing rule:** `best_fit = cheapest tier whose ceiling ≥ the unit's cognition floor`; never
  downgrade below the floor to save tokens (foundry rule). Determinative units leave the token
  economy entirely.
- **Cost model:** `cost($) = Σ_units price(tier).in·in_tok + price(tier).out·out_tok, scaled by
  ratio_ewma(profile)`. Pricing canon = `model-prices.tsv` (move from concierge `statusline/`;
  format: `prefix<TAB>in<TAB>out<TAB>cache_write<TAB>cache_read` per 1M tokens). Estimator reports a
  token band (today) **and** a $-band per candidate tier.
- **Profile schema gains:** `cognition_class` (per unit/stage) and optional `determinative_handler`
  (path the router substitutes for the model). Router fills `model` instead of it being hardcoded.
- **`determinative_handler` semantics (the "0 tokens" mechanism):** the handler is an executable the
  orchestrator runs **instead of** spawning an agent; its stdout IS the unit's output. It MUST carry
  a differential test (same proof discipline as Phase 1) — "0 tokens" must not buy wrong answers.
- **Worked example:** a 26-unit reviewer fan-out where 8 units are `determinative` (lint/format
  checks) → those run as `tf` handlers (0 tok); 18 are `discernment` → sonnet; banner shows
  "≈ $X sonnet vs $Y haiku; 8/26 determinative → free."
- New `knowledge/cognition-routing.md`; lift the 4 classes + scheduler verdicts into the glossary.

---

## 5. Phase 3 — determinism transfer (minimum spec; review S2)

The standing capability behind determinative routing. Keep it small and concrete or mark explicitly
as future direction — do not ship the hand-wavy version.

- **Clamp registry** (`clamped-processes.json`): each entry `{id, input_contract, handler_path,
  test_path, promoted_at, evidence}` — a determinative operation once done by a model, now a tested
  `tf` subcommand / script.
- **Promotion gate:** a process is promoted only with **evidence** — a differential test proving the
  handler reproduces the model's output on a representative corpus. No test → no promotion.
- **Rollback:** a clamped process that regresses is demoted (handler removed; work returns to a
  model). The registry records both transitions.
- **Metric honesty:** "per-unit spend → 0" is real only because work moved to verified code; the
  registry's correctness oracle (the test) is what makes the 0 trustworthy. Show spend-drop in the
  convergence ledger **alongside** the passing oracle, never alone.

---

## 6. Adversarial review — findings & resolutions

Independent reviewer verdict: **NOT_READY** (for the *cutover*, not the arithmetic core). Every
finding is resolved above; map:

| # | Severity | Finding | Resolution in this plan |
|---|---|---|---|
| C1 | CRITICAL | Deleting bash while `tf` is a manual/optional install breaks the ALWAYS-ON guard | §3.7 resolver fallback; never delete in the introducing release |
| C2 | CRITICAL | Single binary is OS/arch-specific; `bash -n` fails on an ELF hook | §3.2 bash shim + §3.3 multi-arch distribution |
| C3 | CRITICAL | `session.json` writer stays in concierge → convergence silently dies | §3.4 move the writer; warn on `base==cur==0`; §2.8(4) full-loop test |
| C4 | CRITICAL | Shared state files; two writers race during migration | §3.5 namespace or disable concierge hooks; both-writers conformance case |
| W1 | WARNING | "Phase 1 done" overstates — stateful half unwritten | §0/§1 honest re-scope |
| W2 | WARNING | Differential harness can't cover side effects | §2.8 diff state files, isolated HOME, pinned clock |
| W3 | WARNING | bash tests measure bash, not `tf` | §2.8 tf-side gate; bash = oracle source |
| W4 | WARNING | gate clock/freshness ambiguity | §2.8(5) boundary cases; single authoritative clock |
| W5 | WARNING | MCP surface unstated | §3.6 assert no `.mcp.json`; confirm Checks C/K pass |
| W6 | WARNING | `verify-prereqs` assumes ≥2 plugins | §3.6 list the dropped/added checks |
| S1 | SUGGESTION | Phase 2 under-specified | §4 concrete table, cost model, handler semantics, worked example |
| S2 | SUGGESTION | Phase 3 hand-wavy | §5 registry schema, promotion gate, correctness oracle |
| S3 | SUGGESTION | cron permission blast-radius / minimal env | §2.7 absolute paths; document posture |
| S4 | SUGGESTION | oracle drifts as bash evolves | §2.8(6) pin oracle SHA |

---

## 7. Open decisions for whoever executes (with recommendations)

- **D1 — Cutover aggressiveness:** *Recommended:* keep concierge bash as a resolver fallback for ≥1
  full release (§3.7). Alternative: hard-cut (only if i2p will hard-require token-fairness).
- **D2 — Binary distribution:** *Recommended:* per-arch binaries committed under `bin/` + `uname`
  selection in `tf-hook.sh` (works offline, no toolchain on user machines). Alternatives:
  release-download, or `cargo install` (requires Rust on the user's box).
- **D3 — State namespacing:** *Recommended:* own dir `~/.claude/state/token-fairness/` with a
  one-time import of existing `calibration.json` (clean coexistence). Alternative: shared path +
  scripted disable of concierge snapshot hooks.
- **D4 — Phase 2/3 scope now vs later:** *Recommended:* finish + cut over Phase 1 first; land Phase 2
  as its own release; treat Phase 3 as future direction until a real clamp candidate appears.
- **D5 — Naming:** `token-fairness` / `tf` are working names. Run `/ideator:name` before publishing.

---

## 8. Verification (end-to-end)

1. **`cargo test`** green — frozen-vector conformance for every new verb (self-contained CI gate).
2. **`tests/conformance.sh`** green — differential vs the **pinned** bash oracle, incl. state-file
   diffs, isolated HOME, pinned clock, both-writers coexistence case.
3. **Convergence loop** (C3): scripted `plan-open → session.json delta → plan-close` advances the
   EWMA; `plan-close` warns on `base==cur==0`.
4. **Hot-path hook smoke:** `tf-hook.sh preflight` denies at a simulated ≥85% ceiling, CONTINUE on a
   clean payload; cold start < 5 ms; `bash -n tf-hook.sh` passes (Check L).
5. **Multi-arch:** the selected binary execs on linux-x64 and mac-arm64 (or the chosen install path
   works on both).
6. **Marketplace:** `scripts/verify-prereqs.sh` green (single-plugin variant); `/plugin marketplace
   add` + `/plugin install scheduler@token-fairness`; `/i2p-check` sees it; resolver fallback works
   with the plugin ABSENT.
7. **Routing (Phase 2):** unit tests for class→tier + the determinative escape; a profile shows
   token+$ bands per tier; a `determinative_handler` passes its differential test.

## 9. Suggested execution order

`ledger → registry → snapshot → signal/verify-payload → report → plan/gate/plan-open/plan-close →
oscron/run-offpeak` (each with §2.8 conformance) **→** packaging (§3.1–3.6) **→** cutover Release 1
(§3.7, bash fallback) **→** Phase 2 **→** (Phase 3 when a candidate appears).
