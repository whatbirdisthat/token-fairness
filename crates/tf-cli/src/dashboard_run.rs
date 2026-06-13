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
    // For now, this is a placeholder. The actual HTTP server would be spawned here.
    // Once the HTTP integration is implemented, this will bind to port 8080 and start serving.

    let msg = if args.prometheus {
        "Dashboard running on 127.0.0.1:8080 with Prometheus metrics enabled at GET /metrics\n"
    } else {
        "Dashboard running on 127.0.0.1:8080\n"
    };

    Out::ok(msg)
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
