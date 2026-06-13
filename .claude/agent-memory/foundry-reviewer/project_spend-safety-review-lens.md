---
name: project_spend-safety-review-lens
description: How to adversarially review token-fairness spend-safety changes (fail-closed guard model)
metadata:
  type: project
---

The tf project's whole purpose is a fail-closed token-spend guard. Reviews here turn on one
question (from doc/design/spend-safety-enforcement.md): *can an ungoverned/expensive/hidden
fan-out still happen?*

**Why:** advisory safety is theatre; the design contract is "every unknown resolves to
HALT/ASK, never silently CONTINUE." The author repeatedly red-teams their own work, so the
real review risk is subtle SAFE-direction holes, not missing guards.

**How to apply when reviewing spend changes:**
- The guard layering is intentional, not muda: CORE-A session cap (whole session, billable
  tokens), ledger `budget_total` via `tf ledger spend` (one off-peak job), and the +Xk arm
  (per-fan-out consent). Three concepts, three scopes — confirm they don't *contradict*, don't
  flag them as duplicative.
- L1 (live rate-limit gate) is empirically BLIND inside Workflows (no `.rate_limits`); the
  signal-INDEPENDENT cap (budget arm + preflight-spend) is the primary guard. Don't demand L1.
- Recalibration that *narrows* what counts toward a cap (e.g. excluding cache_read tokens) is
  the dangerous direction — always argue whether a genuinely expensive run can now slip under.
  cache_read is ~$0.10/M so the USD risk is real but bounded; the residual gap is the
  rate-limit *window* (token-count based), which billable-exclusion no longer protects.
- State files (session.json, budget.json, ledger json) are written via state::write_json →
  fs::write, world-readable (0644, no explicit mode). They hold token counts + session_id, no
  secrets — not a finding unless something sensitive lands there.
- session-tokens.sh sanitizes sid via `tr -c 'A-Za-z0-9._-' '_'` before using it in a path;
  jq uses --arg/--argjson; awk END emits one line → heredoc parse safe. No shell-injection.
