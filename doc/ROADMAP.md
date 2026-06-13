# Project Roadmap

> Last updated: 2026-06-13
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
