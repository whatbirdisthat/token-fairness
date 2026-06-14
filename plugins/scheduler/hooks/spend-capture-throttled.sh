#!/usr/bin/env bash
# spend-capture-throttled.sh — live per-turn spend capture for the PostToolUse hook (Issue #11,
# Part 2d). It appends a `spend` event to the honesty ledger so the dashboard's Session Budget
# number moves MID-session, not only at the Stop hook.
#
# MANDATORY GUARDS (this runs on the HOT PostToolUse path, after every tool):
#   * AFTER the tool — PostToolUse fires once the tool has already run, so this can never delay or
#     veto a tool. It also touches NO gate / deny logic.
#   * FAIL-OPEN — every step is best-effort; the script swallows all errors and always `exit 0`,
#     so a slow or broken capture can never block a tool or fail a turn.
#   * THROTTLED — pricing the transcript on EVERY tool use would add latency to every tool. We
#     skip the capture unless at least THROTTLE_SECONDS have elapsed since the last one, using a
#     stamp file. A typical turn fires many PostToolUse hooks; this collapses them to ~one capture
#     per window while still surfacing spend within seconds.
set -uo pipefail

THROTTLE_SECONDS="${I2P_SPEND_CAPTURE_THROTTLE_SECONDS:-20}"

ROOT="${CLAUDE_PLUGIN_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"

# State dir mirrors the tf binary's default (honours the same override the binary reads), so the
# stamp lives beside the other i2p-cost state and tests can isolate it.
state_dir="${I2P_COST_STATE_DIR:-$HOME/.claude/state/i2p-cost}"
stamp="${state_dir}/.spend-capture.stamp"

now="$(date +%s 2>/dev/null || echo 0)"

# Throttle: if a recent capture stamp exists and is within the window, skip (fail-open exit 0).
if [ -f "$stamp" ]; then
  last="$(cat "$stamp" 2>/dev/null || echo 0)"
  case "$last" in
    ''|*[!0-9]*) last=0 ;;
  esac
  if [ "$now" -ge "$last" ] && [ "$((now - last))" -lt "$THROTTLE_SECONDS" ]; then
    exit 0
  fi
fi

# Record the attempt time FIRST so concurrent PostToolUse hooks don't all run the capture.
mkdir -p "$state_dir" 2>/dev/null || true
printf '%s' "$now" > "$stamp" 2>/dev/null || true

# Best-effort capture; never block, never non-zero.
bash "${ROOT}/hooks/tf-hook.sh" spend --capture >/dev/null 2>&1 || true

exit 0
