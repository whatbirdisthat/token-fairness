# [1] MCP Server, Telemetry Pipeline & Dashboard — Implementation Plan

**Feature:** MCP Server, Telemetry Pipeline & Dashboard
**Roadmap item:** [1]
**Status:** IN PROGRESS → Phase A COMPLETE (ADRs passed); Phase B starting
**Created:** 2026-06-13
**Last updated:** 2026-06-13

---

## Summary

Add three new surfaces to the `tf` binary:
1. **MCP server** (`tf mcp`) — expose scheduler operations as Claude Code MCP tools
2. **Telemetry pipeline** — file-watcher + WebSocket streaming for real-time events
3. **Dashboard** (`tf dashboard`) — embedded HTTP server with Chart.js, REST endpoints, Prometheus metrics

All work is feature-gated to preserve the hook binary's size contract (AC#8).

## EARS Specification Summary

**Ubiquitous:**
- Stdio MCP transport (zero dependencies, standard protocol)
- HTTP dashboard on port 8080 (feature-gated)
- Backward-compatible CLI (no existing commands change)
- Feature-gated heavy deps (axum, tokio, notify) — hook binary unaffected

**Event-driven:**
- WebSocket broadcasts new JSONL lines in real time (within 1s)
- MCP tools return JSON verdicts matching AC#2 contracts
- HTTP serves embedded Chart.js on navigation to localhost:8080

**Optional:**
- Prometheus export at `GET /metrics` (when `--prometheus` flag set)

## Gherkin Scenarios (FEATURE/BDD Contract)

```gherkin
Feature: MCP Server surface
  Scenario: Agent calls tf_gate tool
    Given a Claude Code session with tf mcp registered
    When the agent invokes tool tf_gate with a ceiling payload
    Then the response is {"verdict":"allow"|"deny","reason":"…","ceiling":{…}}

  Scenario: Agent reads budget state
    Given tf_budget_read tool invoked
    Then response is JSON with session_cap, per_fanout_cap, current spend
    And state persists across multiple tool calls

Feature: Live Dashboard
  Scenario: User navigates to dashboard
    Given tf dashboard running on localhost:8080
    When user opens http://localhost:8080 in browser
    Then page loads with three Chart.js charts visible

  Scenario: Real-time chart updates
    Given dashboard running and WebSocket connected
    When a new line is appended to honesty-events.jsonl
    Then connected clients receive the event within 1 second
    And chart redraws without page reload

Feature: Prometheus Metrics
  Scenario: Grafana scrapes metrics
    Given tf dashboard running with --prometheus flag
    When Grafana scrapes GET /metrics
    Then response is valid Prometheus text format
    And metrics include tf_session_spend_tokens (gauge), tf_guard_saves_total (counter)
```

## Test Strategy

### Unit Tests (FOUNDRY mandate: 100% coverage)
- MCP verdict-mapping adapter: feed each source verdict, assert mapped output
- File-watcher path resolution: verify env-override honors, offset-reset on truncate
- Telemetry fold semantics: JS fold-parity invariant (Rust ↔ JS identical output)
- REST endpoint projections: assert spend dedup, period bucketing, MAPE calculation
- Prometheus serialization: validate text format, gauge/counter types correct

### Integration Tests
- `tf mcp` subprocess: full JSON-RPC round-trip with real `tf-core` state
- Dashboard HTTP server: serve embedded HTML, REST endpoints respond, WebSocket broadcasts
- File-watching loop: append to temp JSONL, assert broadcast within 100ms

### Story/E2E Tests (Playwright)
- MCP tool invocation from Claude Code (real MCP subprocess)
- Dashboard navigation and real-time chart updates (browser automation)
- Prometheus scrape and Grafana datasource integration (optional docker-compose path)

## Files to Create / Modify

### Phase B — MCP Server
| File | Action | Rationale |
|------|--------|-----------|
| `crates/tf-core/src/mcp.rs` | Create | MCP tool handlers, verdict adapter, resource serving |
| `crates/tf-core/src/lib.rs` | Modify | Add `pub mod mcp` |
| `crates/tf-cli/src/main.rs` | Modify | Add `mcp` verb dispatch |
| `crates/tf-cli/Cargo.toml` | Modify | Add `rmcp` (0.2.x), feature-gated under `mcp` |
| `crates/tf-cli/tests/mcp.rs` | Create | MCP tool round-trip tests, adapter unit tests |

### Phase C — Telemetry + Dashboard
| File | Action | Rationale |
|------|--------|-----------|
| `crates/tf-core/src/telemetry.rs` | Create | File watcher, WebSocket broadcaster, path resolution |
| `crates/tf-core/src/dashboard.rs` | Create | HTTP server (axum), REST endpoints, metrics export |
| `crates/tf-cli/src/dashboard_run.rs` | Create | `tf dashboard` CLI dispatch, arg parsing |
| `assets/dashboard.html` | Create | Embedded Chart.js HTML, SVG charts, WS client |
| `crates/tf-core/src/lib.rs` | Modify | Add `pub mod telemetry`, `pub mod dashboard` |
| `crates/tf-cli/src/main.rs` | Modify | Add `dashboard` verb dispatch |
| `crates/tf-cli/Cargo.toml` | Modify | Add `axum`, `tokio`, `notify` — feature-gated under `dashboard` |
| `crates/tf-cli/tests/dashboard.rs` | Create | File-watcher tests, REST endpoint tests, fold-parity test |

### Both phases
| File | Action |
|------|--------|
| `Cargo.toml` (workspace) | Modify — add `[features] mcp = ["rmcp"]` and `dashboard = ["axum", "tokio", "notify"]` |

