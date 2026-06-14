#!/usr/bin/env bash
# i2p-tf-widget-version: 1
# Statusline widget — the rate-limit BRIDGE + a combined-risk mini-bar.
#
# WHY: Claude Code delivers the live `.rate_limits` signal to the STATUSLINE's stdin, but NOT to
# hook payloads — so the hook-driven `tf snapshot` always no-ops and the dashboard's
# ratelimit-snapshot.json (its 5h/weekly source) is never written. This widget runs on every
# statusline render with that stdin, and (throttled) pipes it into `tf snapshot`, feeding the sink
# (ratelimit-snapshot.json + windows.json + signal-findings.json) and, in turn, the real-time web
# dashboard (its broadcaster watches captured_at and pushes `windows-updated`).
#
# It also renders ONE coloured segment: a 6-cell combined-risk mini-bar (worst of 5h/7d) + the
# worst-window %, colour-coded, + the snapshot sync age — so the bridge is self-evidently alive.
#
# Contract: fed the full statusline stdin JSON; prints ONE already-coloured segment, no trailing
# newline; NEVER exits non-zero and NEVER blocks the statusline. The `__TF_HOOK_SH__` placeholder
# is replaced with the absolute path to tf-hook.sh at install time (the statusline context has no
# ${CLAUDE_PLUGIN_ROOT}). If the placeholder is unsubstituted, the visual still renders; only the
# snapshot write is skipped.
set +e

TF_HOOK="__TF_HOOK_SH__"
THROTTLE="${I2P_STATUSLINE_SNAPSHOT_THROTTLE_SECONDS:-15}"
STATE_DIR="${I2P_COST_STATE_DIR:-$HOME/.claude/state/i2p-cost}"
SNAP="${STATE_DIR}/ratelimit-snapshot.json"
STAMP="${STATE_DIR}/.statusline-snapshot.stamp"

input=$(cat)
[ -n "$input" ] || exit 0

# ---- extract used_percentage per window (jq, with a pure-grep fallback) ----
_jq=''; command -v jq >/dev/null 2>&1 && _jq=1
getpct() { # $1 = window key (five_hour | seven_day)
  if [ -n "$_jq" ]; then
    printf '%s' "$input" | jq -r ".rate_limits.${1}.used_percentage // empty" 2>/dev/null
  else
    printf '%s' "$input" \
      | grep -o "\"${1}\"[[:space:]]*:[[:space:]]*{[^}]*}" 2>/dev/null \
      | grep -o '"used_percentage"[[:space:]]*:[[:space:]]*[0-9.]*' \
      | grep -o '[0-9.]*$' | head -1
  fi
}
five=$(getpct five_hour)
seven=$(getpct seven_day)

# No live signal in this payload → render nothing (keeps non-Claude renders clean).
[ -z "$five" ] && [ -z "$seven" ] && exit 0

now=$(date +%s 2>/dev/null || echo 0)

# ---- bridge: throttled snapshot write (feeds the sink + the web report) ----
# Guard is the readability of TF_HOOK: after install it is an absolute path to tf-hook.sh; if the
# install-time substitution never ran, TF_HOOK is still the bare placeholder token (not a real
# file) and `-r` is false, so the write is safely skipped. (Do NOT also compare against the literal
# placeholder here — the installer's sed would rewrite that literal too, defeating the guard.)
if [ -r "$TF_HOOK" ]; then
  last=0
  [ -r "$STAMP" ] && last=$(tr -dc '0-9' < "$STAMP" 2>/dev/null | head -c 12)
  last=${last:-0}
  if [ "$now" -gt 0 ] 2>/dev/null && [ $(( now - last )) -ge "$THROTTLE" ] 2>/dev/null; then
    printf '%s' "$input" | bash "$TF_HOOK" snapshot >/dev/null 2>&1
    mkdir -p "$STATE_DIR" 2>/dev/null
    printf '%s' "$now" > "${STAMP}.tmp.$$" 2>/dev/null && mv -f "${STAMP}.tmp.$$" "$STAMP" 2>/dev/null
  fi
fi

# ---- visual: combined-risk mini-bar + sync age ----
risk=$(awk -v a="${five:-0}" -v b="${seven:-0}" 'BEGIN{ r=(a+0>b+0)?a+0:b+0; if(r<0)r=0; if(r>100)r=100; printf "%d", r }' 2>/dev/null)
risk=${risk:-0}

R=$'\033[0m'; DIM=$'\033[2m'
FG_BGREEN=$'\033[92m'; FG_BYELLOW=$'\033[93m'; FG_BRED=$'\033[91m'; FG_BBLACK=$'\033[90m'
# Same thresholds as the house gauge_bar: green <60, amber 60-84, red >=85.
if   [ "$risk" -ge 85 ]; then clr="$FG_BRED"
elif [ "$risk" -ge 60 ]; then clr="$FG_BYELLOW"
else clr="$FG_BGREEN"; fi

width=6
filled=$(( risk * width / 100 ))
[ "$filled" -gt "$width" ] && filled=$width
[ "$filled" -lt 0 ] && filled=0
empty=$(( width - filled ))
bar=""; i=0; while [ "$i" -lt "$filled" ]; do bar="${bar}▰"; i=$(( i + 1 )); done
i=0; while [ "$i" -lt "$empty" ]; do bar="${bar}▱"; i=$(( i + 1 )); done

# sync age from the snapshot's captured_at (proves the sink is being fed)
age_str="${DIM}◌BLIND${R}"
if [ -r "$SNAP" ]; then
  cap=$(grep -o '"captured_at"[[:space:]]*:[[:space:]]*[0-9]*' "$SNAP" 2>/dev/null | grep -o '[0-9]*$' | head -1)
  if [ -n "$cap" ] && [ "$now" -gt 0 ] 2>/dev/null; then
    age=$(( now - cap )); [ "$age" -lt 0 ] && age=0
    if [ "$age" -le 900 ] 2>/dev/null; then
      age_str="${FG_BGREEN}⇡${age}s${R}"
    else
      age_str="${DIM}◌${age}s${R}"
    fi
  fi
fi

printf "${FG_BBLACK}⬢ tf ${clr}${bar} ${risk}%%${R} ${age_str}"
