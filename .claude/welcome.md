# Welcome to token-fairness 👋

The standalone home of `tf` — the token-aware scheduler that protects your usage
meter from a paid lockout. This is a Rust workspace shipping one compiled binary.

**Live dashboard:** start it, then open **http://localhost:8088**

```
tf dashboard --port 8088     # in this repo: plugins/scheduler/bin/tf-x86_64-linux dashboard --port 8088
```

> ⚠️ Port **8080 is taken by cadvisor** on this machine — opening `:8080` shows cadvisor,
> not this dashboard. Use `--port 8088` (or any free port; `$TF_DASHBOARD_PORT` also works).
> The token-fairness page title is **"Token Fairness — Live Dashboard"** (Chart.js + live `/ws`).

## Lanes

- **Watch the live dashboard** — real-time budget, spend-by-model, guard efficacy, rolling-window lockout risk
- **Check usage & budget now** — one-shot status without a server
- **Develop the `tf` binary** — build, test, the dashboard feature gate

## Watch the live dashboard

- **See the real-time dashboard** → `tf dashboard --port 8088`, then open http://localhost:8088
- **Add Prometheus metrics** → `tf dashboard --port 8088 --prometheus` → scrape `GET /metrics`
- **It shows cadvisor instead** → you opened `:8080`; cadvisor squats there. Re-run with `--port 8088` and open http://localhost:8088
- **Pick a different port** → `tf dashboard --port <N>` or `TF_DASHBOARD_PORT=<N> tf dashboard`

## Check usage & budget now

- **Current rolling-window risk** → `tf budget status`
- **Estimator accuracy / job report** → `tf report . --estimator`
- **What would a fan-out cost?** → `tf plan --class <small|medium|large|epic> --now $(date +%s)`

## Develop the `tf` binary

- **Build the hook binary (lean, no dashboard)** → `cargo build --release -p tf-cli`
- **Build with the dashboard + MCP surface** → `cargo build --release -p tf-cli --features mcp,dashboard`
- **Run the tests** → `cargo test --workspace --features tf-cli/mcp,tf-cli/dashboard`
- **Refresh the local distributed binary** → copy `target/release/tf` → `plugins/scheduler/bin/tf-x86_64-linux`

Light is green, trap is clean. 🟢
