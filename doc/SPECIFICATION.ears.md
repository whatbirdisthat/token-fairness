# EARS Specification — token-fairness

**Edition:** 1.0  
**Created:** 2026-06-13  
**Last updated:** 2026-06-13  
**Status:** ACTIVE (Phase B: MCP Server, Phase C: Telemetry + Dashboard)

## Ubiquitous Statements

**US-001: Backward-compatible CLI**  
The system SHALL NOT modify any existing CLI verbs, options, or output format. New features are opt-in surfaces (`tf mcp`, `tf dashboard`) accessible via new top-level verbs.

**US-002: Feature-gated dependencies**  
The system SHALL gate heavy dependencies (`rmcp`, `tokio`, `axum`, `notify`) behind Cargo features (`mcp`, `dashboard`) so that the default hook build excludes them entirely. The default `cargo build --release` (no features) SHALL remain within AC#8's binary-size budget (≤105% of pre-change size).

**US-003: Stdio MCP transport**  
The system SHALL expose token-scheduler operations as MCP tools over stdio transport (JSON-RPC 2.0), implemented via the `rmcp` crate (0.2.x), invoked as `tf mcp`.

**US-004: Pure core, no I/O**  
All logic in `crates/tf-core` SHALL be pure domain logic with no I/O, no platform code, no `unwrap()`/`expect()`/`panic!()` outside tests, and no unchecked indexing. Errors are typed `thiserror` enums.

**US-005: 100% test coverage**  
The system SHALL achieve 100% line coverage and 100% branch coverage. Every function and branch has a test; every error path is deliberately triggered and asserted. The gate `cargo test --workspace` SHALL pass with all tests green.

---

## MCP-specific Statements (Phase B)

### Tool Contracts

**MCP-001: tf_gate tool**  
The `tf_gate` MCP tool SHALL accept a JSON-RPC request with inputSchema `{ ceiling: { "used_pct": number, "headroom": number } }` and return a result object with fields:
- `verdict` (string): one of `"allow"` or `"deny"`
- `reason` (string, always present): a human-readable justification (e.g., `"ceiling exceeded"`, `"no live signal"`)
- `ceiling` (object): the ceiling object from the scheduler's native verdict (for client reference)

The verdict mapping is:
- Scheduler `"CONTINUE"` → MCP `"allow"`
- Scheduler `"HALT"` | `"DEFER"` | `"ASK"` | `"NO_SIGNAL"` → MCP `"deny"`

**MCP-002: tf_budget_read tool**  
The `tf_budget_read` MCP tool SHALL accept an empty JSON-RPC request (no inputSchema required) and return a result object with fields:
- `session_cap` (integer): the session budget ceiling in tokens
- `per_fanout_cap` (integer): the per-fan-out budget ceiling in tokens
- `current_spend` (integer): total tokens spent in the current session
- `fanout_spend` (integer): tokens spent in the current fan-out window (resets on boundary)

**MCP-003: tf_budget_set tool**  
The `tf_budget_set` MCP tool SHALL accept a JSON-RPC request with inputSchema `{ key: string, value: integer }` where key ∈ `{"session_cap", "per_fanout_cap"}` and update the corresponding field in the budget state file. On success, return `{ "success": true, "key": <key>, "new_value": <value> }`.

**MCP-004: tf_report tool**  
The `tf_report` MCP tool SHALL accept an optional JSON-RPC request with inputSchema `{ window: "hour" | "day" | "month" | "ytd" }` (default: `"day"`) and return a result object with fields:
- `window` (string): the requested time window
- `window_open` (integer, unix seconds): the start timestamp of the window
- `window_close` (integer, unix seconds): the end timestamp of the window
- `spend_total` (integer): total tokens spent in the window
- `gate_denials` (integer): count of denials by the gate (verdict = `"HALT"` | `"DEFER"` | `"ASK"`)

**MCP-005: tf_observe tool**  
The `tf_observe` MCP tool SHALL accept an optional JSON-RPC request with inputSchema `{ window: "hour" | "day" | "month" | "ytd" }` (default: `"day"`) and return a result array of snapshot objects, one per deduplicated observation, with fields:
- `span_id` (string): the unique span token
- `cost_tokens` (integer): the token cost of this span
- `model` (string): the model invoked (e.g., `"claude-opus-4"`)
- `role` (string): the role that invoked it (e.g., `"researcher"`)

The array is sorted chronologically by first observation timestamp.

