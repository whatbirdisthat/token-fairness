#!/usr/bin/env bash
# ensure-statusline-widget.sh — idempotently install/refresh the rate-limit statusline widget.
#
# Ships the bridge widget into the user's statusline extension dir
# (~/.claude/state/statusline-widgets.d/), substituting the absolute tf-hook.sh path at install
# time (the statusline render context has no ${CLAUDE_PLUGIN_ROOT}). Drift-aware: only (re)writes
# when the installed copy is missing or differs from the freshly rendered source. Fail-open: any
# problem → exit 0, never blocks SessionStart. Wired as a SessionStart hook.
#
# Installed as 00-tf-ratelimit.sh so it sorts FIRST among statusline widgets (leftmost the widget
# mechanism allows; true line-1 top-left would require a change to the concierge-owned renderer).
set +e

ROOT="${CLAUDE_PLUGIN_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
SRC="${ROOT}/hooks/statusline-ratelimit-widget.sh"
TF_HOOK="${ROOT}/hooks/tf-hook.sh"
DEST_DIR="${HOME}/.claude/state/statusline-widgets.d"
DEST="${DEST_DIR}/00-tf-ratelimit.sh"

[ -r "$SRC" ] || exit 0
mkdir -p "$DEST_DIR" 2>/dev/null || exit 0

# Render: bake the absolute tf-hook.sh path into the placeholder.
rendered=$(sed "s#__TF_HOOK_SH__#${TF_HOOK}#g" "$SRC" 2>/dev/null)
[ -n "$rendered" ] || exit 0

# Drift check: no-op when the installed copy already matches.
if [ -r "$DEST" ] && printf '%s\n' "$rendered" | cmp -s - "$DEST" 2>/dev/null; then
  exit 0
fi

printf '%s\n' "$rendered" > "${DEST}.tmp.$$" 2>/dev/null && mv -f "${DEST}.tmp.$$" "$DEST" 2>/dev/null
chmod +x "$DEST" 2>/dev/null
exit 0