## Acceptance Criteria Mapping

1. ✓ `tf mcp` starts without error (Phase B, step 5)
2. ✓ `tf_gate` returns verdict/reason/ceiling (Phase B, step 5 + integration test)
3. ✓ `tf_budget_read` / `tf_budget_set` work, state persists (Phase B, step 5)
4. ✓ `tf_report`, `tf_observe`, `tf_spend` return JSON (Phase B, step 5)
5. ✓ `tf dashboard` starts on port 8080, serves HTML (Phase C, step 5)
6. ✓ WebSocket receives events within 1s (Phase C, step 5 + story test)
7. ✓ `GET /metrics` is valid Prometheus format (Phase C, step 5 + story test)
8. ✓ Binary size ≤105% of pre-change (Phase B & C, step 6 — binary-size test)
9. ✓ `cargo test --workspace` passes, 100% coverage (Phase B & C, step 6)
10. ✓ `tf --help` lists `mcp` and `dashboard` (Phase B & C, step 0)

## Implementation Plan Checklist

### Phase B — MCP Server
- [ ] **Step 0 — Plan.** (in progress)
- [ ] **Step 1 — EARS.** Update `doc/SPECIFICATION.ears.md` with MCP tool IDs and contracts.
- [ ] **Step 2 — Features.** Write `.feature` files for MCP scenarios (Gherkin).
- [ ] **Step 3 — Tests.** Write failing tests (RED): MCP tool handlers, verdict adapter, resource serving.
- [ ] **Step 4 — Gap map.** Document what tests fail and why.
- [ ] **Step 5 — Implement.** Write `tf-core/src/mpc.rs`, wire into `tf-cli/src/main.rs`, add Cargo deps.
- [ ] **Step 6 — Green.** Drive all tests to GREEN; verify 100% coverage (mcp.rs module).
- [ ] **Step 7 — Sync upstream.** `git fetch origin && git rebase origin/fix/session-boundary-reset-and-help` (or main if merged).
- [ ] **Step 8 — Commit message.** Draft narrative (WHY/WHAT/TESTING/ROADMAP).
- [ ] **Step 9 — Commit & push.** Commit to branch; update ROADMAP.md status.

### Phase C — Telemetry + Dashboard
- [ ] **Step 0 — Plan.** (done above).
- [ ] **Step 1 — EARS.** Update spec with telemetry/dashboard IDs.
- [ ] **Step 2 — Features.** Write `.feature` files for dashboard/WebSocket scenarios.
- [ ] **Step 3 — Tests.** Write failing tests: file-watcher, WebSocket, REST endpoints, metrics, fold-parity.
- [ ] **Step 4 — Gap map.** Document failures.
- [ ] **Step 5 — Implement.** Write `telemetry.rs`, `dashboard.rs`, embed `assets/dashboard.html`, wire into CLI.
- [ ] **Step 6 — Green.** Drive to GREEN; fold-parity invariant test must pass (ADR-004 AC#9).
- [ ] **Step 7 — Sync upstream.** Rebase after Phase B is merged.
- [ ] **Step 8 — Commit message.** Draft narrative.
- [ ] **Step 9 — Commit & push.** Update ROADMAP.md to COMPLETE.

## Known Risks & Mitigations

| Risk | Mitigation |
|------|-----------|
| MCP `rmcp` has tokio; leaks into hook build | Feature-gated `mcp` feature (mandatory). Binary-size test protects AC#8. |
| File-watcher misses events or lags | inotify/FSEvents typically 10–100ms (well under 1s AC#6). Tests verify latency. Offset-reset on truncate prevents silent failure. |
| JS fold diverges from Rust fold | Fold-parity invariant test (AC#9): replay fixed sequence through both, assert identical. Breaks if divs occur. |
| Prometheus metrics drift from Grafana | Metric enum explicit with types. Gauge/counter split tied to `observe.rs` dedup semantics. Self-documenting. |
| Binary size regression | Workspace feature-gating: default build has no `mcp`/`dashboard` features. Separate size budgets for each. CI gate mandatory. |

## Resumption Instructions (if work pauses)

If Phase B pauses mid-step:
1. Read `doc/[1]_MCP_SERVER_TELEMETRY_DASHBOARD_PLAN.md` (this file).
2. Check which step is incomplete in the Phase B checklist above.
3. Return to that step. FOUNDRY step-N agents are resumable (see FOUNDRY skill resumption protocol).
4. Ensure `cargo test --workspace` is green before moving to step 7.

If both phases pause before commit:
1. Verify `cargo build --release --features mcp --features dashboard` is green (integration of both).
2. Verify `cargo build --release` (default, no features) is within AC#8 binary-size budget.
3. Update ROADMAP.md status to reflect pause state (e.g., `STATUS: SUSPENDED` with reason).
4. On resume, FOUNDRY agents re-read the plan and the suspended state, then continue from the last incomplete step.

## References

- `doc/ROADMAP.md` § [1] — feature entry, acceptance criteria
- `doc/design/adr/ADR-001-mcp-transport.md` — MCP architecture decision
- `doc/design/adr/ADR-002-dashboard-architecture.md` — dashboard architecture decision
- `doc/design/adr/ADR-003-telemetry-pipeline.md` — telemetry pipeline decision
- `doc/design/adr/ADR-004-chart-rendering.md` — chart rendering decision
- FOUNDRY skill documentation — THE DEVELOPMENT SYSTEM (9-step process)
- Gherkin/Cucumber spec — BDD scenarios written in `.feature` files