**MCP-006: tf_spend tool**  
The `tf_spend` MCP tool SHALL accept an optional JSON-RPC request with inputSchema `{ span_id: string, cost: integer, model: string, role: string }` and record a new spend event to the ledger. On success, return `{ "success": true, "span_id": <span_id>, "cost": <cost> }`.

**MCP-007: tf_signal tool**  
The `tf_signal` MCP tool SHALL accept a JSON-RPC request with inputSchema `{ name: "gate" | "budget" | "observability", status: "OK" | "ERROR" }` and record a signal event to the signals registry. On success, return `{ "success": true, "signal": <name>, "status": <status> }`.

**MCP-008: tf_plan_open tool**  
The `tf_plan_open` MCP tool SHALL accept a JSON-RPC request with inputSchema `{ title: string, budget_tokens: integer }` and create a new open-ended budget plan. On success, return `{ "success": true, "plan_id": string, "title": string, "budget_tokens": integer }`.

**MCP-009: tf_plan_close tool**  
The `tf_plan_close` MCP tool SHALL accept a JSON-RPC request with inputSchema `{ plan_id: string }` and mark the plan as closed (no further spend against it). On success, return `{ "success": true, "plan_id": string, "closed_at": integer }`.

**MCP-010: tf_schedule_toggle tool**  
The `tf_schedule_toggle` MCP tool SHALL accept a JSON-RPC request with inputSchema `{ enabled: boolean }` and toggle the window-aware schedule gate's enabled state. On success, return `{ "success": true, "enabled": <enabled> }`.

### Resource Contracts

**Resource-001: tf://status**  
The `tf://status` MCP resource SHALL return a JSON object with a live snapshot of all current state files:
- `session_budget`: session budget state (session_cap, per_fanout_cap, current_spend, fanout_spend)
- `signals`: current signal states (gate, budget, observability)
- `timestamp`: unix seconds of the snapshot

The resource is read-only and updated on each request (always fresh).

**Resource-002: tf://calibration**  
The `tf://calibration` MCP resource SHALL return a JSON object with calibration data:
- `rolling_windows`: array of window definitions (hour, day, month, ytd with open/close times)
- `current_window`: the current active window name
- `next_boundary`: unix seconds of the next window boundary

The resource is read-only.

**Resource-003: tf://events**  
The `tf://events` MCP resource SHALL return a JSON array of recent event summaries (last 100 events, JSONL deserialized):
- Each element: `{ "timestamp": int, "kind": string, "details": object }`
- Kinds: `"spend"`, `"signal"`, `"plan_open"`, `"plan_close"`, `"gate_verdict"`

The resource is read-only and truncated to recent 100 events for efficiency.

### Feature Gate

**Feature-001: `mcp` Cargo feature**  
The system SHALL define a Cargo feature `mcp` that gates the `rmcp` dependency and all MCP server code. When disabled (the default), `cargo build --release` SHALL NOT include `rmcp` or `tokio` symbols.

**Feature-002: Workspace feature centralization**  
The workspace root `Cargo.toml` SHALL define `[features]` with `mcp = ["rmcp"]` so that all crates inherit the gate consistently. When enabled, `tf-core` and `tf-cli` both gain access to MCP modules.

### Dispatch & Registration

**MCP-011: CLI dispatch for `tf mcp`**  
The CLI `crates/tf-cli/src/main.rs` SHALL recognize the `mcp` verb and spawn the MCP server via `rmcp::Server` when invoked as `tf mcp`. The server reads JSON-RPC 2.0 from stdin and writes responses to stdout. The process terminates on stdin EOF.

**MCP-012: MCP server initialization**  
On startup, the MCP server SHALL:
1. Register all 10 tools (MCP-001 through MCP-010) with `rmcp::Server`
2. Register all 3 resources (Resource-001 through Resource-003) with `rmcp::Server`
3. Enter the JSON-RPC request/response loop via `rmcp::Server::run()`

The server is **not** a daemon; it runs in the foreground and terminates on parent exit (Claude Code controls the subprocess lifecycle).

### Error Handling

**MCP-013: JSON-RPC error responses**  
When an MCP tool handler encounters an error:
1. Serialize it as a JSON-RPC 2.0 error response: `{ "jsonrpc": "2.0", "error": { "code": <int>, "message": <string> }, "id": <id> }`
2. Use error codes per JSON-RPC spec (e.g., `-32600` for invalid request, `-32601` for method not found)
3. Never panic or return an unstructured string error

