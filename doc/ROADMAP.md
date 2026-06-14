# Project Roadmap

> Last updated: 2026-06-14
> Maintained by: Claude Code + FOUNDRY agents

This document is the authoritative list of planned features for this project.
Each entry is self-contained and can be acted upon by an AI agent or developer without additional context. Features are implemented using THE DEVELOPMENT SYSTEM defined in the ROADMAPPER skill.

---

## Status Legend
- **PENDING** — not yet started
- **IN PROGRESS** — actively being implemented
- **SUSPENDED** — mid-implementation pause; plan file has resumption instructions
- **AWAITING MERGE** — built and reviewed; PR open awaiting human merge (under pr-approval governance)
- **COMPLETE** — shipped
- **DEFERRED** — postponed, reason noted in entry

---

## [1] MCP Server, Telemetry Pipeline & Dashboard
> STATUS: COMPLETE (reworked from stub facade to real, behaviour-tested; all 10 ACs met incl. live WebSocket — 2026-06-14)
> ADDED: 2026-06-13
> LAST UPDATED: 2026-06-14
> PRIORITY: HIGH

> ⚠️ **HONEST STATUS (corrected 2026-06-13; reworked 2026-06-14).** This item was once
> falsely marked COMPLETE while the Phase B MCP layer was a **stub facade** (every handler
> returned hardcoded values; tests asserted only response shape) and the dashboard rendered a
> blank page. An Opus CORRECTNESS-REVIEWER audit caught it; the defects below have since been
> fixed and verified. The ONE remaining gap to COMPLETE is the live WebSocket stream (AC#6).
>
> **CRITICAL — FIXED 2026-06-14:**
> - ✅ C1 — `tf_gate` now delegates to the real `scheduler::gate()` and maps the verdict via
>   `map_verdict`; produces the same verdict as CLI `tf gate`.
> - ✅ C2 — `tf_spend` now appends a real spend event via `state::append_line(observe::events_path(), …)`.
>
> **HIGH — FIXED 2026-06-14:**
> - ✅ H1 — `tf_budget_read` computes real `current_spend`/`fanout_spend` via `observe::fold_events`.
> - ✅ H2 — `tf_report` computes window bounds per requested period + folds real totals.
> - ✅ H3 — `tf_observe` returns real deduped-per-session spend spans from the ledger.
> - ✅ H4 — `tf_signal` persists to `mcp-signals.json`; `tf_plan_open`/`close` delegate to
>   `scheduler::plan_open`/`plan_close` (plan_id persisted & verified); `tf_schedule_toggle`
>   writes `schedule-enabled.json`.
> - ✅ H5 — Resources read real state: `tf://status` reflects real signals + folded budget,
>   `tf://events` reads the real ledger (last 100), `tf://calibration` reads the windows snapshot.
> - ✅ H6 — Hand-rolled stdio loop now implements the MCP handshake (`initialize`,
>   `tools/list`, `resources/list`, `tools/call`); unused `rmcp` (and its `tokio`) removed
>   from the `mcp` feature, shrinking `Cargo.lock` by 448 lines — binary-size intent honoured.
> - ✅ H7 — Dashboard reads `session_cap_tokens`/`per_fanout_cap_tokens`; shows the real
>   configured cap and a correct ceiling % (verified live: 14.4% at 71.9M/500M).
> - ✅ DASH-FRONTEND — `assets/dashboard.html` now renders four real Chart.js views (budget
>   gauge, spend-by-model doughnut, guard-efficacy bar, MAPE stat card) with 3s auto-refresh,
>   per-card error states, and empty-state handling. Served as text/html (verified live).
>
> **MEDIUM — FIXED 2026-06-14:**
> - ✅ M1 — MCP tests rewritten to seed real state and assert derived values (192 workspace
>   tests, green 3× consecutive, no flakiness).
>
> **AC#6 — FIXED 2026-06-14:** the `/ws` WebSocket route is wired into the axum router via a
> `tokio::sync::broadcast` channel fed by a 250ms truncation-safe poll of the events journal
> (path from `observe::events_path()`); new JSONL lines reach connected clients well within the
> 1s SLA (verified live: 101 handshake + exact line received). Best-effort per ADR-003 (lagging
> clients dropped, no replay). Frontend connects to `ws(s)://<location.host>/ws`, refreshes on
> push, auto-reconnects, keeps 3s poll as fallback. 4 behaviour tests (SLA, two-client fan-out,
> malformed-line resilience, partial-line guard). All 10 acceptance criteria now genuinely met.

**Brief Description**
Add Model Context Protocol (MCP) server surface to the `tf` binary so Claude Code agents can invoke token-scheduler operations as MCP tools (not just CLI). Expose real-time telemetry via WebSocket and dashboard via embedded HTTP server showing live budget gauges, spend by model, guard efficacy (SAVES vs BLOWN), and estimator accuracy. Support optional Prometheus metrics export for Grafana integration.

### User Stories
- AS A Claude Code agent I WANT to call `tf gate` as an MCP tool SO THAT I can check budget headroom before spawning a fan-out
- AS A developer I WANT a live dashboard of token spend and guard behavior SO THAT I can monitor and debug scheduler decisions in real time
- AS A user with existing Grafana I WANT to export Prometheus metrics from `tf` SO THAT I can integrate with my monitoring stack

### EARS Specification

**Ubiquitous requirements:**
- The system SHALL expose token-scheduler operations as MCP tools over stdio transport (zero external dependencies)
- The system SHALL serve a live dashboard on HTTP (default port 8080) with real-time charts of budget state, spend, and guard efficacy
- The system SHALL preserve backward compatibility with the existing CLI (all `tf` subcommands unchanged)
- The system SHALL NOT increase the hook binary size significantly (feature-gate heavy dependencies like axum, tokio)

**Event-driven requirements:**
- WHEN a JSONL telemetry file is appended to, THE SYSTEM SHALL broadcast the new lines to connected WebSocket clients in real time
- WHEN an agent calls the `tf_gate` MCP tool, THE SYSTEM SHALL return a JSON verdict with ceiling state and decision rationale
- WHEN a user navigates to `localhost:8080`, THE SYSTEM SHALL serve an HTML page with embedded Chart.js dashboard (no server-side templating)

**State-driven requirements:**
- WHILE a WebSocket client is connected, THE SYSTEM SHALL stream new telemetry events as they arrive (best-effort, no buffering on disconnect)
- WHILE the `tf dashboard` command is running, THE SYSTEM SHALL watch the honesty-events.jsonl file for appends and forward them to subscribers

**Optional feature requirements:**
- WHERE `--prometheus` flag is set, THE SYSTEM SHALL emit metrics in Prometheus text format at `GET /metrics`
- WHERE a docker-compose.yml is provided, THE SYSTEM SHALL enable integration with a Grafana datasource (scrape Prometheus endpoint)

### Acceptance Criteria
(status as of rework 2026-06-14: ✅ met · ❌ not met)
1. ✅ `tf mcp` starts without error; reads tool invocations from stdin (now with MCP handshake)
2. ✅ `tf_gate` returns a REAL verdict via `scheduler::gate()` + `map_verdict`
3. ✅ `tf_budget_read`/`tf_budget_set` work; state persists; real spend folded in
4. ✅ `tf_report`, `tf_observe`, `tf_spend` return REAL JSON (per-period windows, real ledger)
5. ✅ `tf dashboard` serves HTML with four rendering Chart.js views (verified live)
6. ✅ WebSocket at `ws://…/ws` streams new events within 1s — wired via broadcast channel (verified live)
7. ✅ `GET /metrics` returns valid Prometheus text format (when --prometheus set)
8. ✅ Binary size with no `dashboard` feature ≤105% — `mcp` no longer leaks rmcp/tokio (H6 fixed)
9. ✅ `cargo test --workspace --all-features` passes (192 tests, green 3×); MCP tests now assert behaviour
10. ✅ `tf --help` lists `mcp` and `dashboard` subcommands

### Implementation Notes

**Phase B — MCP Server (STUB FACADE — NOT COMPLETE; rework in progress 2026-06-13)**
- Scaffolded: `tf mcp` subcommand with a hand-rolled stdio loop (NOT rmcp, see H6).
- 10 MCP tool handlers and 3 resources EXIST but are stubs (C1, C2, H1–H6 above): they
  validate input then return hardcoded/zero/empty values without calling the real domain
  modules (`scheduler`, `spend`, `report`, `observe`, `signal`).
- The verdict adapter (`map_verdict`) is the one correct piece: CONTINUE→allow,
  HALT|DEFER|ASK|NO_SIGNAL→deny, reason/ceiling always present. It is wired to nothing.
- AC#2 and AC#4 are NOT satisfied (verdicts/JSON are fabricated). Only AC#1 (process starts),
  AC#8 (gating present, though H6 leaks deps), AC#10 (help lists verbs) hold.
- Rework: replace every stub with a delegate to the existing domain function; rewrite tests
  to seed state and assert derived values (M1).

**Phase C — Telemetry + Dashboard (BACKEND COMPLETE & SERVING; FRONTEND incomplete)**
- `tf dashboard` runs a real axum/tokio HTTP server bound to `0.0.0.0:8080`, serving `/`
  with correct `text/html` MIME and JSON REST endpoints (`/api/session-budget`,
  `/api/spend-by-model`, `/api/guard-efficacy`, `/api/estimator-accuracy`) — verified live.
- Core modules: `telemetry.rs` (file-watcher, fold semantics), `dashboard.rs` (REST endpoint
  projections + Prometheus serializer), `dashboard_run.rs` (CLI dispatch + axum router).
- Fold logic replicates `observe.rs` semantics (dedup spend, bin saves/blown, MAPE).
- KNOWN DEFECTS: H7 (wrong budget keys → always shows default cap) and DASH-FRONTEND (the
  embedded HTML's JS only `console.log`s; no Chart.js charts render — page is blank). Both
  must be fixed for AC#5 ("serves HTML with embedded Chart.js charts") to truly hold.
- Feature-gating: `axum`/`tokio`/`notify` under `[features] dashboard` (hook binary unaffected).
- Tests pass but assert backend shape; the rendered page has no HITL coverage yet.

**Testing & References**
- Full coverage (FOUNDRY mandate: 100%). Conformance against CLI outputs; WebSocket event ordering; Prometheus format validation.
- Artifacts: Four ADR documents (transport choice, dashboard architecture, telemetry pipeline, chart rendering); SPECIFICATION.ears.md; mcp.feature Gherkin scenarios; comprehensive plan file (`doc/[1]_MCP_SERVER_TELEMETRY_DASHBOARD_PLAN.md`).

### Human Interface Test Plan
- [Dashboard home page]: navigate to http://localhost:8080 → verify page loads → verify three Chart.js charts visible (budget gauge, spend by model pie, SAVES vs BLOWN trend) → reload page → verify charts persist
- [WebSocket real-time]: start dashboard → append a line to honesty-events.jsonl → verify chart updates within 1s (no page reload needed)
- [MCP tool invocation from Claude]: MCP client calls `tf_gate` tool → verify JSON response with `verdict` and `ceiling` keys → verify response matches CLI output

### Development Plan Reference
When this feature is selected for implementation, a detailed plan will be written to: `doc/[1]_MCP_SERVER_TELEMETRY_DASHBOARD_PLAN.md`
The plan follows THE DEVELOPMENT SYSTEM.

---

## [2] Dashboard: grounded account-wide windows, real-time, + MCP hit evidence
> STATUS: COMPLETE (PR #12 merged 2026-06-14; issue #11 closed)
> ADDED: 2026-06-14
> LAST UPDATED: 2026-06-14
> PRIORITY: HIGH
> GITHUB_ISSUE: #11 (CLOSED)

**Brief Description**
A multi-agent token blow-out was invisible on the dashboard because it only shows per-session
spend vs a manual cap, never the **account-wide** 5h/weekly provider windows (the real lockout
ceiling shared across all concurrent agents). Ground the dashboard in both signals (clearly
labeled), make it genuinely real-time (live windows + live per-turn spend + live gate/blown feed),
and make MCP invocations observable (audit log + dashboard view) and the `tf mcp` server registered.

### User Stories
- AS A developer running multiple agents I WANT the dashboard to show the account-wide 5h/weekly
  window utilization SO THAT a shared-window blow-out is visible before lockout
- AS A developer I WANT the dashboard to update as I work (windows, spend, gate/blown) SO THAT it
  reflects live state, not end-of-session state
- AS A developer I WANT evidence that the MCP is being hit SO THAT I can confirm the surface works

### Acceptance Criteria
1. Dashboard shows account-wide 5h + weekly `used_percentage` (same source as the statusline,
   `windows::live_windows`), clearly labeled "account-wide / shared across all agents", with a
   visible snapshot age and a BLIND state when the snapshot is stale (> SNAPSHOT_MAX_AGE).
2. The session-cap card is relabeled as a per-session measure (not account-wide).
3. Prometheus exposes real `tf_five_hour_used_percent` / `tf_seven_day_used_percent` (the faked
   `tf_weekly_ceiling_percent` is removed/repointed) + `tf_window_snapshot_age_seconds`.
4. Windows update on the dashboard within ~3s of each PostToolUse (poll), freshness visible.
5. Live per-turn spend: a fail-open, throttled PostToolUse `spend --capture` makes the session
   spend move mid-session (added per-tool latency measured and reported); never vetoes/delays a tool.
6. A Live-Events feed renders gate/blown events as they fire.
7. MCP invocations are logged to `mcp-invocations.jsonl` (top-level param KEYS only, never values)
   at the dispatch choke points; `/api/mcp-invocations` + a dashboard card show a counter + recent hits.
8. `tf mcp` is registered via a shipped `.mcp.json`; `plugin.json` note updated.
9. Dashboard copy states hooks (not MCP) are the current enforcement path (no misleading claim).
10. All new Rust stays feature-gated (`dashboard`/`mcp`); hook binary unaffected; suite green 3×;
    fmt/clippy clean on pinned 1.96.0.

### Implementation Notes
Reuse `budget::win_disp` + `budget::SNAPSHOT_MAX_AGE` (promote to `pub`), `windows::live_windows`,
`windows::{load,spent_in_window,remaining}`, `state::append_line`, `observe::events_path` pattern.
New: `dashboard::endpoint_windows`, `dashboard::endpoint_mcp_invocations`,
`windows::snapshot_captured_at`, `observe::mcp_invocations_path` (ungated), `mcp::log_invocation`.
Frontend: Lockout-Risk card, Live-Events feed, MCP-Invocations card, relabels. Config:
PostToolUse spend capture in `hooks.json`; new `plugins/scheduler/.mcp.json`. Full design in the
approved plan file.

### Development Plan Reference
`/home/user/.claude/plans/i-want-to-add-glistening-valiant.md` (approved 2026-06-14).

---

## [3] Statusline → snapshot bridge (feed the dashboard windows + live risk widget)
> STATUS: COMPLETE (PR #14 merged 2026-06-14; issue #13 closed; CI follow-up 8afae3f)
> ADDED: 2026-06-14
> LAST UPDATED: 2026-06-14
> PRIORITY: HIGH
> GITHUB_ISSUE: #13 (CLOSED)
> DEPENDS ON: [2] (the dashboard windows this feeds; PR #12 — merged)

**Brief Description**
The dashboard's account-wide 5h/weekly gauges (item [2]) sit BLIND because Claude Code delivers the
live `.rate_limits` signal only to the statusline's stdin, not to hook payloads — so the hook-driven
`tf snapshot` no-ops and `ratelimit-snapshot.json` is never written. A shipped statusline widget
pipes that stdin into `tf snapshot` (throttled) on each render, feeding the sink and the real-time
web report, and renders a combined-risk mini-bar + sync age so the bridge is self-evidently alive.

### Acceptance Criteria
1. A statusline widget, installed idempotently on SessionStart into
   `~/.claude/state/statusline-widgets.d/00-tf-ratelimit.sh` (tf-hook.sh path baked at install time).
2. On render with `.rate_limits` present, it writes `ratelimit-snapshot.json` (+ `windows.json`,
   `signal-findings.json`) via `tf snapshot`, throttled (~15s, `I2P_STATUSLINE_SNAPSHOT_THROTTLE_SECONDS`).
3. `tf budget status` and the dashboard `/api/windows` go BLIND→fresh (verified 77%/21%); the
   Lockout-Risk gauges light up within ~1.5s of the first bridged render.
4. Visual: `⬢ tf ▰▰▰▰▱▱ NN% ⇡Ns` — combined-risk (worst of 5h/7d) mini-bar + worst-window %,
   color-coded (green <60 / amber 60-84 / red ≥85) + sync age; `◌BLIND` when no/stale snapshot;
   nothing on no-signal payloads.
5. Fail-open: never exits non-zero, never blocks the statusline; `verify-prereqs` GREEN; `bash -n`
   clean. No Rust change (reuses `tf snapshot`); hook + dashboard binaries unaffected.

### Implementation Notes
Mirrors the concierge statusline install/drift pattern and the throttle-stamp pattern. Widget sorts
first among widgets (`00-` prefix) — leftmost the widget mechanism allows. True line-1 top-left needs
an upstream change to the concierge-owned renderer (optional follow-up). Plan:
`/home/user/.claude/plans/i-want-to-add-glistening-valiant.md`.

---

## [4] Dashboard reachability: configurable port + ship the FULL binary on green build
> STATUS: AWAITING MERGE (built + reviewed; PR open under pr-approval governance)
> ADDED: 2026-06-14
> LAST UPDATED: 2026-06-14
> PRIORITY: HIGH
> DEPENDS ON: [1] (the dashboard this makes reachable)

> ⚠️ **Why this exists.** Two defects made the live dashboard ([1]/[2]/[3]) effectively
> unreachable in practice:
> 1. **Port collision.** `tf dashboard` hard-bound `0.0.0.0:8080` — the same port cadvisor
>    (and countless other tools) default to. On any host already running cadvisor the dashboard
>    couldn't start, and `localhost:8080` showed cadvisor instead. There was no way to change it.
> 2. **The released binary had no dashboard at all.** `dashboard`/`mcp` are feature-gated (axum,
>    tokio — to keep the hot-hook binary lean), but `release.yml` built with a bare
>    `cargo build --release` (no `--features`). Every published GitHub Release asset was a 695K
>    `tf` with **no `dashboard` and no `mcp` command** — a headline COMPLETE feature ([1]) was
>    never actually shipped. CI never caught it because no job exercised the feature-gated code.

**Brief Description**
Make the dashboard reachable end-to-end: a configurable bind port so it can dodge an occupied
8080, and a release/CI pipeline that actually ships — and tests — the full (dashboard + MCP)
binary, auto-cut on a green-build version bump. Plus a repository welcome front door that hands
whoever opens the repo a clickable dashboard URL.

### Acceptance Criteria
1. `tf dashboard --port <N>` and `$TF_DASHBOARD_PORT` override the bind port (spec default stays
   8080 per DASH-001). Invalid/zero/missing port → exit 2 with a clear message (was: silently
   ignored — the `main.rs` dispatch only checked `stdout`, never the error code).
2. The startup banner and `--help` are port-aware and name the cadvisor collision + the `--port`
   escape hatch. The served page is verifiably the tf dashboard ("Token Fairness — Live Dashboard",
   Chart.js, live `/ws`, `/api/windows`), not cadvisor.
3. `release.yml` builds every per-arch asset with `--features mcp,dashboard` (the FULL binary).
   tokio only spins up on `tf dashboard`, so the hot-hook path is unaffected — the cost is binary
   size (695K → ~1.3M), an accepted trade for a shipped dashboard.
4. CI (`verify.yml`) lints and tests the feature-gated code (`clippy`/`test` with
   `--features mcp,dashboard`) and builds the smoke-tested + cross-compiled binaries full-featured,
   so a stub-facade or a missing surface can never ship green again.
5. **Auto-release on green build.** A green `verify` on `master` triggers `release.yml` (via
   `workflow_run`); it cuts a release **iff** the workspace version is new (no existing
   `v<version>`). A version bump is the honest signal that the binary changed — one release per
   bump, never one per commit. Manual `v*` tag pushes still release. (`gh release create` makes the
   tag, so we never depend on a GITHUB_TOKEN-pushed tag re-triggering a workflow — a known Actions
   no-op.) Honours "every version bump needs a matching tag."
6. `.claude/welcome.md` (CONCIERGE format) greets whoever opens the repo with the clickable
   dashboard URL + exact start command and the cadvisor caveat.
7. Workspace version bumped `0.1.1 → 0.1.2` so merging this to green master ships the first FULL
   binary (`v0.1.1` is already released as the lean one).

### Implementation Notes
Code: `crates/tf-cli/src/dashboard_run.rs` (port flag/env + validation, port-aware banner/help),
`crates/tf-cli/src/main.rs` (dispatch honours parse-error exit codes). CI: `.github/workflows/{release,verify}.yml`.
Front door: `.claude/welcome.md`. The local distributed binary
`plugins/scheduler/bin/tf-x86_64-linux` (gitignored) was rebuilt full-featured so it works on this
host before the next release lands. Full EARS/spec docs still say "default port 8080" — accurate, as
`--port` only adds an override.
