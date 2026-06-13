---
name: budget-baseline-unit-migration
description: 0.1.1 budget cap reads billable_tokens but baseline_tokens may have been captured in full .tokens units under 0.1.0, collapsing spent_since to 0 across upgrade
metadata:
  type: project
---

In PR #3 (Issue #2 fixes, branch fix/workflow-fanout-cap-bypass), `budget::session_tokens()`
was changed to prefer `session.json.billable_tokens` (cache-read-excluded) over `.tokens`.

The hazard: `spent_since(baseline) = max(0, session_tokens() - baseline_tokens)`. `baseline_tokens`
is only written by `tf budget set --reset`, which captures the *then-current* `session_tokens()`.
A user who ran `--reset` under 0.1.0 has a baseline stored in FULL-token units (large, cache-read
inclusive). After upgrading to 0.1.1, `session_tokens()` returns the much smaller billable count, so
`billable - full_baseline` is negative → clamps to 0 → the cumulative session cap silently resets to
full headroom until the user re-runs `tf budget set --reset`.

**Why:** the two reads are different units; only the migration window is affected (fresh install has
baseline=0, which is correct).
**How to apply:** bounded, not unbounded — a single fan-out is still capped by per_fanout_cap (150k)
and session_cap (2M) against `est`; only the *cumulative* ceiling loosens, and it self-heals on the
next `--reset` or once fresh billable exceeds the stale baseline. Graded HIGH, not CRITICAL. If a
follow-up adds a unit/version stamp to budget.json or auto-reset on schema change, this is resolved.