**MCP-014: Tool handler fallibility**  
Every MCP tool handler (MCP-001 through MCP-010) is fallible. Handlers that fail to read state files, parse JSON, or compute results SHALL return a JSON-RPC error response. No unwrap/expect/panic in production code outside tests.

### Testing

**Test-MCP-001: Verdict-mapping adapter unit tests**  
The test suite SHALL include unit tests that feed each of the five scheduler verdicts (`CONTINUE`, `HALT`, `DEFER`, `ASK`, `NO_SIGNAL`) into the adapter and assert the mapped MCP verdict (`allow` or `deny`), reason presence, and ceiling presence.

**Test-MCP-002: Tool round-trip integration tests**  
The test suite SHALL include integration tests that spawn `tf mcp` as a subprocess, send valid JSON-RPC requests (e.g., `tf_gate` with a ceiling payload), read the responses, and assert on the result structure (verdict, reason, ceiling fields).

**Test-MCP-003: Resource serving tests**  
The test suite SHALL include tests that request each MCP resource (tf://status, tf://calibration, tf://events) and assert the response is valid JSON with expected keys.

**Test-MCP-004: Error-path tests**  
The test suite SHALL include tests that send malformed JSON-RPC requests (missing params, unknown method, invalid JSON) and assert the server responds with appropriate JSON-RPC errors.

**Test-MCP-005: Coverage requirement**  
All new code in `crates/tf-core/src/mcp.rs` and the MCP dispatch in `crates/tf-cli/src/main.rs` SHALL achieve 100% line and branch coverage. The test-suite gate `cargo test --workspace` is mandatory.

### No Flaky Tests

**Test-MCP-006: Deterministic tests**  
The test suite SHALL be run 3 times in a row with `cargo test --workspace mcp` and pass all 3 times without failures. Tests that depend on timing SHALL use deterministic waits or mocks, never sleep loops. Tests that interact with temp files SHALL use `testutil::temp_dir()` with proper cleanup.

---

## Dashboard-specific Statements (Phase C)

### HTTP Server & Static Assets

**DASH-001: HTTP server on port 8080**  
The `tf dashboard` command SHALL spawn an HTTP server (axum-based) listening on `127.0.0.1:8080`. The server SHALL:
1. Serve an embedded Chart.js dashboard HTML at `GET /` (no server-side templating)
2. Provide REST JSON endpoints for real-time state snapshots (see DASH-002)
3. Broadcast WebSocket events at `ws://127.0.0.1:8080/ws` (see TELEM-002)
4. Emit Prometheus metrics at `GET /metrics` (optional, see PROM-001)

**DASH-002: REST state endpoints**  
The HTTP server SHALL provide the following JSON endpoints (all GET, no auth):
- `GET /api/session-budget` → JSON object with `{ session_cap, per_fanout_cap, current_spend, ceiling_pct }`
- `GET /api/spend-by-model` → JSON array of `{ model, tokens, count }` for all models in current session
- `GET /api/guard-efficacy` → JSON object with `{ saves_count, blown_count, save_rate_pct }`
- `GET /api/estimator-accuracy` → JSON object with `{ mean_absolute_percentage_error, min_error_pct, max_error_pct }`

All responses are snapshots of the most recent fold of the event logs (see TELEM-003).

**DASH-003: Embedded HTML & assets**  
The HTML asset at `assets/dashboard.html` SHALL be embedded in the binary at compile time (no external HTTP fetch). The page SHALL:
1. Load three Chart.js charts: a gauge (spend %), a pie (by model), and a line trend (SAVES vs BLOWN over time)
2. Establish a WebSocket connection to `ws://127.0.0.1:8080/ws` on page load
3. Replay the live fold state from REST `/api/…` endpoints on first load
4. Update charts incrementally as WebSocket events arrive (see ADR-004 for parity requirement)
5. Tolerate WebSocket disconnection and reconnect on demand

---

### Telemetry Pipeline & File Watcher

**TELEM-001: File-watcher initialization**  
On `tf dashboard` startup, the system SHALL:
1. Resolve the path to `honesty-events.jsonl` via `observe::events_path()` (NOT hardcoded, NOT from CLI arg). Path resolution respects `TF_*` env overrides per ADR-003.
2. If the file does not exist, create it as an empty file (0 bytes).
3. Open the file and record the current byte offset (EOF).
4. Spawn an inotify-based watcher (via `notify` crate) to monitor the file for appends (see TELEM-002).

**TELEM-002: Real-time WebSocket broadcast**  
When a watched file is appended to:
1. The watcher detects the append event (inotify on Linux, FSEvents on macOS, platform-native elsewhere).
2. The system reads the new bytes from the recorded offset to the current EOF.
3. Each appended line (JSONL) is parsed and broadcast to all connected WebSocket clients in a single JSON message: `{ "type": "event", "data": <parsed JSONL object> }`
4. The offset is advanced to the new EOF.
5. Broadcast latency SHALL be within 100 ms (best-effort, no buffering on client disconnect).

**TELEM-003: Telemetry fold semantics**  
The system SHALL maintain a live fold of all JSONL events (from all source files: `honesty-events.jsonl`, `estimator-accuracy.jsonl`) using the same semantics as `observe.rs:fold_events()`:

1. **Session spend (gauge):** Latest cumulative value per session (dedup to most recent event). Resets on session boundary.
2. **Spend by model (pie):** Sum tokens per unique model across all observations in current session.
3. **Guard saves/blown (counter trend):** Bin events by their period window (`hour`, `day`, etc.), count saves/blown per bin, emit as time-series.
4. **Estimator accuracy (MAPE):** Fold the estimator-accuracy JSONL ledger, calculate mean absolute percentage error, emit per-period breakdown.

This fold is computed on every REST request and on every WebSocket broadcast. The fold logic SHALL EXACTLY replicate `observe.rs:fold_events()` — see ADR-004 (fold-parity invariant, AC#9).

---

### Telemetry Robustness

**TELEM-004: Truncation handling**  
If the watched file is truncated (size < current offset), the system SHALL:
1. Reset the offset to 0.
2. Seek to EOF to record the new offset.
3. Continue watching for appends (no panic, no silent failure).

This handles log rotation gracefully without requiring restart.

---

## Prometheus Metrics (Phase C)

**PROM-001: Prometheus endpoint & flag**  
When the `tf dashboard` command is invoked with `--prometheus` flag:
1. The HTTP server SHALL emit Prometheus-format metrics at `GET /metrics`.
2. Format is plain text, one metric per line, per Prometheus docs.
3. Metrics are current snapshots of the folded event state (see TELEM-003).

**PROM-002: Gauge metrics (resettable)**  
The system SHALL emit the following **gauge** metrics (allow decrease):
- `tf_session_spend_tokens` — current session cumulative spend (integer). Resets to 0 on session boundary.
- `tf_session_ceiling_percent` — spend as % of session ceiling (0–100, floating-point). Updated on every new event.
- `tf_weekly_ceiling_percent` — spend as % of 7-day rolling window ceiling (0–100, floating-point).

**PROM-003: Counter metrics (monotonic)**  
The system SHALL emit the following **counter** metrics (never decrease, always increase):
- `tf_guard_saves_total` — total count of guard SAVE verdicts across all time (integer, monotonic).
- `tf_guard_blown_total` — total count of guard BLOWN verdicts across all time (integer, monotonic).
- `tf_guard_procedural_denies_total` — total count of procedural denials (verdict = NOT CONTINUE) across all time (integer, monotonic).

**PROM-004: Metric help strings**  
Each metric in PROM-002 and PROM-003 SHALL include a help comment line (prefixed with `# HELP`) explaining its meaning and units.

---

### Feature Gate for Dashboard

**Feature-003: `dashboard` Cargo feature**  
The system SHALL define a Cargo feature `dashboard` that gates the `axum`, `tokio`, and `notify` dependencies and all dashboard/telemetry code. When disabled (the default), `cargo build --release` SHALL NOT include these symbols. The default build SHALL remain within AC#8's binary-size budget (≤105% of pre-change size).

**Feature-004: Workspace feature centralization**  
The workspace root `Cargo.toml` SHALL define `[features]` with `dashboard = ["axum", "tokio", "notify"]` so that all crates inherit the gate consistently.

---

### CLI Dispatch & Registration

**DASH-004: CLI dispatch for `tf dashboard`**  
The CLI `crates/tf-cli/src/main.rs` SHALL recognize the `dashboard` verb and dispatch to a handler that:
1. Parses optional `--prometheus` flag
2. Calls the dashboard server initialization (see DASH-001)
3. Binds to `127.0.0.1:8080` and runs the server in the foreground

The server is a foreground process; it terminates on SIGINT (Ctrl+C) or parent exit.

---

### Testing

**Test-DASH-001: File-watcher path resolution**  
The test suite SHALL include a test that sets `TF_EVENTS_DIR` env var and verifies the watcher resolves the correct path via `observe::events_path()`, not a hardcoded path.

**Test-DASH-002: Truncation robustness**  
The test suite SHALL include a test that truncates the watched file mid-stream and verifies:
1. The watcher does NOT panic.
2. The offset is reset correctly.
3. New appends are read after truncation.

**Test-DASH-003: WebSocket event ordering**  
The test suite SHALL include an integration test that:
1. Starts the dashboard server with a temp JSONL file.
2. Appends a sequence of events to the file.
3. Opens a WebSocket client and collects all broadcast events.
4. Asserts events are received in order and match the appended JSONL exactly.

**Test-DASH-004: REST endpoint snapshots**  
The test suite SHALL include tests for each REST endpoint (DASH-002) that:
1. Set up a known event state in a temp JSONL file.
2. Query the endpoint.
3. Assert the response JSON matches the expected fold (see TELEM-003).

**Test-DASH-005: Fold-parity invariant (CRITICAL)**  
The test suite SHALL include a property-based test (proptest) that:
1. Generates a fixed sequence of 50 synthetic JSONL events (spend, saves, blown, etc.).
2. Feeds the sequence into `observe.rs:fold_events()` (Rust) and the embedded JavaScript fold function (see assets/dashboard.html).
3. Asserts the final fold state (spend, saves_count, blown_count, MAPE) is identical in both implementations.
4. This test is CRITICAL (AC#9): if it fails, live charts (WS feed) diverge from reloaded charts (REST feed).

**Test-DASH-006: Prometheus format validation**  
The test suite SHALL include a test that:
1. Starts the dashboard with `--prometheus` flag.
2. Queries `GET /metrics`.
3. Parses the response as Prometheus text format.
4. Asserts exactly 6 metrics are present with correct types (gauges, counters).

**Test-DASH-007: Binary-size constraint (AC#8)**  
The test suite SHALL include a test that:
1. Builds `cargo build --release` (default, no features).
2. Measures binary size.
3. Asserts size is ≤105% of pre-change baseline.

**Test-DASH-008: Coverage requirement**  
All new code in `crates/tf-core/src/telemetry.rs`, `crates/tf-core/src/dashboard.rs`, and `crates/tf-cli/src/dashboard_run.rs` SHALL achieve 100% line and branch coverage. The gate `cargo test --workspace` is mandatory.

**Test-DASH-009: No flaky tests**  
The test suite SHALL be run 3 times in a row with `cargo test --workspace dashboard` and pass all 3 times without failures.

---

## Non-Dashboard Statements (Inherited from Prior Phases)

**S-001 through S-100:** [Omitted — see ROADMAP.md AC#1–10, prior iteration commits for pre-MCP requirements]

---

## Acceptance Criteria Traceability

| AC # | Requirement | EARS Statement(s) |
|------|-----------|----------|
| 1 | `tf mcp` starts without error | MCP-011, Feature-001 |
| 2 | `tf_gate` returns {verdict, reason, ceiling} | MCP-001, Test-MCP-001 |
| 3 | `tf_budget_read`/`tf_budget_set` work, state persists | MCP-002, MCP-003 |
| 4 | `tf_report`, `tf_observe`, `tf_spend` return JSON | MCP-004, MCP-005, MCP-006 |
| 5 | `tf dashboard` serves HTML with Chart.js | DASH-001, DASH-003, DASH-004, Test-DASH-003 |
| 6 | WebSocket receives events within 1s | TELEM-002, Test-DASH-003 |
| 7 | `GET /metrics` valid Prometheus format | PROM-001, PROM-002, PROM-003, Test-DASH-006 |
| 8 | Binary size ≤105% of pre-change | US-002, Feature-003, Feature-004, Test-DASH-007 |
| 9 | Fold-parity invariant (JS ↔ Rust match) | TELEM-003, Test-DASH-005 (CRITICAL) |
| 10 | `tf --help` lists `mcp` and `dashboard` | US-001 (new verbs visible) |

---

## Revision History

| Date | Editor | Change |
|------|--------|--------|
| 2026-06-13 | Handler | Initial: MCP-001 through MCP-010, Resource-001–003, Feature gates, error handling, test contracts |
| 2026-06-13 | Handler | Phase C: DASH-001–004, TELEM-001–004, PROM-001–004, Feature-003–004, Test-DASH-001–009, AC mapping updated |
