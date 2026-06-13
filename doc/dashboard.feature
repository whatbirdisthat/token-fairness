# Dashboard and Telemetry Scenarios (Phase C)

Feature: Live Dashboard HTTP Server
  Scenario: Dashboard starts and serves HTML
    Given tf dashboard is invoked with no arguments
    When the HTTP server binds to port 8080
    Then the server returns HTTP 200 OK with Content-Type application/html
    And the response body contains Chart.js library tags
    And the response body contains an embedded WebSocket client

  Scenario: Dashboard serves embedded assets without external fetches
    Given tf dashboard is running on localhost:8080
    When a user navigates to http://localhost:8080
    Then the page loads completely without any external HTTP requests
    And all JavaScript and CSS are embedded in the HTML
    And the page renders three Chart.js charts visible

  Scenario: User navigates to dashboard and views initial state
    Given tf dashboard is running with a sample JSONL event file
    And the file contains 5 spend events for different models
    When a user loads http://localhost:8080 in a browser
    Then the page loads within 2 seconds
    And the first chart displays a gauge with session spend percentage
    And the second chart displays a pie chart of spend by model
    And the third chart displays a time-series trend of SAVES vs BLOWN

  Scenario: Dashboard persists on page reload
    Given tf dashboard is running with known event state
    And a user has loaded the dashboard
    When the user presses F5 to reload the page
    Then the page reloads and displays the same charts
    And the gauge, pie, and trend values are identical to before reload
    And no live event data is lost during reload

Feature: Real-Time WebSocket Broadcasting
  Scenario: WebSocket client connects and receives initial state
    Given tf dashboard is running on localhost:8080
    When a WebSocket client connects to ws://localhost:8080/ws
    Then the server accepts the connection (HTTP 101)
    And the client receives a welcome message with the current fold state
    And the state includes current spend, model counts, and guard efficacy

  Scenario: New events are broadcast to connected WebSocket clients
    Given tf dashboard is running with a WebSocket client connected
    And the honesty-events.jsonl file is being watched
    When a new spend event is appended to honesty-events.jsonl
    Then the server broadcasts the event to all connected clients within 100 ms
    And the event is formatted as JSON with type "event" and data payload
    And the client receives the event without page reload

  Scenario: Multiple WebSocket clients receive the same broadcast
    Given tf dashboard is running on localhost:8080
    When three separate WebSocket clients connect to ws://localhost:8080/ws
    And a new event is appended to honesty-events.jsonl
    Then all three clients receive the event within 100 ms
    And each client receives identical event data

  Scenario: WebSocket disconnection and reconnection
    Given a WebSocket client is connected to the dashboard
    When the client disconnects (TCP close)
    And then reconnects within 5 seconds
    Then the client receives a full state snapshot on reconnect
    And no events that arrived during disconnection are lost
    And the client continues receiving new events normally

  Scenario: WebSocket broadcast preserves event ordering
    Given tf dashboard is running with a WebSocket client
    When five events are appended to honesty-events.jsonl in rapid succession
    Then the client receives all five events in the exact order appended
    And the event timestamps are monotonically increasing

Feature: REST JSON Endpoints
  Scenario: GET /api/session-budget returns current spend state
    Given tf dashboard is running with known event state
    When a client sends GET /api/session-budget
    Then the response is HTTP 200 with Content-Type application/json
    And the response body contains:
      | field | type |
      | session_cap | integer |
      | per_fanout_cap | integer |
      | current_spend | integer |
      | ceiling_pct | number |
    And ceiling_pct is between 0 and 100

  Scenario: GET /api/spend-by-model returns model breakdown
    Given tf dashboard is running with spend events for models: opus, sonnet, haiku
    When a client sends GET /api/spend-by-model
    Then the response is HTTP 200 with Content-Type application/json
    And the response is an array of objects, each with:
      | field | example |
      | model | "claude-opus-4" |
      | tokens | 15000 |
      | count | 5 |
    And the array includes all three models
    And the sum of tokens matches the session total spend

  Scenario: GET /api/guard-efficacy returns save/blown counts
    Given tf dashboard is running with known guard verdicts
    When a client sends GET /api/guard-efficacy
    Then the response is HTTP 200 with Content-Type application/json
    And the response body contains:
      | field | type |
      | saves_count | integer |
      | blown_count | integer |
      | save_rate_pct | number |
    And save_rate_pct = saves_count / (saves_count + blown_count) * 100
    And save_rate_pct is between 0 and 100

  Scenario: GET /api/estimator-accuracy returns MAPE metrics
    Given tf dashboard is running with estimator-accuracy.jsonl entries
    When a client sends GET /api/estimator-accuracy
    Then the response is HTTP 200 with Content-Type application/json
    And the response body contains:
      | field | type |
      | mean_absolute_percentage_error | number |
      | min_error_pct | number |
      | max_error_pct | number |
    And mean_absolute_percentage_error is between 0 and 1000
    And min_error_pct <= mean_absolute_percentage_error <= max_error_pct

