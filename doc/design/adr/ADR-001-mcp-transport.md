# ADR-001: MCP Transport Choice

## Status
Accepted

## Context
Roadmap item [1] requires exposing token-scheduler operations (`tf_gate`, `tf_budget_read`, `tf_budget_set`, `tf_report`, `tf_observe`, `tf_spend`) as MCP tools so Claude Code agents can invoke them programmatically rather than shelling out to the CLI. The driving user story is: *"AS A Claude Code agent I WANT to call `tf gate` as an MCP tool SO THAT I can check budget headroom before spawning a fan-out."*

The constraints that bound this decision:
- The `tf` binary runs on hot Claude Code hooks. The release profile (`opt-level = "z"`, `lto = true`, `strip = true`, `panic = "abort"`) exists to keep the binary small and fast. Any transport that drags in a TLS stack or always-on HTTP server violates that contract.
- The EARS spec is explicit: *"The system SHALL expose token-scheduler operations as MCP tools over stdio transport (zero external dependencies)."*
- The consumer is Claude Code, which manages MCP servers as subprocesses registered in settings.json. There is exactly one Claude session per server instance.

The transport choice determines the process model, the dependency surface, and whether the MCP server is co-located with the dashboard/metrics surfaces (ADR-002, ADR-003).

## Decision
The MCP server uses **stdio transport** — JSON-RPC 2.0 over stdin/stdout, the standard MCP wire protocol — implemented via the `rmcp` crate, invoked as `tf mcp`.

## Rationale
Stdio wins on every axis that matters for this tool:

- **Zero external dependencies.** Stdio needs no listening socket, no port allocation, no TLS, no bind-address security review. It is the path of least binary-size growth, which directly protects the hook-binary contract in the release profile. SSE and gRPC both require an HTTP server in the MCP process; gRPC additionally requires a TLS story for any non-loopback use.
- **It is the standard MCP transport.** Claude Code supports stdio servers out of the box via settings.json registration. The agent-facing user story is satisfied immediately with no bespoke client. SSE-over-HTTP and HTTP/gRPC are non-standard or secondary MCP transports — choosing them means betting on a less-supported path for the primary consumer.
- **Single-instance lifecycle is free.** Claude Code spawns `tf mcp` as a subprocess and owns its lifecycle (start, stdin/stdout pipes, termination). There is exactly one instance per session, so there are no concurrency, port-collision, or multi-client coordination concerns to design around.
- **Clean separation from observability.** Stdio forces the MCP surface to be distinct from the dashboard (ADR-002) and telemetry (ADR-003) surfaces. The MCP server answers tool calls; the dashboard serves humans. Keeping them separate means the heavy HTTP/WebSocket dependencies (axum, tokio, notify) never load into the MCP/hook process and can be feature-gated away.

The payoff: an agent gets a working `tf_gate` tool with no new infrastructure, no network configuration, and no measurable hook-binary cost.

## Consequences

**What this makes easy:**
- `tf mcp` runs as a Claude-managed subprocess. Tool calls and responses flow as JSON-RPC 2.0 over stdin/stdout. No port, no firewall, no TLS.
- Registration is a one-time settings.json entry pointing at the `tf` binary with the `mcp` subcommand.
- MCP tool handlers are thin wrappers over existing `tf-core` functions (`crates/tf-core/src/scheduler.rs`, `budget.rs`, `report.rs`, `observe.rs`, `spend.rs`). The handlers invoke the same core logic the CLI invokes; persistence and read semantics for `tf_budget_read` / `tf_budget_set` / `tf_report` / `tf_observe` / `tf_spend` are therefore identical to the CLI, satisfying acceptance criteria 3–4.

