//! tf-core — the deterministic arithmetic of the token-fairness scheduler.
//!
//! A faithful 1:1 port of the bash token-aware scheduler (born in the idea-to-production
//! CONCIERGE plugin). Every module reproduces one bash script's observable contract:
//! the same CLI verbs, the same one-line JSON, the same exit codes, the same state files.
//! All *parsing* uses serde_json; all *output* is hand-formatted (see [`fmt`]) so the
//! conformance vectors captured from the bash pass bit-for-bit.

pub mod budget;
pub mod calibrate;
pub mod ceiling;
#[cfg(feature = "dashboard")]
pub mod dashboard;
pub mod ensemble;
pub mod estimate;
pub mod fmt;
pub mod ledger;
#[cfg(feature = "mcp")]
pub mod mcp;
pub mod observe;
pub mod offpeak;
pub mod registry;
pub mod report;
pub mod routing;
pub mod scheduler;
pub mod signal;
pub mod snapshot;
pub mod spend;
pub mod state;
#[cfg(feature = "dashboard")]
pub mod telemetry;
pub mod windows;

/// Test-only support shared across modules. The `tf` modules resolve state paths from process
/// environment variables (`I2P_*`); a process-global `Mutex` serializes the tests that override
/// them so the parallel test runner can't have one test's `set_var` clobber another's. No external
/// dependency — just a guard the env-touching tests lock before mutating the environment.
#[cfg(test)]
pub(crate) mod testutil {
    use std::sync::Mutex;
    /// Lock before any `std::env::set_var`/`remove_var` of a shared `I2P_*` var in a test.
    pub static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// A unique temp directory for a test, created fresh. Caller removes it when done.
    pub fn temp_dir(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "tf-test-{}-{}-{}",
            tag,
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}

/// The uniform result of a CLI verb: what to print where, and the process exit code.
/// Modelled on the bash scripts, which write a JSON line to stdout (or an advisory
/// message to stderr) and exit with a contract-defined code.
#[derive(Debug, Default, Clone)]
pub struct Out {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

impl Out {
    /// stdout line, exit 0.
    pub fn ok(s: impl Into<String>) -> Out {
        Out {
            stdout: s.into(),
            stderr: String::new(),
            code: 0,
        }
    }
    /// stdout line, explicit exit code.
    pub fn line(s: impl Into<String>, code: i32) -> Out {
        Out {
            stdout: s.into(),
            stderr: String::new(),
            code,
        }
    }
    /// stderr message, explicit exit code.
    pub fn err(msg: impl Into<String>, code: i32) -> Out {
        Out {
            stdout: String::new(),
            stderr: msg.into(),
            code,
        }
    }
}
