//! CLI dispatch for `tf dashboard` command.
//!
//! Parses dashboard-specific arguments and starts the HTTP server.

use tf_core::Out;

/// Arguments for the dashboard command.
#[derive(Debug, Default)]
pub struct DashboardArgs {
    pub prometheus: bool,
}

impl DashboardArgs {
    /// Parse dashboard CLI arguments.
    pub fn from_argv(argv: &[String]) -> (Self, Out) {
        let mut args = DashboardArgs::default();
        let mut i = 0;

        while i < argv.len() {
            match argv[i].as_str() {
                "--prometheus" => {
                    args.prometheus = true;
                    i += 1;
                }
                "--help" | "-h" => {
                    return (
                        args,
                        Out::ok(
                            "tf dashboard — live HTTP dashboard with Chart.js charts and \
                             WebSocket real-time streaming\n\n\
                             Usage: tf dashboard [OPTIONS]\n\n\
                             Options:\n  \
                             --prometheus         Enable Prometheus metrics export at GET /metrics\n  \
                             --help               Show this help message\n\n\
                             The dashboard server binds to 0.0.0.0:8080 (all interfaces).\n",
                        ),
                    );
                }
                _ => {
                    i += 1;
                }
            }
        }

        (args, Out::ok(""))
    }
}

/// Run the dashboard server.
/// Returns Out with status and exit code.
pub fn run(args: DashboardArgs) -> Out {
    println!("Dashboard running on 0.0.0.0:8080 (all interfaces)");
    if args.prometheus {
        println!("Prometheus metrics enabled at GET /metrics");
    }

    // Start the async runtime and run the dashboard server.
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => return Out::err(format!("failed to create tokio runtime: {}", e), 1),
    };

    if let Err(e) = rt.block_on(start_server(args.prometheus)) {
        return Out::err(format!("dashboard server error: {}", e), 1);
    }

    // Server exited normally (e.g., Ctrl+C).
    Out::ok("")
}

/// Capacity of the per-connection broadcast channel. A client that lags more than this many
/// unread lines is dropped (its `recv()` yields `Lagged`); the watcher and other clients are
/// never blocked. Best-effort delivery, per ADR-003 — no replay for disconnected clients.
const BROADCAST_CAPACITY: usize = 1024;

/// Poll interval for the events-journal watcher. The <1s push SLA (AC#6) is met with margin: a
/// new line surfaces within one interval (~250ms) plus channel/socket latency. This is the poll
/// fallback sanctioned by the task spec — event-driven `notify` would be ~10-100ms but adds a
/// non-tokio callback thread to bridge; a short async-sleep poll reuses the truncation-safe
/// `telemetry::read_new_bytes` primitive, never busy-loops (the sleep yields the executor), and is
/// deterministic for tests.
const WATCH_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// Spawn the single events-journal watcher task and return the broadcast sender that `/ws`
/// connections subscribe to.
///
/// The watcher resolves the journal path via [`tf_core::observe::events_path`] (honouring the
/// `I2P_HONESTY_EVENTS` / `I2P_COST_STATE_DIR` overrides — critical for test isolation), starts at
/// current EOF (best-effort, no replay), then polls for newly-appended bytes. Each *complete*
/// JSONL line (terminated by `\n`) is broadcast; a partial trailing line is buffered until its
/// newline arrives, so a half-written line is never emitted.
fn spawn_event_broadcaster() -> tokio::sync::broadcast::Sender<String> {
    let (tx, _rx) = tokio::sync::broadcast::channel::<String>(BROADCAST_CAPACITY);
    let watcher_tx = tx.clone();

    tokio::spawn(async move {
        let path = tf_core::observe::events_path();
        // Start at EOF. If the file can't be initialized yet, start at offset 0 and let the first
        // successful read pick it up — never panic in the watcher.
        let mut offset = match tf_core::telemetry::init_watcher(&path) {
            Ok((_p, off)) => off,
            Err(_) => 0,
        };
        // Holds bytes of an as-yet-unterminated trailing line across poll iterations.
        let mut pending = String::new();

        loop {
            // Stop the watcher once every subscriber (and the server) has gone away.
            if watcher_tx.receiver_count() == 0 && watcher_tx.strong_count() <= 1 {
                // No active receivers; keep looping cheaply — a future `/ws` connect re-subscribes
                // off the same sender. We still sleep so this is not a busy-loop.
                tokio::time::sleep(WATCH_POLL).await;
                continue;
            }

            match tf_core::telemetry::read_new_bytes(&path, &mut offset) {
                Ok(bytes) if !bytes.is_empty() => {
                    // Lossily decode: the journal is UTF-8 JSONL, but never panic on a stray byte.
                    pending.push_str(&String::from_utf8_lossy(&bytes));
                    // Emit every complete line; retain the unterminated tail in `pending`.
                    while let Some(nl) = pending.find('\n') {
                        let line: String = pending.drain(..=nl).collect();
                        let trimmed = line.trim_end_matches(['\r', '\n']);
                        if !trimmed.is_empty() {
                            // A send error only means there are no live receivers right now; that's
                            // fine — drop the line (best-effort) and keep watching.
                            let _ = watcher_tx.send(trimmed.to_string());
                        }
                    }
                }
                Ok(_) => {}
                // Transient IO error (e.g. file briefly absent): skip this tick, try again.
                Err(_) => {}
            }

            tokio::time::sleep(WATCH_POLL).await;
        }
    });

    tx
}

