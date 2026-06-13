#!/usr/bin/env bash
# startup-report.sh — SessionStart hook. Session-SAFE scheduled jobs: nothing is silently lost to
# a crash/restart. On every session start it (1) resets ephemeral armed-state (a fresh session has
# no live in-session crons — CronCreate is session-only; OS-cron arming survives), (2) prints the
# brief dashboard of durable jobs + estimator convergence, and (3) injects context telling the agent
# to RE-ARM any durable cron. Silent when there is nothing scheduled and no calibration (never nags).
#
# Delegates determinism to the `tf` binary (resolved per-arch via tf-hook.sh's logic).
set -uo pipefail

ROOT="${CLAUDE_PLUGIN_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
# Resolve the per-arch tf binary (same logic as tf-hook.sh), or a `tf` on PATH.
arch="$(uname -m 2>/dev/null || echo unknown)"; os="$(uname -s 2>/dev/null | tr 'A-Z' 'a-z' || echo unknown)"
case "$os" in linux*) os=linux ;; darwin*) os=darwin ;; esac
TF="${ROOT}/bin/tf-${arch}-${os}"
[ -x "$TF" ] || TF="$(command -v tf 2>/dev/null || true)"
[ -n "$TF" ] || exit 0
command -v jq >/dev/null 2>&1 || exit 0

payload=""; [ -t 0 ] || payload="$(cat 2>/dev/null || true)"
cwd=""
[ -n "$payload" ] && cwd="$(printf '%s' "$payload" | jq -r '.cwd // empty' 2>/dev/null)"
[ -n "$cwd" ] || cwd="$(pwd 2>/dev/null || echo .)"

# (The stale-session.json spend reset now lives in the `tf session-boundary` SessionStart hook —
# it zeroes session.json on a session_id change so the preflight-spend gate doesn't read a prior
# heavy session's total as current spend. See plugins/scheduler/hooks/hooks.json.)

# Fresh session ⇒ no in-session cron is live yet. Reset ephemeral arming so the report is truthful.
"$TF" registry reset-armed "$cwd" >/dev/null 2>&1 || true

brief="$("$TF" report "$cwd" --brief 2>/dev/null || true)"
[ -n "$brief" ] || exit 0   # nothing scheduled, no calibration → stay silent

ctx="token-fairness scheduler — session-safe startup. Durable scheduled jobs live in ${cwd}/.i2p/scheduled-jobs.json (ledgers in .i2p/jobs/). The DURABLE arming path is OS-cron (\`tf oscron install ${cwd} <id>\`, then \`tf registry arm ${cwd} <id> oscron\`) — it survives Claude being closed (machine awake). A job shown as '⚠ NOT armed' should be offered OS-cron (or, ephemerally, an in-session CronCreate re-arm). Full picture: \`tf report ${cwd}\`. When the user asks 'how's the estimator doing?', run \`tf report ${cwd} --estimator\`."

msg="$brief"

# KAIZEN at startup — the self-improving estimator's current champion(s) + accuracy, so every
# session opens knowing how the predictor is doing. Up to 2 classes; silent when there's no data.
kz="$("$TF" report "$cwd" --kaizen 2>/dev/null | grep 'MAPE' | head -2)"
[ -n "$kz" ] && msg="${msg}"$'\n'"🧠 KAIZEN"$'\n'"$kz"
ctx="${ctx} The estimator self-improves: it runs several prediction algorithms concurrently and promotes the most accurate (champion) per job class; \`tf report ${cwd} --kaizen\` shows the ensemble scoreboard, \`--taxonomy\` the classification graph, and \`tf estimator backtest <key>\` replays history to find the best formula."

# Periodic tip (line 3) — throttled to ~once/day so it teaches without nagging.
tipfile="${HOME}/.claude/hook-state/scheduler-tip-last"
now_epoch="$(date +%s 2>/dev/null || echo 0)"
last_tip=0; [ -r "$tipfile" ] && last_tip="$(cat "$tipfile" 2>/dev/null | tr -dc '0-9')"; [ -n "$last_tip" ] || last_tip=0
if [ $(( now_epoch - last_tip )) -ge 72000 ]; then   # 20 h
  msg="${msg}"$'\n'"💡 ask \"how's the estimator doing?\" for the full convergence report"
  mkdir -p "$(dirname "$tipfile")" 2>/dev/null && printf '%s' "$now_epoch" > "$tipfile" 2>/dev/null || true
fi

jq -cn --arg m "$msg" --arg c "$ctx" \
  '{systemMessage:$m, hookSpecificOutput:{hookEventName:"SessionStart", additionalContext:$c}}'
exit 0
