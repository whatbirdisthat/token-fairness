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
> STATUS: IN PROGRESS (Phase A COMPLETE; Phase C dashboard backend COMPLETE & serving; Phase B MCP is a STUB FACADE — see Honest Status; dashboard frontend renders blank)
> ADDED: 2026-06-13
> LAST UPDATED: 2026-06-13
> PRIORITY: HIGH

> ⚠️ **HONEST STATUS (corrected 2026-06-13 after adversarial review).** This item was
> previously marked COMPLETE. That was false. An Opus CORRECTNESS-REVIEWER audit found the
> entire Phase B MCP layer is a **stub facade**: every tool handler validates input then
> returns a hardcoded success/zero/empty value without calling the real domain logic. The
> test suite stayed green only because it asserts response *shape*, never behaviour. Phase C
> (dashboard) backend is genuinely implemented and now serves over HTTP with correct MIME
> types, but the embedded HTML only `console.log`s data — the three `<canvas>` charts never
> render, so the page is effectively blank. Outstanding defects below MUST be fixed before
> this item is COMPLETE.
>
> **CRITICAL (product safety):**
> - C1 — `tf_gate` (mcp.rs) never calls `scheduler::gate()`; it computes a bogus
>   `used_pct + headroom > 100` with inverted logic (more safety margin → deny). The
>   agent-facing spend guard returns a fabricated verdict.
> - C2 — `tf_spend` claims to record a spend event but writes nothing; success is faked, so
>   MCP-recorded spend is silently dropped and the budget under-counts.
>
> **HIGH (incomplete/misleading):**
> - H1 — `tf_budget_read` hardcodes `current_spend`/`fanout_spend` to 0.
> - H2 — `tf_report` returns a fixed 24h window + zero totals regardless of requested period.
> - H3 — `tf_observe` always returns `[]`.
> - H4 — `tf_signal`, `tf_plan_open`, `tf_plan_close`, `tf_schedule_toggle` return success
>   without persisting anything.
> - H5 — Resources `tf://status`, `tf://calibration`, `tf://events` serve hardcoded/empty
>   state (status reports all signals "OK" unconditionally — masks a real ERROR).
> - H6 — Server does not use `rmcp` (hand-rolled stdin loop); no `initialize`/`tools/list`
>   handshake, so a real MCP client likely can't enumerate tools. The unused `rmcp`/`tokio`
>   deps still ship under `--features mcp`, defeating the binary-size intent.
> - H7 — Dashboard reads budget keys `session_cap`/`per_fanout_cap`; on-disk keys are
>   `session_cap_tokens`/`per_fanout_cap_tokens`, so it always shows the default cap and the
>   ceiling % is computed against the wrong denominator. *(Fixed as part of the rework.)*
> - DASH-FRONTEND — `DASHBOARD_HTML` embeds placeholder JS that only logs to console; no
>   Chart.js charts render. The "dashboard" the user asked for does not exist yet.
>
> **MEDIUM:** M1 — MCP tests assert response shape only, not behaviour (the hole that let the
> facade pass the gate). Rework must rewrite them to seed state and assert derived values.

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
(status as of corrected review 2026-06-13: ✅ met · ❌ not met · 🔧 in rework)
1. ✅ `tf mcp` starts without error; reads tool invocations from stdin
2. ❌ `tf_gate` tool returns a REAL verdict `{"verdict":…,"reason":…,"ceiling":{…}}` — currently fabricated (C1) 🔧
3. 🔧 `tf_budget_read`/`tf_budget_set` work; state persists — read hardcodes spend to 0 (H1)
4. ❌ `tf_report`, `tf_observe`, `tf_spend` return REAL JSON — currently zeros/empty/dropped (C2,H2,H3) 🔧
5. 🔧 `tf dashboard` serves HTML with embedded Chart.js charts — serves HTML, but charts don't render (DASH-FRONTEND)
6. ❌ WebSocket at `ws://…/ws` streams new events within 1s — endpoint not wired into the router yet
7. ✅ `GET /metrics` returns valid Prometheus text format (when --prometheus set)
8. 🔧 Binary size with no `dashboard` feature ≤105% — gating present but `mcp` leaks unused rmcp/tokio (H6)
9. 🔧 `cargo test --workspace` passes — but MCP tests assert shape not behaviour (M1); must be rewritten
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
