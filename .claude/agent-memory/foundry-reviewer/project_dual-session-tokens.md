---
name: dual-session-tokens
description: Two separate session_tokens() fns — budget.rs (CAP, reads billable_tokens) vs scheduler.rs (convergence, reads .tokens). Don't conflate when reviewing cap/convergence changes.
metadata:
  type: project
---

There are TWO independent `session_tokens()` functions in tf-core, easily conflated in review:

- `crates/tf-core/src/budget.rs::session_tokens()` — feeds the SESSION CAP. As of Issue #2 it
  prefers `billable_tokens` (in+out+cache_creation, cache-reads excluded) with a `-1` sentinel
  fallback to `.tokens`. Reason: a long session inflated raw `.tokens` to 71.6M ($61) and tripped
  the 2M cap at trivial real cost.
- `crates/tf-core/src/scheduler.rs::session_tokens()` — feeds CONVERGENCE (plan-open baseline /
  plan-close actual delta). Reads `.tokens` ONLY, unchanged.

**Why this matters for review:** a change to the cap's token source must NOT be assumed to touch
convergence, and vice versa. The byte-identical `plan-close` (no --actual) contract depends on
scheduler's `.tokens` path staying put.

**How to apply:** when a PR touches `billable_tokens` or the cap, verify it edited budget.rs's
copy and left scheduler.rs's copy reading `.tokens`. Frozen vectors `convergence_loop_advances` +
`gate_verdicts` (crates/tf-cli/tests/stateful.rs) and the conformance `convergence-loop-stdout`
differential (captures stdout only, `2>/dev/null`, so added stderr warnings are invisible) are the
guardrails. See [[binary-distribution-lazy-download]] for the version-tag requirement on bumps.
