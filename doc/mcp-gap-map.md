# MCP Server — Gap Map (Test-Driven Development Baseline)

**Created:** 2026-06-13  
**Status:** RED (STEP 4 of FOUNDRY pipeline)  
**Purpose:** Document which tests are RED and why, establishing the baseline before implementation.

## Test Summary

Total tests written: 26  
- Tests SKIPPED (placeholders): 7 (verdict adapter unit tests)
- Tests IGNORED (`#[ignore]`): 19 (integration/tool/resource tests)
- Tests PASSING: 0 (no implementations yet)

## Test Categories

### 1. Verdict-Mapping Adapter Unit Tests (7 tests)

These test the pure adapter function that maps scheduler verdicts to MCP verdicts.

**Status:** Currently SKIPPED (placeholder logic only)

```
✗ test_verdict_adapter_continue_maps_to_allow — FAIL
  Reason: adapter function not implemented
  
✗ test_verdict_adapter_halt_maps_to_deny — FAIL
  Reason: adapter function not implemented
  
✗ test_verdict_adapter_defer_maps_to_deny — FAIL
  Reason: adapter function not implemented
  
✗ test_verdict_adapter_ask_maps_to_deny — FAIL
  Reason: adapter function not implemented
  
✗ test_verdict_adapter_no_signal_maps_to_deny — FAIL
  Reason: adapter function not implemented
  
✗ test_verdict_adapter_reason_always_present — FAIL
  Reason: adapter function not implemented
  
✗ test_verdict_adapter_ceiling_always_present — FAIL
  Reason: adapter function not implemented
```

### 2. MCP Tool Round-Trip Integration Tests (10 tests)

These spawn `tf mcp` as a subprocess and send JSON-RPC requests.

**Status:** All IGNORED (`#[ignore]` due to subprocess spawn failures)

```
✗ test_tf_gate_tool_allow_verdict — FAIL
  Reason: `tf mcp` command not implemented; subprocess spawn fails
  
✗ test_tf_gate_tool_deny_verdict — FAIL
  Reason: same
  
✗ test_tf_budget_read_returns_state — FAIL
  Reason: same
  
✗ test_tf_budget_set_updates_state — FAIL
  Reason: same
  
✗ test_tf_report_returns_spend_window — FAIL
  Reason: same
  
✗ test_tf_observe_returns_deduplicated_spans — FAIL
  Reason: same
  
✗ test_tf_spend_records_event — FAIL
  Reason: same
  
✗ test_tf_signal_sets_state — FAIL
  Reason: same
  
✗ test_tf_plan_open_creates_plan — FAIL
  Reason: same
  
✗ test_tf_plan_close_closes_plan — FAIL
  Reason: same
```

### 3. Additional MCP Tool Tests (1 test)

```
✗ test_tf_schedule_toggle — FAIL
  Reason: `tf mcp` command not implemented
```

### 4. MCP Resource Tests (3 tests)

