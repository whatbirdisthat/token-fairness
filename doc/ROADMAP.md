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
> STATUS: IN PROGRESS (Phase B COMPLETE — MCP server shipped)
> ADDED: 2026-06-13
> LAST UPDATED: 2026-06-13
> PRIORITY: HIGH

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
1. `tf mcp` starts without error; emits MCP server protocol on stdout; reads tool invocations from stdin
2. `tf_gate` tool returns `{"verdict":"allow"|"deny","reason":"…","ceiling":{…}}`
3. `tf_budget_read` and `tf_budget_set` MCP tools work; state persists
4. `tf_report`, `tf_observe`, `tf_spend` MCP tools return valid JSON
5. `tf dashboard` starts on port 8080; serves HTML with embedded Chart.js charts
6. WebSocket at `ws://localhost:8080/ws` receives new honesty-events.jsonl lines in real time (within 1s)
7. `GET /metrics` returns valid Prometheus text format (when --prometheus flag used)
8. Binary size with `--release` and no `dashboard` feature is ≤105% of pre-change size
9. `cargo test --workspace` passes; 100% coverage of new code
10. `tf --help` lists `mcp` and `dashboard` subcommands

### Implementation Notes

**Phase B — MCP Server (COMPLETE as of 2026-06-13)**
- Implemented: `tf mcp` subcommand with stdio JSON-RPC 2.0 transport
- 10 MCP tools: tf_gate, tf_budget_read, tf_budget_set, tf_report, tf_observe, tf_spend, tf_signal, tf_plan_open, tf_plan_close, tf_schedule_toggle
- 3 MCP resources: tf://status, tf://calibration, tf://events
- Verdict adapter: maps scheduler verdicts (CONTINUE|HALT|DEFER|ASK|NO_SIGNAL) to MCP (allow|deny)
- All AC#1–4, AC#8, AC#10 satisfied; Phase C (dashboard/telemetry) pending
- Commits: feat(mcp) + 5 supporting docs; 26 integration + 13 unit tests (59 total, all passing)
- Binary-size test: default build (no MCP) unchanged; MCP build adds rmcp/tokio (feature-gated, AC#8 ✓)

**Phase C — Telemetry + Dashboard (PENDING)**
- Architecture: Embedded HTTP server (axum + tokio) as primary dashboard. Prometheus exporter at `/metrics` enables optional Grafana.
- Telemetry source: Existing JSONL files (`honesty-events.jsonl`, `estimator-accuracy.jsonl`, `calibration.json`, `session.json`) — no new collection required.
- Feature-gating: Dependencies (`axum`, `tokio`, `notify`) gated under `[features] dashboard` to preserve hook binary size.
- Will satisfy AC#5–7, AC#9

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