/// `GET /ws` — upgrade to a WebSocket and forward each newly-appended journal line to the client.
///
/// Uses axum 0.7's [`axum::extract::ws::WebSocketUpgrade`]: `on_upgrade` hands us an owned
/// [`axum::extract::ws::WebSocket`], which we drive as a `Sink<Message>`. Each broadcast line is
/// sent as a `Message::Text`. A client that lags past the channel capacity is dropped gracefully
/// (the `Lagged` arm just resyncs); a send failure (dead socket) ends the loop. No `unwrap`.
async fn ws_handler(
    ws: axum::extract::ws::WebSocketUpgrade,
    axum::extract::State(tx): axum::extract::State<tokio::sync::broadcast::Sender<String>>,
) -> axum::response::Response {
    // Subscribe BEFORE the upgrade completes so no line appended after this point is missed.
    let rx = tx.subscribe();
    ws.on_upgrade(move |socket| handle_ws_socket(socket, rx))
}

/// Per-connection pump: forward broadcast lines to one WebSocket until the socket dies.
async fn handle_ws_socket(
    mut socket: axum::extract::ws::WebSocket,
    mut rx: tokio::sync::broadcast::Receiver<String>,
) {
    use axum::extract::ws::Message;
    use tokio::sync::broadcast::error::RecvError;

    loop {
        match rx.recv().await {
            Ok(line) => {
                if socket.send(Message::Text(line)).await.is_err() {
                    // Client went away — stop pumping this connection.
                    break;
                }
            }
            // This client fell behind the channel capacity: skip the dropped lines (best-effort,
            // no replay) and keep serving the freshest ones rather than crashing or blocking.
            Err(RecvError::Lagged(_)) => continue,
            // Sender dropped (server shutting down) — nothing more will arrive.
            Err(RecvError::Closed) => break,
        }
    }
}

/// Root handler: serve the embedded dashboard HTML.
///
/// Returns the page wrapped in [`axum::response::Html`] so the response carries
/// `Content-Type: text/html` — without the wrapper axum defaults a `&str` body
/// to `text/plain`, and the browser renders the markup as literal text.
async fn root_handler() -> axum::response::Html<&'static str> {
    axum::response::Html(tf_core::dashboard::DASHBOARD_HTML)
}