Feature: File Watcher and Truncation Handling
  Scenario: Dashboard watches honesty-events.jsonl from env-resolved path
    Given the TF_EVENTS_DIR environment variable is set to a custom temp directory
    And a honesty-events.jsonl file exists in that directory
    When tf dashboard starts
    Then the watcher monitors the correct file at TF_EVENTS_DIR/honesty-events.jsonl
    And the watcher is NOT monitoring a hardcoded path like /tmp/tf-events

  Scenario: Truncated event file is handled gracefully
    Given tf dashboard is running and watching honesty-events.jsonl
    And the watcher has recorded an offset of 1000 bytes
    When the file is truncated to 500 bytes (log rotation)
    Then the watcher does NOT panic or hang
    And the offset is reset to the new EOF
    And subsequent appends to the file are read normally
    And no garbage or corrupt data is broadcasted

  Scenario: Empty event file is created if missing
    Given tf dashboard is invoked with a nonexistent honesty-events.jsonl path
    When the dashboard initializes the watcher
    Then the system creates an empty honesty-events.jsonl file
    And the file is 0 bytes
    And the watcher begins monitoring it for appends

  Scenario: New appends are detected within latency SLA
    Given tf dashboard is running and watching honesty-events.jsonl
    When a 100-byte event is appended to the file
    Then the watcher detects the append within 100 ms (measured via platform-native FSEvents/inotify)
    And the event is broadcast to WebSocket clients within 100 ms of detection

Feature: Prometheus Metrics Export
  Scenario: Prometheus endpoint is disabled by default
    Given tf dashboard starts without the --prometheus flag
    When a client sends GET /metrics
    Then the server returns HTTP 404 Not Found

  Scenario: Prometheus endpoint is enabled with flag
    Given tf dashboard starts with --prometheus flag
    When a client sends GET /metrics
    Then the server returns HTTP 200 OK with Content-Type text/plain
    And the response body is valid Prometheus text format (one metric per line)
    And no external fetch or template is used to generate metrics

  Scenario: All six Prometheus metrics are present with correct types
    Given tf dashboard is running with --prometheus flag and known event state
    When a client sends GET /metrics
    Then the response includes these metrics (by name):
      | metric name | type | description |
      | tf_session_spend_tokens | gauge | current session cumulative spend |
      | tf_session_ceiling_percent | gauge | spend as % of session ceiling |
      | tf_weekly_ceiling_percent | gauge | spend as % of 7-day rolling window |
      | tf_guard_saves_total | counter | total count of SAVE verdicts (all time) |
      | tf_guard_blown_total | counter | total count of BLOWN verdicts (all time) |
      | tf_guard_procedural_denies_total | counter | total procedural denials (all time) |
    And each metric includes a HELP comment line explaining the metric

  Scenario: Prometheus metrics are updated on new events
    Given tf dashboard is running with --prometheus flag
    And the initial spend is 1000 tokens and saves_total is 5
    When a new spend event is appended to honesty-events.jsonl for 500 tokens with a SAVE verdict
    Then a subsequent GET /metrics shows:
      | metric | new_value | expected_change |
      | tf_session_spend_tokens | 1500 | +500 |
      | tf_guard_saves_total | 6 | +1 (monotonic) |

  Scenario: Gauge metrics decrease on session boundary, counters never decrease
    Given tf dashboard is running with --prometheus flag
    And session_spend is 2000 tokens and saves_total is 10
    When a session boundary is crossed (e.g., new day)
    Then a subsequent GET /metrics shows:
      | metric | behavior |
      | tf_session_spend_tokens | resets to 0 (gauge, can decrease) |
      | tf_guard_saves_total | remains 10 (counter, monotonic increase only) |

