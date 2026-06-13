# ADR-004: Chart Rendering Strategy

## Status
Accepted

## Context
The dashboard (ADR-002) needs charts: a budget gauge, spend-by-model breakdown, SAVES-vs-BLOWN guard efficacy over time, and estimator accuracy (MAPE). The EARS spec requires that *"WHEN a user navigates to `localhost:8080`, THE SYSTEM SHALL serve an HTML page with embedded Chart.js dashboard (no server-side templating)"* and that the live view update without page reload (ADR-003's WebSocket stream).

Separately, the tool already produces **static reports** via `tf observe --period month --write …` (backed by `crates/tf-core/src/observe.rs`), which emit git-committable markdown with Mermaid diagrams for documentation and audit. These two consumers have opposite requirements: the live dashboard must be interactive and browser-driven; the static report must render with no browser at all (it lives in markdown in the repo).

The question: server-side rendering, client-side rendering, or both?

## Decision
**Hybrid.** The live dashboard renders **client-side with Chart.js** (interactive, real-time, updates via WebSocket without page reload), served from a compile-time-embedded static HTML page. **Static reports** continue to render **server-side** in the existing Pressroom/markdown+Mermaid form for committed, browser-free documentation.

## Rationale
- **Live dashboard demands client-side rendering.** The defining requirement is real-time updates without reload. Chart.js consumes the WebSocket event stream (ADR-003) and the REST snapshots, and redraws in place. Server-side SVG would have to regenerate and re-push the entire page on every event — the wrong tool for an interactive, streaming view.
- **No server-side templating, by spec and by design.** The HTML page is static and embedded at compile time via `include_str!`. The server ships bytes; the browser does the rendering from data fetched over REST/WebSocket. This keeps the server trivial (no template engine, no render loop), keeps the page in the binary (consistent with ADR-002's self-contained model), and satisfies the EARS "no server-side templating" requirement directly.
- **Static reports demand server-side, browser-free rendering.** A git-committed monthly report must be readable in a diff, on GitHub, in any markdown viewer — with no JavaScript runtime. Mermaid-in-markdown (the existing `observe.rs` Pressroom output) is exactly right and already shipped. Forcing it into Chart.js would make committed docs depend on a browser to render, which defeats their purpose.
- **The split is a clean separation of *rendering* concerns.** Live telemetry/budget UI and historical/audit reports are different products with different lifecycles, and each uses the rendering technology that fits. They do, however, share a *data-transformation* contract (the event-fold/aggregation semantics) that must stay consistent across the live and static paths — see the Consequences section, which makes that coupling explicit rather than denying it.

The payoff: an interactive live dashboard that updates in real time, and durable committed reports that render anywhere — each optimal for its consumer, with the existing report path unchanged.

## Consequences

**What this makes easy:**
- Dashboard page is `assets/dashboard.html` with embedded Chart.js, served via `include_str!` (compiled into the `dashboard`-feature binary). No build-time asset pipeline, no separate static-file deployment.
- REST endpoints feed initial/snapshot data: `GET /api/status`, `GET /api/events`, `GET /api/spend`, `GET /api/calibration`. These are JSON projections of the existing telemetry (`report.rs`, `spend.rs`, `calibrate.rs`, `state.rs`). The WebSocket (`/ws`, ADR-003) supplies live event deltas in the raw JSONL schema (**not** aggregates).
- Dashboard charts consume both REST snapshots and WS deltas, applying the **same aggregation semantics as `observe.rs:fold_events()`** to produce spend-by-model (deduped to the latest cumulative reading per session), SAVES-vs-BLOWN (bucketed by period), and estimator MAPE (mean per period). **The client-side JS fold is load-bearing**: because the WebSocket carries raw JSONL events and the charts need aggregates, the browser must replicate the `fold_events()` dedup/binning logic in JavaScript. Its logic must match `fold_events()` exactly, verified by a test that replays a fixed event sequence through both the Rust `fold_events()` and the JS fold and asserts identical output (acceptance criterion 9: 100% test coverage includes this fold-parity invariant test).
- Dashboard charts map cleanly to data: budget gauge (radial) from session/ceiling state, spend-by-model (pie) from the folded `spend` aggregate, SAVES-vs-BLOWN (time series) from folded `honesty-events`, estimator MAPE (line) from folded `estimator-accuracy`.
- Static reports are **unchanged**: `tf observe --period month --write doc/honesty/` still emits markdown + Mermaid via `observe.rs`. Zero regression risk to the existing audit path.
- A client reconnecting after a dropped WebSocket re-hydrates from the REST endpoints (getting the latest aggregates), then resumes the live stream (applying deltas correctly) — the mitigation referenced in ADR-003.

**What this makes harder / what we give up:**
- **The client-side aggregation must stay in sync with the server.** The JS fold (spend dedup to "latest cumulative per session", period binning of SAVES/BLOWN, MAPE mean per period) is **not** a rendering detail; it is a shared data-transformation contract with `observe.rs:fold_events()` (the dedup rationale lives at `observe.rs:~201`: the Stop hook re-emits cumulative spend every turn, so only the last event per session counts). Any change to `fold_events()` must be mirrored in the browser JS, and vice-versa. This coupling is **explicit and tested** (the fold-parity test above), not hidden. The original claim that the paths "share no rendering code, so there is no coupling to break" was wrong: there is a real coupling, and it is in the data fold, not the rendering.
  - **Failure mode this guards against:** if the JS fold diverged from the Rust fold, live charts (built from WS deltas through the JS fold) would disagree with reloaded charts (built from REST aggregates through the Rust fold). The HITL test "reload → charts persist" could still pass (both sides use REST on reload) while *live* updates silently drifted. The fold-parity invariant test is what closes that gap.
- **Two rendering technologies in the codebase.** Chart.js (browser) and Mermaid/Pressroom (server). Each must be maintained; they serve disjoint consumers and share no *rendering* code (the additive cost is real), but they are not fully decoupled — they share the fold/aggregation contract described above.
- **The live dashboard requires JavaScript in the browser.** It will not render in a no-JS environment. Accepted: the live dashboard is inherently an interactive browser tool; the no-JS audience is served by the static reports.
- **Larger initial page load for the dashboard** (Chart.js payload). Bounded — it loads once per session and is embedded, not fetched from a CDN.
- **Two schema/aggregation surfaces over the same telemetry.** REST endpoints serve aggregates; the WebSocket serves raw JSONL deltas; the browser reconciles them through the JS fold. They must stay schema- and semantics-consistent, enforced by the fold-parity test, not by hope.

## Alternatives Considered
- **A-only) Server-side SVG (Pressroom-style) for everything** — rejected for the live dashboard. Pixel-perfect and JS-free, but static: every data change forces full regeneration and a page reload, which cannot deliver the real-time, no-reload experience the EARS spec requires. Retained, correctly, for static reports — where its browser-free output is the whole point.
- **B-only) Client-side Chart.js for everything** — rejected for static reports. Interactive and live for the dashboard (where it is chosen), but it would make git-committed reports depend on a JavaScript runtime to render, breaking their use in diffs, on GitHub, and in plain markdown viewers. Wrong tool for durable documentation.
- **Single unified renderer (either direction)** — rejected. No single technology satisfies both "real-time interactive in a browser" and "renders with no browser in a committed file." The hybrid is not indecision; it is matching each renderer to a genuinely different consumer.

## References
- `doc/ROADMAP.md` § [1] — EARS: `WHEN user navigates to localhost:8080 … embedded Chart.js dashboard (no server-side templating)`; chart list in acceptance/HITL plan; acceptance criterion 9 (100% test coverage, incl. the fold-parity invariant)
- `crates/tf-core/src/observe.rs` — `fold_events()` (and the cumulative-per-session spend dedup at ~line 201) — the aggregation contract the client-side JS fold must match exactly
- ADR-002 (Dashboard Architecture) — the embedded HTTP server hosting `assets/dashboard.html` and the REST endpoints
- ADR-003 (Telemetry Pipeline) — the WebSocket stream (raw JSONL deltas) and reconnect/re-hydrate flow
- Telemetry sources for the REST projections: `crates/tf-core/src/{report,spend,calibrate}.rs`, `state.rs`