/// Build the fully-wired axum router, binding the broadcast-sender state.
///
/// Factored out of [`start_server`] so tests can mount the exact same routes (including `/ws`)
/// on an ephemeral port. The router is parameterised by the broadcast-sender state that `/ws`
/// needs; the stateless `/api/*` and `/metrics` handlers are generic over the state type, so they
/// compose freely. `.with_state(..)` at the end binds the state and erases it back to `Router<()>`.
fn build_router(
    enable_prometheus: bool,
    broadcast_tx: tokio::sync::broadcast::Sender<String>,
) -> axum::Router {
    use axum::routing::get;
    use axum::Router;

    let mut router: Router<tokio::sync::broadcast::Sender<String>> = Router::new()
        .route("/", get(root_handler))
        .route("/ws", get(ws_handler))
        .route(
            "/api/session-budget",
            get(|| async {
                match tf_core::dashboard::endpoint_session_budget() {
                    Ok(json) => axum::Json(json),
                    Err(e) => axum::Json(serde_json::json!({"error": e.to_string()})),
                }
            }),
        )
        .route(
            "/api/spend-by-model",
            get(|| async {
                match tf_core::dashboard::endpoint_spend_by_model() {
                    Ok(json) => axum::Json(json),
                    Err(e) => axum::Json(serde_json::json!({"error": e.to_string()})),
                }
            }),
        )
        .route(
            "/api/guard-efficacy",
            get(|| async {
                match tf_core::dashboard::endpoint_guard_efficacy() {
                    Ok(json) => axum::Json(json),
                    Err(e) => axum::Json(serde_json::json!({"error": e.to_string()})),
                }
            }),
        )
        .route(
            "/api/estimator-accuracy",
            get(|| async {
                match tf_core::dashboard::endpoint_estimator_accuracy() {
                    Ok(json) => axum::Json(json),
                    Err(e) => axum::Json(serde_json::json!({"error": e.to_string()})),
                }
            }),
        );

    // Conditionally add Prometheus metrics endpoint
    if enable_prometheus {
        router = router.route(
            "/metrics",
            get(|| async {
                match tf_core::dashboard::compute_fold() {
                    Ok(state) => {
                        let budget_path = format!("{}/budget.json", tf_core::state::state_dir());
                        let budget_json = tf_core::state::read_json(&budget_path)
                            .unwrap_or(serde_json::json!({}));
                        let metrics =
                            tf_core::dashboard::PrometheusMetrics::from_fold(&state, &budget_json);
                        metrics.to_string()
                    }
                    Err(e) => format!("# ERROR: {}\n", e),
                }
            }),
        );
    }

    // Bind the broadcast-sender state, finalising to `Router<()>`.
    router.with_state(broadcast_tx)
}