Feature: Fold-Parity Invariant (Critical AC#9)
  Scenario: JavaScript fold matches Rust fold on synthetic data
    Given a fixed sequence of 50 synthetic JSONL events (spend, saves, blown, MAPE data)
    When the sequence is fed into observe.rs:fold_events() (Rust)
    And the same sequence is fed into the embedded JavaScript fold function (assets/dashboard.html)
    Then the final fold state is identical in both implementations:
      | field | must match exactly |
      | session_spend | cumulative tokens |
      | saves_count | count of SAVE verdicts in period |
      | blown_count | count of BLOWN verdicts in period |
      | mean_absolute_percentage_error | MAPE value to 2 decimal places |
    And the test repeats for 10 random synthetic sequences without divergence

  Scenario: Fold correctly deduplicates spend to latest cumulative per session
    Given JSONL events with cumulative spend: session A [100, 200], session B [50]
    When the fold is computed
    Then the final session_spend for session A is 200 (not 300, not 100)
    And session_spend for session B is 50
    And the fold result is stable across multiple replays (idempotent)

  Scenario: Fold correctly bins SAVES/BLOWN by time period
    Given 10 SAVE events and 5 BLOWN events spread across two time periods (00:00–01:00, 01:00–02:00)
    When the fold is computed
    Then the fold output includes a time-series array with two period entries:
      | period | saves | blown |
      | hour_1 | 7 | 2 |
      | hour_2 | 3 | 3 |

Feature: Binary Size & Feature-Gating
  Scenario: Default build excludes dashboard dependencies
    Given the workspace Cargo.toml defines [features] dashboard = ["axum", "tokio", "notify"]
    When cargo build --release is executed (default, no features)
    Then the compiled tf binary does NOT contain axum, tokio, or notify symbols
    And the binary size is within 105% of the pre-change baseline

  Scenario: Dashboard feature adds expected dependencies
    Given the dashboard feature is not enabled
    When cargo build --release --features dashboard is executed
    Then the compiled tf binary includes axum, tokio, and notify symbols
    And the dashboard subcommand is available (tf --help lists "dashboard")

  Scenario: MCP and Dashboard features can coexist
    Given both [features] mcp and [features] dashboard are defined
    When cargo build --release --features mcp --features dashboard is executed
    Then the compilation succeeds
    And both `tf mcp` and `tf dashboard` subcommands are available
    And the binary size is reasonable (not combining into excessively large binary)

Feature: CLI Integration
  Scenario: tf --help lists dashboard subcommand
    Given the dashboard feature is enabled or compiled with all features
    When tf --help is executed
    Then the help text includes a line for the dashboard subcommand
    And tf --help | grep dashboard shows the correct command name

  Scenario: tf dashboard --help shows usage
    Given the dashboard command is available
    When tf dashboard --help is executed
    Then the output includes:
      | element |
      | usage/synopsis |
      | optional flags (e.g., --prometheus) |
      | port 8080 (default binding) |
      | example: tf dashboard --prometheus |

  Scenario: tf dashboard with --prometheus flag enables metrics
    Given the dashboard feature is compiled
    When tf dashboard --prometheus is executed
    Then the server binds to port 8080
    And GET /metrics returns Prometheus text format
    And all six metrics (PROM-002, PROM-003) are present

Feature: Error Handling & Robustness
  Scenario: Server gracefully handles malformed JSON from WebSocket
    Given a WebSocket client is connected
    When the client sends invalid JSON (unparseable)
    Then the server does NOT panic
    And the server either closes the connection or sends an error message
    And other clients remain connected

  Scenario: REST endpoint handles missing event files gracefully
    Given tf dashboard is running but honesty-events.jsonl has been deleted
    When a client sends GET /api/session-budget
    Then the server returns HTTP 200 with a zero-filled response:
      | field | value |
      | current_spend | 0 |
      | ceiling_pct | 0 |
      | saves_count | 0 |
    And the server does NOT panic

  Scenario: Dashboard recovers from file-watcher errors
    Given tf dashboard is watching honesty-events.jsonl
    When the file is moved to a different directory
    Then the watcher handles the error gracefully
    And the server continues accepting HTTP requests
    And the server either re-watches the new location or logs the error without crashing
