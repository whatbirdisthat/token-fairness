# Phase C Gap Map — Why Tests Fail

**Date:** 2026-06-13  
**Status:** RED (39 failing tests, all #[ignore]d)

## Overview

Phase C introduces the telemetry pipeline and HTTP dashboard for the `tf` binary. All 39 tests are marked `#[ignore]` because the following modules have NOT been implemented yet:

1. `crates/tf-core/src/telemetry.rs` — File watcher, WebSocket broadcaster, offset tracking
2. `crates/tf-core/src/dashboard.rs` — HTTP server (axum), REST endpoints, metrics export
3. `crates/tf-cli/src/dashboard_run.rs` — CLI dispatch for `tf dashboard`
4. `assets/dashboard.html` — Embedded Chart.js HTML with JavaScript fold function
5. CLI integration — `tf dashboard` verb not yet wired into `crates/tf-cli/src/main.rs`
6. Cargo features — `dashboard` feature added to `Cargo.toml` and `crates/tf-cli/Cargo.toml`, but no conditional code yet

## Test Categories & Gaps

### 1. File-Watcher Tests (5 tests) — TELEM-001, TELEM-004, Test-DASH-001, Test-DASH-002

**Gap:** No `telemetry.rs` module.

**What's needed:**
- `pub fn watch_events_file() -> Result<Watcher, Error>` — Initialize file watcher at `observe::events_path()`, create empty file if missing, record initial offset
- `pub fn on_file_append(watcher: &mut Watcher, offset: &mut u64) -> Result<Vec<String>, Error>` — Detect appends, read new bytes, update offset
- **Truncation handling:** When file size < offset, reset offset to 0 (EOF)
- Error type: `enum TelemetryError { FileNotFound, IoError, WatcherError }`

**Tests blocked:** All 5 file-watcher tests (#[ignore])

---

### 2. Fold Semantics Tests (4 tests) — TELEM-003, Test-DASH-004

**Gap:** No fold implementation in `telemetry.rs` or `dashboard.rs`. The fold logic must exactly replicate `observe.rs:fold_events()` semantics.

**What's needed:**
- Struct `FoldState` with fields:
  - `session_spend: u64` — latest cumulative (dedup)
  - `spend_by_model: HashMap<String, (u64, u32)>` — (tokens, count) per model
  - `saves_count: u32` — total SAVE verdicts
  - `blown_count: u32` — total BLOWN verdicts
  - `mape: f64` — mean absolute percentage error
  - `periods: Vec<PeriodBucket>` — time-series by period
- `pub fn fold_events(events: &[Event]) -> FoldState` — Replicate `observe.rs:fold_events()` logic exactly

**Tests blocked:** 4 fold tests (#[ignore])

---

### 3. Fold-Parity Invariant (2 tests) — AC#9, Test-DASH-005 **[CRITICAL]**

**Gap:** JavaScript fold function does not exist in `assets/dashboard.html`.

**What's needed:**
- Embed a JavaScript function `function foldEvents(events) { … }` in `assets/dashboard.html`
- Implement identical fold logic to Rust version:
  - Dedup spend to latest per session
  - Bin SAVES/BLOWN by period
  - Calculate MAPE
- Unit test that:
  - Generates 50 synthetic JSONL events
  - Feeds into Rust fold
  - Feeds into JS fold (via JavaScript executor or manual validation)
  - Asserts identical output

**Critical dependency:** This test MUST pass. If JS fold diverges from Rust fold, live charts (WS + JS) diverge from reloaded charts (REST + Rust).

**Tests blocked:** 2 fold-parity tests (#[ignore])

---

### 4. REST Endpoint Tests (4 tests) — DASH-002, Test-DASH-004

**Gap:** No HTTP server in `dashboard.rs`.

**What's needed:**
- `crates/tf-core/src/dashboard.rs`:
  - `pub async fn start_server(port: u16, prometheus: bool) -> Result<(), DashboardError>`
  - Routes:
    - `GET /api/session-budget` → JSON `{ session_cap, per_fanout_cap, current_spend, ceiling_pct }`
    - `GET /api/spend-by-model` → JSON array `[{ model, tokens, count }, …]`
    - `GET /api/guard-efficacy` → JSON `{ saves_count, blown_count, save_rate_pct }`
    - `GET /api/estimator-accuracy` → JSON `{ mean_absolute_percentage_error, min_error_pct, max_error_pct }`
  - Each endpoint computes a fresh fold from JSONL files
- Error type: `enum DashboardError { FoldError, HttpError, IoError }`

**Tests blocked:** 4 REST endpoint tests (#[ignore])

---

### 5. WebSocket Broadcasting Tests (5 tests) — TELEM-002, Test-DASH-003

**Gap:** No WebSocket handler in `dashboard.rs`.

**What's needed:**
- Route `GET /ws` (WebSocket upgrade)
- Maintain set of connected clients (Arc<Mutex<Vec<Sender>>>)
- Broadcast incoming JSONL events:
  - On file append, broadcast `{ "type": "event", "data": <parsed_json> }`
  - Latency SLA: within 100 ms
- Handle disconnection gracefully (remove from set)

**Tests blocked:** 5 WebSocket tests (#[ignore])

---

### 6. Prometheus Metrics Tests (8 tests) — PROM-001–004, Test-DASH-006

**Gap:** No Prometheus serialization in `dashboard.rs`.

**What's needed:**
- `pub struct PrometheusMetrics { … }` with 6 fields:
  - `session_spend_tokens: u64` (gauge)
  - `session_ceiling_percent: f64` (gauge)
  - `weekly_ceiling_percent: f64` (gauge)
  - `guard_saves_total: u64` (counter)
  - `guard_blown_total: u64` (counter)
  - `guard_procedural_denies_total: u64` (counter)
- `impl Display for PrometheusMetrics` — format as Prometheus text:
  ```
  # HELP tf_session_spend_tokens Current session cumulative spend
  # TYPE tf_session_spend_tokens gauge
  tf_session_spend_tokens 1500
  
  # HELP tf_guard_saves_total Total count of SAVE verdicts
  # TYPE tf_guard_saves_total counter
  tf_guard_saves_total 10
  ```
- Route `GET /metrics` (only if `--prometheus` flag set)

**Tests blocked:** 8 Prometheus tests (#[ignore])

---

### 7. Static Asset Tests (1 test) — DASH-003

**Gap:** No `assets/dashboard.html` file.

**What's needed:**
- Create `assets/dashboard.html` with:
  - Chart.js library (CDN link or embedded)
  - Three Chart.js charts (gauge, pie, line)
  - Embedded JavaScript fold function (see fold-parity requirement)
  - WebSocket client that connects to `ws://localhost:8080/ws`
  - On first load, fetch REST endpoints to populate charts
  - On WS event, update charts incrementally
- Embed HTML in binary at compile time via `include_str!()` macro

**Tests blocked:** 1 static asset test (and indirectly all HTTP tests)

---

### 8. CLI Integration Tests (3 tests) — DASH-004, Test-DASH-001

**Gap:** No CLI dispatch for `tf dashboard`.

**What's needed:**
- `crates/tf-cli/src/dashboard_run.rs` with:
  - `pub fn run(args: DashboardArgs) -> Result<(), Error>`
  - Parse `--prometheus` flag
  - Bind to port 8080
  - Call `dashboard::start_server()`
- `crates/tf-cli/src/main.rs`:
  - Add `"dashboard"` case in verb match
  - Dispatch to `dashboard_run::run()`
- Help text: `tf --help` and `tf dashboard --help`

**Tests blocked:** 3 CLI integration tests (#[ignore])

---

### 9. Binary-Size Tests (3 tests) — AC#8, Test-DASH-007

**Gap:** Dashboard dependencies (axum, tokio, notify) not yet feature-gated in conditional code (though Cargo.toml gates are in place).

**What's needed:**
- Verify `cargo build --release` (no features) compiles and excludes dashboard symbols
- Verify `cargo build --release --features dashboard` compiles and includes them
- Measure binary size: default must be ≤105% of pre-change baseline

**Pre-change baseline (from Phase B):** To be measured when Phase C implementation is complete.

**Tests blocked:** 3 binary-size tests (#[ignore])

---

### 10. Error Handling & Robustness Tests (4 tests)

**Gap:** No error handling in modules that don't exist yet.

**What's needed:**
- Graceful handling of missing event files (return zero-filled JSON)
- WebSocket error handling (don't panic on malformed JSON)
- File-watcher recovery from file moves/renames
- HTTP error responses with 500 or 503 status

**Tests blocked:** 4 error-handling tests (#[ignore])

---

## Implementation Order (STEP 5)

To move tests from RED to GREEN, implement in this order:

1. **Telemetry module** (`tf-core/src/telemetry.rs`):
   - File-watcher initialization
   - Offset tracking & truncation handling
   - Event parsing

2. **Fold logic** (in `observe.rs` or `telemetry.rs`):
   - Replicate `fold_events()` semantics
   - Dedup, binning, MAPE calculation

3. **Dashboard module** (`tf-core/src/dashboard.rs`):
   - Axum HTTP server
   - REST endpoints (using fold)
   - WebSocket broadcaster
   - Prometheus metrics serialization

4. **Static assets** (`assets/dashboard.html`):
   - Chart.js HTML
   - JavaScript fold function
   - WebSocket client

5. **CLI dispatch** (`tf-cli/src/dashboard_run.rs`, `main.rs` modification):
   - Argument parsing
   - Server startup

6. **Feature-gating validation**:
   - Verify default build has no dashboard symbols
   - Verify feature build includes them

## Test Removal Strategy

Once each module is implemented:
1. Remove `#[ignore]` from corresponding tests
2. Run tests to verify they pass
3. Run full suite 3 times to check for flakiness
4. Verify 100% coverage with `cargo tarpaulin --out Html`

Final verification: `cargo test --workspace --all-features` must pass with 100% coverage.

---

## Acceptance Criteria Status

| AC # | Requirement | Status | Tests | Blocking |
|------|-----------|--------|-------|----------|
| 1 | `tf mcp` starts | ✅ PASS (Phase B) | 0 | No |
| 2 | `tf_gate` returns JSON | ✅ PASS (Phase B) | 0 | No |
| 3 | `tf_budget_read/set` work | ✅ PASS (Phase B) | 0 | No |
| 4 | MCP tools return JSON | ✅ PASS (Phase B) | 0 | No |
| 5 | `tf dashboard` serves HTML | ❌ RED | 3 | **YES** |
| 6 | WebSocket receives within 1s | ❌ RED | 5 | **YES** |
| 7 | Prometheus metrics valid | ❌ RED | 8 | No (optional feature) |
| 8 | Binary size ≤105% | ⏳ PENDING | 3 | **YES** |
| 9 | Fold-parity invariant | ❌ RED | 2 | **CRITICAL** |
| 10 | `tf --help` lists commands | ⏳ PENDING | 1 | **YES** |

**Critical path to completion:**
1. Implement telemetry.rs (file-watcher)
2. Implement fold logic
3. Implement dashboard.rs (HTTP server, REST, WS)
4. Create assets/dashboard.html with JS fold
5. Wire CLI dispatch
6. Run all tests 3× to verify no flakiness
7. Commit & push

---

**ESTIMATED EFFORT:** 4–6 hours of focused implementation.