These request MCP resources (tf://status, tf://calibration, tf://events).

**Status:** All IGNORED

```
✗ test_resource_tf_status — FAIL
  Reason: `tf mcp` command not implemented
  
✗ test_resource_tf_calibration — FAIL
  Reason: same
  
✗ test_resource_tf_events — FAIL
  Reason: same
```

### 5. Error Handling Tests (4 tests)

These verify JSON-RPC error responses.

**Status:** All IGNORED

```
✗ test_unknown_method_returns_error — FAIL
  Reason: `tf mcp` command not implemented; error handling not wired
  
✗ test_malformed_json_rpc_returns_error — FAIL
  Reason: same
  
✗ test_invalid_json_returns_parse_error — FAIL
  Reason: same
  
✗ test_invalid_budget_set_key — FAIL
  Reason: same
```

### 6. Edge Case Tests (1 test)

```
✗ test_stdio_eof_graceful_shutdown — FAIL
  Reason: `tf mcp` command not implemented
```

## Root Cause Analysis

**Primary blocker:** The `tf mcp` subcommand does not exist.

- `crates/tf-cli/src/main.rs` has no `mcp` verb dispatch
- `crates/tf-core/src/mcp.rs` module does not exist
- `crates/tf-core/src/lib.rs` does not expose a `pub mod mcp`

**Secondary blockers (will surface after primary is fixed):**

1. Verdict-mapping adapter (pure function) not implemented
2. MCP tool handlers not implemented:
   - `tf_gate` handler
   - `tf_budget_read` handler
   - `tf_budget_set` handler
   - `tf_report` handler
   - `tf_observe` handler
   - `tf_spend` handler
   - `tf_signal` handler
   - `tf_plan_open` handler
   - `tf_plan_close` handler
   - `tf_schedule_toggle` handler
3. MCP resource serving not implemented:
   - `tf://status` resource
   - `tf://calibration` resource
   - `tf://events` resource
4. JSON-RPC error handling not implemented
5. `rmcp::Server` integration not complete

## Implementation Roadmap (STEP 5)

To move all tests from RED to GREEN:

### Phase 1: Core MCP Module Setup

1. Create `crates/tf-core/src/mcp.rs` with:
   - Verdict-mapping adapter function (pure Rust)
   - Stub tool handler functions (return unimplemented error for now)
   - Stub resource functions (return unimplemented error for now)

2. Update `crates/tf-core/src/lib.rs`:
   - Add `pub mod mcp;` (feature-gated under `mcp` feature if needed)

### Phase 2: CLI Dispatch

3. Update `crates/tf-cli/src/main.rs`:
   - Add `"mcp"` case in the verb dispatch match statement
   - Instantiate `rmcp::Server`
   - Register all 10 tools
   - Register all 3 resources
   - Start the server with `rmcp::Server::run()`

### Phase 3: Verdict Adapter Implementation

4. Implement the verdict-mapping adapter in `crates/tf-core/src/mcp.rs`:
   ```rust
   pub fn map_verdict(cli_verdict: &str, reason: &str, ceiling: &Value) -> Value {
       let mcp_verdict = match cli_verdict {
           "CONTINUE" => "allow",
           _ => "deny",
       };
       json!({
           "verdict": mcp_verdict,
           "reason": reason,
           "ceiling": ceiling
       })
   }
   ```

### Phase 4: Tool Handlers

5. Implement each tool handler:
   - Invoke the corresponding `tf-core` function
   - Apply the verdict adapter where needed (tf_gate)
   - Return JSON-RPC result or error

### Phase 5: Resource Serving

6. Implement resource handlers:
   - Read current state files
   - Serialize to JSON
   - Return to MCP client

### Phase 6: Error Handling

7. Wrap all handler calls with proper error handling:
   - File I/O errors → JSON-RPC error
   - JSON parsing errors → JSON-RPC error
   - Unknown methods → JSON-RPC error

### Phase 7: Testing and Stabilization

8. Run `cargo test --test mcp --features mcp`:
   - All unit tests should pass
   - All integration tests should pass
   - Run 3× to check for flaky tests

## Testing Execution Instructions (for STEP 5)

Once implementation starts:

```bash
# Run all MCP tests with the feature flag
cargo test --test mcp --features mcp

# Run a single test
cargo test --test mcp --features mcp test_tf_gate_tool_allow_verdict -- --nocapture

# Run with logging (if RUST_LOG is set)
RUST_LOG=debug cargo test --test mcp --features mcp -- --nocapture
```

## Expected Coverage

Once implementations are complete, the following files should have 100% coverage:

- `crates/tf-core/src/mcp.rs` (verdict adapter, tool handlers, resource handlers)
- `crates/tf-cli/src/main.rs` (MCP verb dispatch)

Verify with:

```bash
cargo llvm-cov --package tf-core --package tf-cli mcp
```

## Notes for Implementer (STEP 5)

1. **Pure functions first:** The verdict adapter is a pure function and should be implemented and tested before touching I/O.

2. **State files:** Tool handlers will need to read/write state files in the cost directory. Use existing `state::*` functions from `tf-core/src/state.rs` (or similar).

3. **Error handling:** Every I/O operation can fail. Wrap in Result types; never unwrap/expect in production code outside tests.

4. **Feature gate:** Ensure the `mcp` feature gate is applied correctly so the default build (no features) excludes rmcp/tokio.

5. **Binary size:** After implementation, verify:
   ```bash
   cargo build --release
   cargo build --release --features mcp
   # Compare binary sizes; mcp build should be <105% of default build
   ```

## Revision History

| Date | Status | Notes |
|------|--------|-------|
| 2026-06-13 | RED | Initial gap map; 19 tests ignored, 7 adapter tests skipped. All blocked on `tf mcp` subcommand. |
