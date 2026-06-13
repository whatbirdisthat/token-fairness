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
                             The dashboard server binds to 127.0.0.1:8080 by default.\n",
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
    println!("Dashboard running on 127.0.0.1:8080");
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

/// Start the axum HTTP server on 127.0.0.1:8080.
async fn start_server(enable_prometheus: bool) -> Result<(), String> {
    use axum::routing::get;
    use axum::Router;

    // Build the router with all endpoints
    let mut router = Router::new()
        .route("/", get(|| async { tf_core::dashboard::DASHBOARD_HTML }))
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
                        let budget_path =
                            format!("{}/budget.json", tf_core::state::state_dir());
                        let budget_json =
                            tf_core::state::read_json(&budget_path)
                                .unwrap_or(serde_json::json!({}));
                        let metrics = tf_core::dashboard::PrometheusMetrics::from_fold(
                            &state,
                            &budget_json,
                        );
                        metrics.to_string()
                    }
                    Err(e) => format!("# ERROR: {}\n", e),
                }
            }),
        );
    }

    // Bind and listen
    let addr = "127.0.0.1:8080"
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
}
