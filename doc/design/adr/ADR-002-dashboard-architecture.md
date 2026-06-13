# ADR-002: Dashboard Architecture

## Status
Accepted

## Context
Roadmap item [1] requires a live dashboard showing budget gauges, spend by model, guard efficacy (SAVES vs BLOWN), and estimator accuracy. The user story: *"AS A developer I WANT a live dashboard of token spend and guard behavior SO THAT I can monitor and debug scheduler decisions in real time."* A secondary story asks for Prometheus export so users with existing Grafana can integrate.

Constraints:
- `tf` is a single-developer, single-machine budget guard. The telemetry it visualizes (`honesty-events.jsonl`, `estimator-accuracy.jsonl`, `calibration.json`, `session.json`) is local to the machine running Claude Code.
- The EARS spec mandates a dashboard on HTTP (default port 8080) *and* says the system *"SHALL NOT increase the hook binary size significantly (feature-gate heavy dependencies like axum, tokio)."* These two requirements are in tension: the dashboard needs a web server, but the hook binary must stay lean.
- Docker is available (29.5.3) but no Grafana/Prometheus is installed locally. The binary is self-contained and distributed via lazy-download of GitHub Release assets — adding a mandatory external service would break that "just a binary" deployment story.

The decision: where do the live charts come from?

## Decision
An **embedded HTTP server** (`axum` on `tokio`), launched via a new `tf dashboard` subcommand, is the primary dashboard — feature-gated under a `dashboard` Cargo feature so the hook binary never pays for it. An **optional Prometheus exporter** at `GET /metrics` (behind `--prometheus`) lets users who already run Grafana integrate without making Grafana a dependency.

## Rationale
- **Embedded server is immediate ROI for a single-machine tool.** A developer runs `tf dashboard`, opens `localhost:8080`, and sees live charts with no service to install, no config to write, no Docker to orchestrate. For a single-developer budget guard, that is the highest-leverage path to the "monitor in real time" user story.
- **It works offline and ships in the binary.** The HTML/JS is embedded at compile time (`include_str!`); there is no asset-serving deployment, no CDN, no external service. This preserves the "self-contained binary, lazy-downloaded" distribution model the project already commits to.
- **Feature-gating resolves the binary-size tension.** `axum`, `tokio`, and `notify` live under `[features] dashboard`. The default hook build excludes them, keeping the binary within the EARS budget (acceptance criterion 8: ≤105% of pre-change size). The dashboard build opts into the weight only when the user wants a dashboard.
- **Prometheus export buys Grafana without owning it.** Users with an existing monitoring stack get `GET /metrics` in Prometheus text format and a documented `docker-compose.grafana.yml`. This is opt-in complexity: it costs nothing for the common case and unlocks the enterprise case. We do not make Grafana primary, because that would impose external-service overhead on a tool whose whole value is being a frictionless local binary.

The payoff: the 90% case (one developer, one machine, instant local dashboard) is trivial, and the 10% case (existing Grafana) is a documented opt-in — without forking into two maintained primary UIs.

## Consequences

**What this makes easy:**
- `tf dashboard` is a new CLI subcommand, distinct from `tf mcp` (ADR-001). It serves the dashboard HTML, REST endpoints, and WebSocket on port 8080. Satisfies acceptance criteria 5–6, 10.
- The HTTP server reads the existing JSONL telemetry — no new collection pipeline, no schema change. Charts are projections of files `tf-core` already writes (`report.rs`, `observe.rs`, `spend.rs`, `calibrate.rs`).
- `GET /metrics` (when `--prometheus` is set) emits Prometheus text format; Grafana integration is documented via `doc/design/adr/docker-compose.grafana.yml`. Satisfies acceptance criterion 7 and the `WHERE --prometheus` EARS requirement.

