# Learning event — Workflow fan-out cap-bypass (2026-06-13)

Working brief for **issue #2**. On 2026-06-13 a 24+-agent fan-out was driven
through Claude Code's **Workflow** tool under a stated **+400k** token cap. Three of the scheduler's
token-safety guarantees did not fire and **~987k tokens** were spent — **2.47× the cap**. Nothing
was lost (the job finished, all outputs passed adversarial review), but the meter was unprotected.
This folder records what happened and specifies fixes (interfaces + frozen test vectors) to implement.

## TL;DR

- The fan-out ran via the **Workflow** tool, where the harness does **not** deliver `.rate_limits`
  to mid-turn Bash and the typed `+Xk` budget directive was never set (the user chose "+400k" through
  a question UI, not by typing it). So **L1 (live ceiling) was blind, L2 (hard budget) was never
  armed**, and `tf`'s convergence sample came back **0** because subagent tokens don't move the
  main-session counter `plan-close` reads.
- None of this is a surprise to the codebase: it is exactly the four items the scheduler's own
  `knowledge/token-aware-scheduling.md` §"Needs empirical verification" (lines 110–122) flagged as
  unconfirmed. **This event is that empirical probe.**

## The four open questions — now answered by this event

| # | Open question (from `token-aware-scheduling.md`) | What this event observed |
|---|---|---|
| 1 | Does a `PreToolUse(Agent\|Task)` hook receive `.rate_limits`? | Not usefully here — `tf gate` returned `no-live-signal`; the backstop never had a ceiling to act on. |
| 2 | Do subagents receive the live payload? | No — subagent token spend never moved `session.json .tokens` (P3). |
| 3 | Inside a Workflow, is `.rate_limits` readable, or only `budget.*`? | L1 behaved as blind; `budget.*` is the only plausible in-Workflow guard, and it was **null** (not typed). |
| 4 | Does `.cost.total_cost_usd` update intra-turn? | Not probed — flagged as the clean follow-up (`tf verify-payload`). |

## Contents

- [`incident-report.md`](./incident-report.md) — the full, honest account: timeline with real `tf`
  output, the three failures traced to named functions, metrics, and a five-layer scorecard.
- [`proposed-solutions.md`](./proposed-solutions.md) — three fixes + one procedure fix, each with the
  exact file/function, a new flag/verb interface, and frozen-vector test cases in the repo's
  `crates/tf-cli/tests/cli.rs` form. Prioritised "start here" order included.

## Status

This is the working brief for **issue #2**, landed on branch `fix/workflow-fanout-cap-bypass` (docs
only — CI stays green). The Rust fixes specified in `proposed-solutions.md` are still to be
implemented; the "start here" order is plan-close `--actual` → `ledger spend` → `doctor`/`gate
--on-no-signal`. The companion record lives in the idea-to-production repo under
`doc/token-fairness-learnings/`.
