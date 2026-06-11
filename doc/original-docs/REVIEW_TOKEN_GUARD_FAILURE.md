> **⚠️ SUPERSEDED (2026-06-11).** This post-mortem's central claim — "the limit is invisible to
> scripts" — is **false**: the harness delivers the live rolling rate-limit window to every turn
> (`.rate_limits.five_hour.used_percentage`), which the status line already reads. That false premise
> drove an over-correction ("never autonomously resume; prefer serial"). The corrected operating
> model — a live ceiling guard, a hard budget cap, supervised off-peak continuation, and cheap resume,
> all backed by a 100%-tested deterministic library — now lives in
> [`plugins/concierge/knowledge/token-aware-scheduling.md`](plugins/concierge/knowledge/token-aware-scheduling.md)
> and is enacted by the `token-scheduler` skill (`/concierge:schedule`). This file is kept only as the
> incident record; its still-valid rules (HALT-don't-retry, budget-as-consent, throttle, pre-flight)
> are carried forward in the reconciliation section of that doc.

# Token Guard Failure — Post-Mortem and Revised Background-Work Pattern

## What happened

The adversarial review + improvement workflows were launched as background fan-outs (explicit user opt-in). A token-watch cron (`*/13 * * * *`) was designed to monitor spend and pause the workflows when tokens ran low. The resume logic was also automated via a one-shot cron.

**The system failed three times.** Each time, the workflows exhausted the user's **monthly spend limit** ("You've hit your monthly spend limit · raise it at claude.ai/settings/usage") before the Verify/Improve/Re-review stages could run. The user was blocked from interactive model access for several hours.

---

## Root cause analysis

### 1. Monthly spend limit is invisible to scripts

There is no disk file tracking the monthly credit balance. The only persisted token signal is `~/.claude/state/i2p-cost/session.json` (session token count, written by the concierge Stop hook). This is a completely different metric. The token-watch cron monitored it as a proxy — but the monthly limit can be hit even when the session counter is low, because it resets on the monthly billing cycle, not per session.

**Consequence:** The cron had no signal until AFTER the workflow completed with errors. The damage was already done.

### 2. Crons auto-resumed without user consent

The 5-hour one-shot resume cron and manual resume logic in the token-watch restarted workflows autonomously after the subscription's 5-hour session reset. The user had no opportunity to say "I don't have monthly credits available right now." Once the cron fired, the workflows started immediately and burned through the monthly budget in under 2 minutes.

### 3. Parallel fan-out is credit-greedy and unthrottleable

~80–130 parallel subagents launch simultaneously. At the scale of this job (26 files × 4 stages), this exhausts a monthly limit faster than any 13-minute monitoring interval can catch. There is no way to throttle this from inside a workflow when the monthly limit is not readable via any API or disk signal.

### 4. Rate-limit error detection is post-mortem, not pre-flight

The cron prompt said "check TaskOutput for rate-limit errors" — but TaskOutput is only readable AFTER the workflow completes. By definition, this is too late.

---

## The broken pattern (DO NOT USE)

```
launch workflow (parallel fan-out, many agents)
  → cron monitors proxy metric every N minutes
    → if proxy exceeded: TaskStop + pause in ledger
    → cron auto-resumes after N hours
```

**Why it's broken:** The monitoring interval is 13 minutes; the monthly limit can be exhausted in < 2 minutes. The proxy metric (session tokens) does not reflect monthly spend. Auto-resume fires without user consent or credit availability check.

---

## Corrected background-work rules

### Rule 1: Crons MONITOR and ALERT — never autonomously start or resume a Workflow

A cron may check status and post a health note. It must NOT call `Workflow {}` or trigger a resume. Background workflow launches and resumes are **always** explicit user actions.

### Rule 2: Workflows must carry an explicit budget directive

Background workflows must use the `budget` API inside the script:

```javascript
// Refuse to run autonomously without an explicit budget
if (!budget.total) {
  log('No budget directive set — refusing autonomous run. User must pass a +Xk budget.')
  return { skipped: true, reason: 'no-budget' }
}

// Halt before spending the last 50k
while (budget.remaining() > 50_000) {
  // ... launch agents ...
}
```

The user signals consent and capacity by including `+Xk` in their message (e.g. "run this with +500k"), which sets `budget.total`. Without it, the workflow refuses to proceed.

### Rule 3: Rate-limit errors = HALT, do not retry

When a workflow agent fails with a rate-limit or monthly-limit error, the workflow script must catch it and record the halt — not continue to the next unit. The `pipeline()` and `parallel()` already drop null results from failed agents, but the workflow should explicitly log the reason and return early rather than producing a silent partial result.

### Rule 4: Pre-flight > post-hoc monitoring

Before launching any multi-agent fan-out, the main session should confirm the approach with the user if there is any uncertainty about credit headroom. A cheap pre-flight read of `session.json` can catch obvious cases (session counter already high), but cannot catch monthly limit proximity. When in doubt, ask the user before launching.

### Rule 5: Prefer serial in-session work for credit-sensitive tasks

For tasks where the Review stage is already done (cached results on disk), skip the workflow entirely. Apply improvements one file at a time in the main session using the cached data. One agent per file is dramatically cheaper than a parallel fan-out, and the user can stop at any point.

---

## What was recovered

25 of 26 file reviews were cached from the workflow transcripts before the credits ran out. These are available as human-readable markdown in `doc/cached-reviews/<slug>.md` with:
- Severity-ranked findings (CRITICAL/HIGH/MEDIUM/LOW/SUGGESTION) with quoted evidence
- Capability-uplift proposals with concrete uplift text and rationale

Missing (review never completed): `handler-graphviz`, `handler-mermaid`.

---

## Revised plan for this task

Apply improvements in the main session, file by file, using `doc/cached-reviews/<slug>.md` as input:
1. Read the cached review for the file.
2. One agent applies the confirmed improvements (Edit, one file only).
3. Quick diff check before moving to the next file.
4. For `handler-graphviz` and `handler-mermaid`: combined one-pass review+improve agent.
5. Write reports after all files done.

No autonomous crons. No auto-resume. User controls pacing.

---

## Lessons for the marketplace self-improve pattern

The existing `/foundry:self-improve` skill already enforces the right pattern: one element at a time, on a branch, through `/foundry:pr-review`, then PR. It does not fan out autonomously. That is the correct shape for improvement work on marketplace agents.

The mistake was treating this task as a "background batch job" rather than a "structured improvement session." Batch-job thinking led to the autonomous cron infrastructure. The self-improve skill's interactive, bounded pattern would have been safer — and should be the default for future improvement work on agents.