**Prometheus metric set and types (acceptance criterion 7):**
- The Prometheus exporter at `GET /metrics` emits the following metrics, with types chosen for semantic correctness (not convenience):
  * `tf_session_spend_tokens` (**gauge**): cumulative billable tokens for the current session (resets to 0 on the session boundary).
  * `tf_session_ceiling_percent` (**gauge**): current 5-hour rolling-window utilization (0–100%).
  * `tf_weekly_ceiling_percent` (**gauge**): current 7-day rolling-window utilization (0–100%).
  * `tf_guard_saves_total` (**counter**): cumulative SAVE events since process start (monotonic).
  * `tf_guard_blown_total` (**counter**): cumulative BLOWN events since process start (monotonic).
  * `tf_guard_procedural_denies_total` (**counter**): cumulative procedural-deny events since process start (monotonic).
- **Spend and ceiling metrics are GAUGES, not counters.** This is forced by the cumulative-per-session semantics of `crates/tf-core/src/observe.rs` (the `spend` rollup, ~line 201, is deduped to "only the LAST cumulative reading per session" because the Stop hook re-emits cumulative spend every turn). A naive exporter that published cumulative session spend as a Prometheus *counter* would emit a value that **decreases** at the session boundary; Prometheus interprets a decreasing counter as a counter reset, which corrupts `rate()` / `increase()` queries in Grafana. Modeling spend as a gauge is the only correct choice for a value that legitimately falls when the session rolls over.
- **Counters are reserved for genuinely monotonic event tallies** — SAVES, BLOWN, and procedural denies, which only ever increase within a process lifetime. This split (gauges for resettable cumulative state, counters for monotonic tallies) is what keeps Grafana `rate()` queries valid across session resets and is part of acceptance criterion 7's contract.

**What this makes harder / what we give up:**
- **We own the web server and the file-watching.** Bringing axum/tokio/notify in-tree means we maintain an HTTP server and cross-platform file-watching (ADR-003) ourselves, rather than delegating to Grafana. Accepted: the maintenance is bounded and the alternative imposes an external service on every user.
- **Single-machine only.** The embedded dashboard visualizes local files. Multi-machine aggregation is explicitly out of scope for the embedded path; users who need it use the Prometheus → Grafana route.
- **Best-effort telemetry, no persistence.** The broadcast layer has no message queue; a client that disconnects misses events while away (see ADR-003). Acceptable for a development dashboard.
- **Two render-time codepaths to keep coherent.** The Prometheus exporter and the REST/WebSocket dashboard read the same telemetry but format it differently. We accept the second codepath because it is small (a text serializer over the metric set enumerated above) and strictly opt-in. The gauge-vs-counter mapping above is the contract both this exporter and any Grafana dashboard depend on.

## Alternatives Considered
- **B) Grafana as primary** — rejected. Professional and multi-machine-capable, but requires an external service (none installed locally) and a configuration burden that is pure overhead for a single-developer tool. It inverts the project's "just a binary" ethos. Demoting Grafana to an opt-in integration captures its value without its cost.
- **A-only (embedded, no Prometheus)** — rejected as too narrow. It would strand users who already run Grafana and explicitly asked for export. The Prometheus exporter is cheap (a text endpoint over existing data) and satisfies a stated user story, so excluding it leaves value on the table.
- **C) Full hybrid with two co-equal primary UIs** — rejected as over-built. Maintaining two first-class dashboards doubles support surface. The chosen design keeps the embedded dashboard primary and Prometheus as a thin export, getting hybrid flexibility without hybrid maintenance.

## References
- `doc/ROADMAP.md` § [1] — EARS: HTTP dashboard on port 8080; `SHALL NOT increase hook binary size`; `WHERE --prometheus`; acceptance criteria 5–8, 10
- `crates/tf-core/src/observe.rs` (~line 201) — the cumulative-per-session spend dedup that forces spend/ceiling metrics to be gauges, not counters
- ADR-001 (MCP Transport) — `tf dashboard` is a separate process/surface from `tf mcp`
- ADR-003 (Telemetry Pipeline) — the file-watch + WebSocket layer feeding this server
- ADR-004 (Chart Rendering) — how this server's charts are drawn
- `doc/design/adr/docker-compose.grafana.yml` — Grafana datasource for the Prometheus opt-in
- `Cargo.toml` release profile + `[features] dashboard` — the binary-size guarantee