**Verdict-mapping adapter (the `tf_gate` tool — acceptance criterion 2):**
- The CLI gate does **not** emit the verdict vocabulary the MCP tool contract requires. The scheduler's verdict literals are `CONTINUE | HALT | DEFER | ASK | NO_SIGNAL` (see `crates/tf-core/src/scheduler.rs`, e.g. the L1-blind branch around lines 442–496 that selects `"HALT" | "DEFER" | "ASK"` and the `CONTINUE` branch). Acceptance criterion 2 requires the MCP tool to return `{"verdict":"allow"|"deny","reason":"…","ceiling":{…}}`. These are **not** the same vocabulary; the MCP tool must **translate**, not pass through.
- The MCP tool layer therefore provides an explicit **contract adapter** over the scheduler's verdict JSON. The adapter is owned by the MCP tool wrapper, not by the CLI / `scheduler.rs`, which keep their native vocabulary. The mapping is:
  - `CONTINUE` → `allow`
  - `HALT`, `DEFER`, `ASK`, `NO_SIGNAL` → `deny` (every non-`CONTINUE` verdict denies; the gate's whole purpose is to refuse to CONTINUE when there is no headroom or no live signal)
  - `reason` — always present in the adapter output, never omitted. When the scheduler JSON carries a reason (e.g. `"no-live-signal"` on the blind-L1 branch), it is forwarded; otherwise the adapter synthesizes a reason string from the source verdict so the field is never null/missing.
  - `ceiling` — always present, taken from the `ceiling` object the scheduler already embeds in its verdict JSON (the `ceil_json` field on every gate branch).
- This adapter is a small, pure transformation (scheduler verdict JSON → MCP tool JSON) and is unit-testable in isolation: feed each of the five source verdicts, assert the mapped `verdict`/`reason`/`ceiling`. It is the load-bearing piece that makes AC#2 true, and it must not be described as a verbatim contract reuse.
- The MCP server is testable in-process: feed a JSON-RPC request to a handler, assert on the JSON-RPC response, with `tf-core` operating against a temp-dir state. No subprocess, no socket needed for unit tests.

**Dependency surface — `rmcp` and the `tokio` leak (protects acceptance criterion 8):**
- The MCP server depends on `rmcp` (the official Rust MCP SDK, `modelcontextprotocol/rust-sdk`). The version targeted and tested is **`rmcp` 0.2.x** (the current published line as of this decision); the exact pinned version must be recorded in `Cargo.toml` and updated here when bumped. `rmcp` **does** have `tokio` as a transitive dependency: its runtime and all of its transports — including the stdio transport this ADR selects — are built on the `tokio` async runtime. There is no `tokio`-free build of `rmcp`.
- Because `rmcp` pulls in `tokio`, the MCP tools **must themselves be feature-gated** so they never leak `tokio` into the pure hook build. The MCP tools are gated under a dedicated `mcp` Cargo feature (which may be unified with the `dashboard` feature, since that feature already carries `tokio` for axum — see ADR-002). The default hook build enables **neither** `mcp` nor `dashboard`, so neither `rmcp` nor `tokio` is compiled into it.
- A **binary-size test for the MCP-without-dashboard build** (`cargo build --release --features mcp`) must pass within the AC#8 budget (≤105% of the pre-change hook-binary size is the hook-build target; the `mcp`/`dashboard` builds are separately budgeted opt-in artifacts and must not regress the default build). The CI gate asserts that the **default** build excludes `rmcp`/`tokio` entirely, which is what keeps the hook-binary contract intact.

**What this makes harder / what we give up:**
- **No HTTP endpoint on the MCP surface.** Grafana cannot scrape the MCP server. That capability lives entirely in the dashboard/metrics surface (ADR-002), reached via a separate `tf dashboard` process. This is intentional separation, not an oversight.
- **No concurrent external clients.** Only the parent Claude session can talk to a given `tf mcp` instance. A second consumer needs its own subprocess. For a per-session budget guard this is the correct model, not a limitation.
- **Requires settings.json registration.** Unlike a discoverable HTTP endpoint, stdio servers must be declared to Claude Code. This is a documented one-line setup step, paid once.
- **The MCP build is not `tokio`-free.** Choosing `rmcp` means the `mcp`/`dashboard` builds carry an async runtime. This is acceptable because those builds are opt-in and the default hook build excludes them; it is *not* acceptable to leave the feature gate off, which is why the binary-size test above is mandatory.

## Alternatives Considered
- **B) SSE over HTTP** — rejected. Provides a real-time event channel scrapeable by Grafana, but adds an HTTP server to the MCP process, is not the standard MCP transport, and duplicates the real-time concern that the dashboard/WebSocket surface (ADR-003) already owns. The event-streaming need is met there, not on the MCP transport.
- **C) HTTP/gRPC** — rejected. Modern and multi-client-capable, but the heaviest option: it requires a TLS story for any production/non-loopback use, is not the standard MCP transport, and its concurrency benefits are irrelevant to a single-session-per-instance budget guard. Pure complexity with no payoff for this consumer.

## References
- `doc/ROADMAP.md` § [1] — EARS: *"SHALL expose token-scheduler operations as MCP tools over stdio transport (zero external dependencies)"*; acceptance criteria 1–4, 8
- `crates/tf-core/src/scheduler.rs` — the native gate verdict vocabulary (`CONTINUE | HALT | DEFER | ASK | NO_SIGNAL`) that the MCP `tf_gate` adapter translates to `allow`/`deny`
- ADR-002 (Dashboard Architecture) — the separate HTTP surface the MCP server intentionally does not provide; the `dashboard`/`tokio` feature gate the `mcp` feature may be unified with
- ADR-003 (Telemetry Pipeline) — where the real-time event channel lives
- `rmcp` 0.2.x — official Rust MCP SDK; depends transitively on `tokio` (Implementation Notes, ROADMAP item [1])
- `Cargo.toml` release profile + `[features] mcp` / `dashboard` — the binary-size contract the feature gate protects
