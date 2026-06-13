# ADR-003: Telemetry Pipeline

## Status
Accepted

## Context
The dashboard (ADR-002) must update in real time. Roadmap item [1] specifies: *"WHEN a JSONL telemetry file is appended to, THE SYSTEM SHALL broadcast the new lines to connected WebSocket clients in real time"* and *"WHILE the `tf dashboard` command is running, THE SYSTEM SHALL watch the honesty-events.jsonl file for appends and forward them to subscribers."* Acceptance criterion 6 sets the bar: WebSocket clients receive new lines within 1 second.

The existing telemetry is append-only JSONL written by `tf-core` (`honesty-events.jsonl`, `estimator-accuracy.jsonl`, plus `calibration.json` / `session.json` as point-state). This append-only contract is load-bearing: it is how the tool records honesty events and estimator accuracy, and it must not be disturbed.

The question: how do new events get from a file append to a browser chart, fast, without coupling the scheduler's core logic to the presentation layer?

## Decision
**File-watching + WebSocket, best-effort.** A new module `crates/tf-core/src/telemetry.rs` watches for appends to the events journal (path resolved through `observe::events_path()`, honoring the `I2P_HONESTY_EVENTS` full-path override and the `I2P_COST_STATE_DIR` directory override) via the `notify` crate (inotify on Linux, FSEvents on macOS), parses new lines, and broadcasts them to connected WebSocket clients at `ws://localhost:8080/ws`. Delivery is best-effort: no buffering or replay for clients that are disconnected when an event arrives.

## Rationale
- **It respects the append-only JSONL contract.** The watcher observes files that `tf-core` already writes; the writers are untouched. The telemetry pipeline is a pure reader. This keeps the source of truth (the JSONL on disk) authoritative and the broadcast a derived, disposable view.
- **It decouples telemetry from core logic.** The alternative — instrumenting the scheduler/budget code to emit WebSocket events at runtime (option D) — would weld presentation into `scheduler.rs`/`budget.rs`, make those modules untestable without a socket, and break the modular design. File-watching keeps the boundary clean: core writes files, the watcher reads them, the server broadcasts. Each layer is independently testable.
- **It meets the latency bar without polling cost.** inotify/FSEvents deliver append notifications in roughly 10–100ms — comfortably inside the 1-second EARS budget — with no busy-loop CPU. Polling (option C) would either miss the latency target or burn CPU re-reading files; file-watching gets low latency for free.
- **Best-effort matches the tool's philosophy and the use case.** This is a development dashboard, not an audit ledger. The durable record already exists: the JSONL files. A client that reconnects re-reads current state via the REST endpoints (ADR-004) and resumes the live stream. Buffering/replaying events for absent clients would add a queue and persistence layer for no real benefit — "honesty over perfectionism."
- **Single-machine means no message bus.** The events, the files, and the dashboard are all on one host. A broker (Kafka/RabbitMQ) solves multi-machine durability problems this tool does not have.

The payoff: real-time charts within ~100ms of an event, the core scheduler untouched and still unit-testable, and zero new infrastructure beyond one cross-platform crate.

## Consequences

**What this makes easy:**
- New module `crates/tf-core/src/telemetry.rs`: a file watcher and a WebSocket broadcaster. The dashboard server (ADR-002) wires it to the `/ws` route.
- Clients receive `{ts, kind, …}` JSON events with the *same schema as the JSONL files* — no transform, no second schema to keep in sync. A new event kind in the files flows to the dashboard automatically.
- The pipeline is testable without a browser: append a line to a temp-dir JSONL, assert the watcher emits the parsed event; connect a test WebSocket client, assert it receives the broadcast. The existing temp-dir/`ENV_LOCK` test conventions apply directly.