/// Start the axum HTTP server on 0.0.0.0:8080.
async fn start_server(enable_prometheus: bool) -> Result<(), String> {
    // Start the single events-journal watcher and get the broadcast sender that `/ws`
    // connections subscribe to. One watcher fans out to all clients.
    let broadcast_tx = spawn_event_broadcaster();
    let router = build_router(enable_prometheus, broadcast_tx);

    // Bind and listen on all interfaces (0.0.0.0) so remote clients can connect
    let addr = "0.0.0.0:8080"
        .parse::<std::net::SocketAddr>()
        .map_err(|e| e.to_string())?;

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| e.to_string())?;

    axum::serve(listener, router)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_prometheus_flag() {
        let argv = vec!["--prometheus".to_string()];
        let (args, _out) = DashboardArgs::from_argv(&argv);
        assert!(args.prometheus);
    }

    #[test]
    fn test_parse_help_flag() {
        let argv = vec!["--help".to_string()];
        let (_args, out) = DashboardArgs::from_argv(&argv);
        assert_eq!(out.code, 0);
        assert!(out.stdout.contains("dashboard"));
    }

    #[test]
    fn test_parse_no_args() {
        let argv = vec![];
        let (args, _out) = DashboardArgs::from_argv(&argv);
        assert!(!args.prometheus);
    }

    /// The root handler must serve the embedded page as `text/html` so browsers
    /// render it as a webpage (and Chart.js executes) rather than as literal text.
    #[tokio::test]
    async fn test_root_handler_content_type_is_html() {
        use axum::response::IntoResponse;

        let response = root_handler().await.into_response();
        let content_type = response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .expect("root response must carry a Content-Type header")
            .to_str()
            .expect("Content-Type must be valid UTF-8");

        assert!(
            content_type.starts_with("text/html"),
            "expected text/html, got {content_type}"
        );
    }

    /// The root handler must serve the actual embedded dashboard markup.
    #[tokio::test]
    async fn test_root_handler_serves_dashboard_html() {
        use axum::body::to_bytes;
        use axum::response::IntoResponse;

        let response = root_handler().await.into_response();
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body must be readable");
        let body = String::from_utf8(bytes.to_vec()).expect("body must be UTF-8");

        assert!(
            body.contains("<!DOCTYPE html>"),
            "body must be the HTML page"
        );
        assert_eq!(body, tf_core::dashboard::DASHBOARD_HTML);
    }

    // ====================================================================
    // /ws live-push integration tests (AC#6)
    // ====================================================================
    //
    // These tests set the process-global `I2P_HONESTY_EVENTS` override to a per-test temp file so
    // the watcher reads an isolated journal. That env var is process-wide, so we serialize the
    // env-touching tests behind a local lock (tf-core's `ENV_LOCK` is `pub(crate)` and not visible
    // here). The guard is held across the env set AND the server spawn (the watcher task resolves
    // `events_path()` asynchronously after spawn) and across the test's `.await` points, so it must
    // be an async-aware `tokio::sync::Mutex` — a `std::sync::Mutex` guard held across `.await`
    // trips clippy's `await_holding_lock` (rightly: it would block the runtime thread).
    static WS_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    use futures_util::StreamExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::Message as TMessage;

    /// AC#6 SLA ceiling. The watcher polls at 250ms; a generous-but-bounded 1s wait pins the
    /// "within 1 second" acceptance criterion without ever using a bare sleep as the assertion —
    /// the test FAILS if the line does not arrive before this bound elapses.
    const SLA: Duration = Duration::from_secs(1);

    /// A unique temp events-journal path for one test.
    fn temp_events_path(tag: &str) -> std::path::PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "tf-ws-test-{}-{}-{}.jsonl",
            tag,
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ))
    }

    /// Spawn the watcher + a full router on an ephemeral 127.0.0.1 port; return the bound address.
    /// The caller must already hold `WS_ENV_LOCK` and have set `I2P_HONESTY_EVENTS`.
    async fn spawn_test_server() -> std::net::SocketAddr {
        let broadcast_tx = spawn_event_broadcaster();
        let router = build_router(false, broadcast_tx);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        addr
    }

    /// Connect a WebSocket client to the test server's `/ws`.
    async fn connect_ws(
        addr: std::net::SocketAddr,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        let url = format!("ws://{}/ws", addr);
        let (stream, _resp) = tokio_tungstenite::connect_async(url)
            .await
            .expect("ws handshake should return 101 and connect");
        stream
    }

    /// Append a line (with trailing newline) to the journal file.
    fn append_line(path: &std::path::Path, line: &str) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open journal for append");
        writeln!(f, "{}", line).expect("append line");
    }

    /// Wait (bounded by the SLA) for the next text message on the socket; fail if it doesn't arrive.
    async fn recv_text_within_sla(
        stream: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> String {
        loop {
            let msg = tokio::time::timeout(SLA, stream.next())
                .await
                .expect("a line must arrive within the 1s SLA")
                .expect("stream must yield a message")
                .expect("message must not be an error");
            match msg {
                TMessage::Text(t) => return t,
                // Ignore protocol frames (ping/pong) that aren't our payload.
                _ => continue,
            }
        }
    }

    /// AC#6: a line appended to the journal reaches a connected client within the SLA, verbatim.
    #[tokio::test]
    async fn ws_pushes_new_line_within_sla() {
        let _g = WS_ENV_LOCK.lock().await;
        let journal = temp_events_path("single");
        std::env::set_var("I2P_HONESTY_EVENTS", &journal);

        let addr = spawn_test_server().await;
        let mut client = connect_ws(addr).await;
        // Give the watcher a beat to record EOF before the first append, so the line is seen as new.
        // (Bounded readiness wait, not an assertion sleep.)
        tokio::time::sleep(Duration::from_millis(50)).await;

        let line = r#"{"ts":1000,"kind":"blown","reason":"paid lockout"}"#;
        append_line(&journal, line);

        let got = recv_text_within_sla(&mut client).await;
        assert_eq!(got, line, "client must receive the exact appended line");

        std::env::remove_var("I2P_HONESTY_EVENTS");
        let _ = std::fs::remove_file(&journal);
    }

    /// Two clients connected simultaneously both receive the same appended line (fan-out).
    #[tokio::test]
    async fn ws_fans_out_to_two_clients() {
        let _g = WS_ENV_LOCK.lock().await;
        let journal = temp_events_path("fanout");
        std::env::set_var("I2P_HONESTY_EVENTS", &journal);

        let addr = spawn_test_server().await;
        let mut a = connect_ws(addr).await;
        let mut b = connect_ws(addr).await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        let line = r#"{"ts":2000,"kind":"gate","class":"save","est":120000}"#;
        append_line(&journal, line);

        let got_a = recv_text_within_sla(&mut a).await;
        let got_b = recv_text_within_sla(&mut b).await;
        assert_eq!(got_a, line, "client A must receive the line");
        assert_eq!(got_b, line, "client B must receive the line");

        std::env::remove_var("I2P_HONESTY_EVENTS");
        let _ = std::fs::remove_file(&journal);
    }

    /// A malformed / non-JSON line must NOT crash the broadcaster: it is forwarded verbatim (the
    /// `/ws` pump is content-agnostic — the browser re-fetches aggregates, which tolerate garbage),
    /// and a subsequent good line still arrives. Pins "best-effort, panic-free" robustness.
    #[tokio::test]
    async fn ws_malformed_line_does_not_crash_broadcaster() {
        let _g = WS_ENV_LOCK.lock().await;
        let journal = temp_events_path("malformed");
        std::env::set_var("I2P_HONESTY_EVENTS", &journal);

        let addr = spawn_test_server().await;
        let mut client = connect_ws(addr).await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        let garbage = "this is not json {{{";
        append_line(&journal, garbage);
        let got_garbage = recv_text_within_sla(&mut client).await;
        assert_eq!(got_garbage, garbage, "garbage line is forwarded verbatim");

        // The broadcaster is still alive: a good line that follows still arrives.
        let good = r#"{"ts":3000,"kind":"blown","reason":"5h-window exhausted"}"#;
        append_line(&journal, good);
        let got_good = recv_text_within_sla(&mut client).await;
        assert_eq!(got_good, good, "broadcaster survives malformed input");

        std::env::remove_var("I2P_HONESTY_EVENTS");
        let _ = std::fs::remove_file(&journal);
    }

    /// A partial (newline-less) trailing write must NOT be emitted until its newline arrives —
    /// the broadcaster never sends a half-written line.
    #[tokio::test]
    async fn ws_does_not_emit_partial_trailing_line() {
        use std::io::Write;
        let _g = WS_ENV_LOCK.lock().await;
        let journal = temp_events_path("partial");
        std::env::set_var("I2P_HONESTY_EVENTS", &journal);

        let addr = spawn_test_server().await;
        let mut client = connect_ws(addr).await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Write a partial line WITHOUT a trailing newline.
        {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&journal)
                .expect("open journal");
            write!(f, r#"{{"ts":4000,"kind":"blown""#).expect("partial write");
        }
        // It must NOT be delivered within the SLA (no complete line yet).
        let early = tokio::time::timeout(SLA, client.next()).await;
        assert!(
            early.is_err(),
            "partial line must not be emitted before its newline"
        );

        // Now complete the line; the WHOLE line must arrive.
        append_line(&journal, r#",reason":"paid lockout"}"#);
        let got = recv_text_within_sla(&mut client).await;
        assert_eq!(
            got, r#"{"ts":4000,"kind":"blown",reason":"paid lockout"}"#,
            "the completed line is emitted whole, once"
        );

        std::env::remove_var("I2P_HONESTY_EVENTS");
        let _ = std::fs::remove_file(&journal);
    }
}
