Feature: MCP Server stdio transport and tools

  Background:
    Given a Claude Code session with tf mcp registered as an MCP server
    And the tf binary built with --features mcp

  # tf_gate tool — Happy path
  Scenario: Agent invokes tf_gate tool to check budget headroom
    Given a ceiling payload { "used_pct": 80, "headroom": 15 }
    When the agent calls tool tf_gate with the ceiling payload
    Then the MCP response is valid JSON-RPC 2.0 result
    And the result contains { "verdict": "allow", "reason": "...", "ceiling": {...} }
    And the reason field is a non-empty string
    And the ceiling field contains the original ceiling object

  Scenario: Gate denies when spend + headroom exceeds 100%
    Given a ceiling payload { "used_pct": 90, "headroom": 15 }
    When the agent calls tool tf_gate with the ceiling payload
    Then the result contains { "verdict": "deny" }
    And the reason field contains "ceiling" or "headroom"

  Scenario: Gate denies when no live signal is available
    Given the signals registry has { "gate": "ERROR" }
    And a ceiling payload { "used_pct": 50, "headroom": 20 }
    When the agent calls tool tf_gate with the ceiling payload
    Then the result contains { "verdict": "deny" }
    And the reason field contains "signal" or "live"

  # tf_budget_read tool — Happy path
  Scenario: Agent reads current budget state
    Given tf_budget_read tool invoked
    Then the MCP response is valid JSON with fields:
      | session_cap | integer | >= 1000 |
      | per_fanout_cap | integer | >= 100 |
      | current_spend | integer | >= 0 |
      | fanout_spend | integer | >= 0 |

  Scenario: Budget state persists across multiple tool calls
    Given the initial budget state { session_cap: 50000, current_spend: 10000 }
    When the agent calls tf_budget_read
    Then current_spend is 10000
    When the agent calls tf_budget_read again
    Then current_spend is still 10000 (not reset)

  # tf_budget_set tool — Happy path
  Scenario: Agent updates session budget ceiling
    Given the current session_cap is 50000
    When the agent calls tf_budget_set with { key: "session_cap", value: 60000 }
    Then the result contains { "success": true, "key": "session_cap", "new_value": 60000 }
    When tf_budget_read is called
    Then session_cap is now 60000

  Scenario: Agent updates per_fanout_cap
    Given the current per_fanout_cap is 5000
    When the agent calls tf_budget_set with { key: "per_fanout_cap", value: 7500 }
    Then the result contains { "success": true, "key": "per_fanout_cap", "new_value": 7500 }

  # tf_report tool — Happy path
  Scenario: Agent requests a daily spend report
    Given tf_report tool invoked with { window: "day" }
    Then the MCP response is valid JSON with fields:
      | window | string | "day" |
      | window_open | integer | > 0 |
      | window_close | integer | > window_open |
      | spend_total | integer | >= 0 |
      | gate_denials | integer | >= 0 |

  Scenario: Agent requests monthly report
    When tf_report is invoked with { window: "month" }
    Then the response contains window: "month"
    And window_open and window_close span ~30 days

  Scenario: Agent requests report without window (defaults to day)
    When tf_report is invoked with no window parameter
    Then the response contains window: "day"

  # tf_observe tool — Happy path
  Scenario: Agent observes deduplicated spend events
    Given spend events recorded for span IDs [s1, s2, s1, s3] (s1 appears twice)
    When tf_observe is invoked with { window: "day" }
    Then the result is an array of 3 distinct span objects:
      | span_id | s1 |
      | span_id | s2 |
      | span_id | s3 |
    And each has fields { span_id, cost_tokens, model, role }
    And the array is sorted chronologically by first observation

  # tf_spend tool — Happy path
  Scenario: Agent records a new spend event
    When the agent calls tf_spend with { span_id: "abc123", cost: 500, model: "claude-opus-4", role: "researcher" }
    Then the result contains { "success": true, "span_id": "abc123", "cost": 500 }
    When tf_observe is called next
    Then the new event appears in the observation array

  # tf_signal tool — Happy path
  Scenario: Agent sets a signal state
    When the agent calls tf_signal with { name: "gate", status: "OK" }
    Then the result contains { "success": true, "signal": "gate", "status": "OK" }
    When tf_gate is called next
    Then the gate uses the OK signal (not ERROR)

  Scenario: Agent sets error signal
    When the agent calls tf_signal with { name: "observability", status: "ERROR" }
    Then the result contains { "success": true, "signal": "observability", "status": "ERROR" }

  # tf_plan_open and tf_plan_close — Happy path
  Scenario: Agent opens a new budget plan
    When the agent calls tf_plan_open with { title: "Q3 research budget", budget_tokens: 100000 }
    Then the result contains { "success": true, "plan_id": "..." (non-empty), "title": "Q3 research budget", "budget_tokens": 100000 }

  Scenario: Agent closes a plan
    Given an open plan with plan_id "plan-123"
    When the agent calls tf_plan_close with { plan_id: "plan-123" }
    Then the result contains { "success": true, "plan_id": "plan-123", "closed_at": ... (unix seconds) }

  # tf_schedule_toggle — Happy path
  Scenario: Agent enables window-aware schedule gate
    When the agent calls tf_schedule_toggle with { enabled: true }
    Then the result contains { "success": true, "enabled": true }

  Scenario: Agent disables window-aware schedule gate
    When the agent calls tf_schedule_toggle with { enabled: false }
    Then the result contains { "success": true, "enabled": false }

  # Resource contracts — Happy path
  Scenario: Agent requests tf://status resource
    When the agent requests resource tf://status
    Then the response is valid JSON with fields:
      | session_budget | object | contains session_cap, per_fanout_cap, current_spend |
      | signals | object | contains gate, budget, observability status |
      | timestamp | integer | unix seconds, > 0 |

  Scenario: Agent requests tf://calibration resource
    When the agent requests resource tf://calibration
    Then the response is valid JSON with fields:
      | rolling_windows | array | non-empty |
      | current_window | string | one of hour/day/month/ytd |
      | next_boundary | integer | unix seconds |

  Scenario: Agent requests tf://events resource
    When the agent requests resource tf://events
    Then the response is a JSON array
    And each element has { timestamp, kind, details }
    And kind ∈ { "spend", "signal", "plan_open", "plan_close", "gate_verdict" }
    And array length is <= 100 (recent events only)

  # Abuse / error paths
  Scenario: Unknown MCP tool invoked
    When the agent calls tool tf_unknown_tool
    Then the server returns a JSON-RPC 2.0 error
    And error.code is -32601 (method not found)
    And error.message contains "method not found" or "unknown"

  Scenario: Malformed JSON-RPC request (missing params)
    When the agent sends { "jsonrpc": "2.0", "method": "tf_gate" } (no params, no id)
    Then the server returns a JSON-RPC 2.0 error
    And error.code is -32600 (invalid request) or similar

  Scenario: Invalid JSON sent to stdin
    When the agent sends "{ this is not json }"
    Then the server returns a JSON-RPC 2.0 parse error
    And error.code is -32700 (parse error)

  Scenario: Tool handler reads missing state file
    Given the session budget file is deleted
    When the agent calls tf_budget_read
    Then the server returns a JSON-RPC error with code != 200
    And error.message indicates "file not found" or "io error"

  Scenario: tf_budget_set with invalid key
    When the agent calls tf_budget_set with { key: "invalid_key", value: 1000 }
    Then the server returns a JSON-RPC error
    And error.message contains "invalid key" or "unknown key"

  Scenario: tf_budget_set with invalid value type
    When the agent calls tf_budget_set with { key: "session_cap", value: "not-a-number" }
    Then the server returns a JSON-RPC error
    And error.message contains "invalid" or "type"

  # Edge cases and boundary conditions
  Scenario: Gateway with exactly zero headroom
    Given a ceiling payload { "used_pct": 100, "headroom": 0 }
    When the agent calls tf_gate
    Then the verdict is "deny" (no room to continue)

  Scenario: Gateway with negative headroom (should fail gracefully)
    Given a ceiling payload { "used_pct": 50, "headroom": -10 }
    When the agent calls tf_gate
    Then either verdict is "deny" or the server returns an error (graceful failure)

  Scenario: Multiple concurrent tool calls (simulated)
    When the agent calls tf_budget_read, tf_spend, tf_budget_read in rapid succession
    Then all three calls complete and return consistent state
    And no data corruption or race conditions observed

  Scenario: Server gracefully handles EOF on stdin
    Given the MCP server running
    When the parent process closes stdin
    Then the server terminates cleanly (exit code 0)
    And no panic or stack trace emitted

  # Verdict mapping invariants
  Scenario: Verdict adapter always translates CONTINUE to allow
    Given the scheduler returns { "verdict": "CONTINUE", ... }
    When tf_gate processes the scheduler verdict
    Then the MCP tool response contains verdict: "allow"

  Scenario: Verdict adapter always translates non-CONTINUE to deny
    Given the scheduler returns { "verdict": "HALT"|"DEFER"|"ASK"|"NO_SIGNAL" }
    When tf_gate processes the scheduler verdict
    Then the MCP tool response contains verdict: "deny"

  Scenario: Reason field is never omitted in adapter output
    When tf_gate processes any scheduler verdict
    Then the MCP response always contains a reason field
    And reason is a non-empty string
    And reason is never null or missing

  Scenario: Ceiling field is always present in adapter output
    When tf_gate processes any scheduler verdict
    Then the MCP response always contains a ceiling field
    And ceiling contains the original ceiling object from the scheduler
    And ceiling is never null or missing