**Path resolution — the watcher must use the same helpers as the writers (critical for acceptance criterion 6):**
- The watcher's target path is resolved through `observe::events_path()` (and the `state::state_dir()` it builds on), **not a hardcoded literal**. This is the same resolution the writers use, so the watcher and the writers always operate on the exact same file. `observe::events_path()` (`crates/tf-core/src/observe.rs:27`) returns `I2P_HONESTY_EVENTS` verbatim when set, otherwise `"{state_dir}/honesty-events.jsonl"`; `state::state_dir()` (`crates/tf-core/src/state.rs:54`) returns `I2P_COST_STATE_DIR` when set, otherwise the default `~/.claude/state/i2p-cost`.
- This is load-bearing for correctness, not a nicety. The test suite overrides `I2P_HONESTY_EVENTS` / `I2P_COST_STATE_DIR` to point at temp dirs (see e.g. `crates/tf-core/src/budget.rs`, `spend.rs`, `snapshot.rs`, `observe.rs` tests). A watcher that watched a literal `honesty-events.jsonl` path would, in those tests, watch a *different* file than the one being written — the broadcast would silently emit nothing and AC#6 would fail. The same silent failure would occur in any real deployment that sets either env override. Resolving through the helpers makes the watcher and writers agree in every environment.

**Robustness to truncation / rotation:**
- The watcher tracks a read offset into the events journal so it only emits newly appended lines. If the watched file's size **decreases below the tracked read offset** (indicating truncation or replacement — e.g. log rotation, or a manual `: > file`), the watcher detects the shrink and resets its offset to the new EOF (or start-of-file) rather than seeking past the new end. This prevents the two silent failure modes a naive offset-tracker has: reading garbage from a stale offset, or emitting nothing forever after the file is cleared. It ensures robustness across log rotation and manual file clears.

**What this makes harder / what we give up:**
- **`notify` is a new (feature-gated) dependency.** It lives under the `dashboard` feature (ADR-002), so the hook binary does not pay for it. Cross-platform file-watching has platform-specific behaviors (FSEvents coalescing, inotify watch limits) the watcher must tolerate.
- **No persistence on disconnect.** A client offline between event T and reconnect misses events in that window. Mitigated by re-reading current state from REST on reconnect; not mitigated for the missed individual events. Accepted for a dev dashboard.
- **Single-machine only.** The watcher sees local files. Cross-host telemetry is out of scope here (use the Prometheus/Grafana route from ADR-002).
- **Append parsing must be robust to partial writes.** A reader observing a file mid-append must not choke on a half-written line; the watcher reads complete lines and tolerates a trailing partial until the next notification. (Truncation/rotation, a distinct failure mode, is handled by the offset-reset rule above.)

## Alternatives Considered
- **B) Message queue (Kafka/RabbitMQ)** — rejected. Durable and multi-subscriber, but a heavyweight external service that solves multi-machine durability — a problem a single-machine budget guard does not have. Massive operational overkill.
- **C) Polling** — rejected. Removes the `notify` dependency, but cannot hit the 1-second latency target without frequent re-reads that waste CPU, and re-reading a growing JSONL repeatedly scales badly. File-watching gives lower latency at lower cost.
- **D) Direct instrumentation of `tf-core`** — rejected as architecturally corrosive. Emitting WebSocket events from inside `scheduler.rs`/`budget.rs` couples core logic to the presentation transport, makes the core untestable without a socket, and violates the modular boundary that lets the hook binary exclude the dashboard entirely. The whole point of feature-gating the dashboard collapses if the core emits its events.

## References
- `doc/ROADMAP.md` § [1] — EARS event-driven (`WHEN JSONL appended … broadcast`) and state-driven (`WHILE dashboard running … watch honesty-events.jsonl`) requirements; acceptance criterion 6 (within 1s)
- `crates/tf-core/src/observe.rs:27` (`events_path()`) and `crates/tf-core/src/state.rs:54` (`state_dir()`) — the env-aware path helpers the watcher resolves through (`I2P_HONESTY_EVENTS`, `I2P_COST_STATE_DIR`)
- ADR-002 (Dashboard Architecture) — the server that hosts `/ws` and the `dashboard` feature gate
- ADR-004 (Chart Rendering) — REST endpoints clients re-read on reconnect
- Existing JSONL writers: `crates/tf-core/src/{report,spend,calibrate}.rs`, `state.rs` — the append-only sources the watcher reads (via the same helpers)
- `notify` crate — cross-platform inotify/FSEvents file-watching
