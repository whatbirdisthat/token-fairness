#!/usr/bin/env bash
# session-tokens.sh — Stop hook. Writes ~/.claude/state/i2p-cost/session.json `.tokens`, the
# ACTUAL-spend signal that makes the estimator converge (review C3 / §3.4 — CRITICAL).
#
# This MUST travel WITH the scheduler: `tf plan-close` reads `session.json .tokens` to compute a
# plan's real cost and feed `calibrate close`. If this writer is absent, plan-close sees
# `cur==base==0`, `actual==0`, and the EWMA never advances — silently. (plan-close additionally
# emits a visible warning when it sees base==cur==0, so the failure is never silent.)
#
# Ported from concierge's statusline/capture-cost.sh — the session.json writer only (lifecycle/
# per-phase cost attribution stays in concierge; this plugin owns just the convergence signal).
# Always exits 0; never blocks. Needs jq (no-op without it).
set -uo pipefail

PLUGIN_ROOT="${CLAUDE_PLUGIN_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
PRICES="${PLUGIN_ROOT}/statusline/model-prices.tsv"

command -v jq >/dev/null 2>&1 || exit 0
payload=""; [ -t 0 ] || payload="$(cat 2>/dev/null || true)"
[ -n "$payload" ] || exit 0

tp="$(printf '%s' "$payload"  | jq -r '.transcript_path // empty' 2>/dev/null)"
sid="$(printf '%s' "$payload" | jq -r '.session_id // empty' 2>/dev/null)"
[ -n "$tp" ] && [ -r "$tp" ] || exit 0
[ -n "$sid" ] || sid="nosid"

state="${I2P_COST_STATE_DIR:-${HOME}/.claude/state/i2p-cost}"
mkdir -p "$state" 2>/dev/null || exit 0
safe_sid="$(printf '%s' "$sid" | tr -c 'A-Za-z0-9._-' '_')"
ck="${state}/${safe_sid}.ckpt"   # holds: "<lines> <tokens> <usd> <billable_tokens>"

# billable_tokens = in+out+cache_creation (EXCLUDES cache_read). The budget cap (CORE-A) reads it
# so cheap cache hits — which inflate raw .tokens into the tens of millions on a long session — don't
# trip the cap at trivial real cost (Issue #2 follow-up). The 4th ckpt field is optional: an old
# 3-field checkpoint reads prev_billable="" → 0 and re-accumulates from here (self-healing).
last_lines=0 prev_tokens=0 prev_usd=0 prev_billable=0
if [ -r "$ck" ]; then read -r last_lines prev_tokens prev_usd prev_billable < "$ck" 2>/dev/null; fi
case "$last_lines"  in (''|*[!0-9]*) last_lines=0 ;; esac
case "$prev_tokens" in (''|*[!0-9]*) prev_tokens=0 ;; esac
case "$prev_usd" in (''|*[!0-9.]*) prev_usd=0 ;; esac
case "$prev_billable" in (''|*[!0-9]*) prev_billable=0 ;; esac

cur_lines="$(wc -l < "$tp" 2>/dev/null | tr -d ' ')"; case "$cur_lines" in (''|*[!0-9]*) cur_lines=0 ;; esac
# Transcript shrank (compaction / replacement) → reprocess from the top, reset cumulative.
if [ "$cur_lines" -lt "$last_lines" ]; then last_lines=0; prev_tokens=0; prev_usd=0; prev_billable=0; fi
start=$((last_lines + 1))
[ "$cur_lines" -ge "$start" ] || exit 0   # no new lines this turn

# Sum new assistant lines: total tokens + USD (priced per model via model-prices.tsv).
delta="$(
  tail -n +"$start" "$tp" 2>/dev/null \
  | jq -r 'select(.type=="assistant") | [(.message.model//"?"),(.message.usage.input_tokens//0),(.message.usage.output_tokens//0),(.message.usage.cache_creation_input_tokens//0),(.message.usage.cache_read_input_tokens//0)] | @tsv' 2>/dev/null \
  | awk -v PF="$PRICES" '
      BEGIN { FS="\t"; k=0
        while ((getline line < PF) > 0) {
          if (line ~ /^#/ || line ~ /^[[:space:]]*$/) continue
          split(line,a,"\t"); pref[++k]=a[1]; pin[a[1]]=a[2]+0; pout[a[1]]=a[3]+0; pcw[a[1]]=a[4]+0; pcr[a[1]]=a[5]+0
        } }
      { m=$1; tin=$2+0; tout=$3+0; tcw=$4+0; tcr=$5+0
        tokens += tin+tout+tcw+tcr
        billable += tin+tout+tcw            # excludes cache_read (the cheap, count-inflating part)
        ri=0; ro=0; rcw=0; rcr=0
        for (i=1;i<=k;i++){ p=pref[i]; if (substr(m,1,length(p))==p){ ri=pin[p];ro=pout[p];rcw=pcw[p];rcr=pcr[p]; break } }
        usd += (tin*ri + tout*ro + tcw*rcw + tcr*rcr)/1000000.0 }
      END { printf "%d %d %.6f", tokens+0, billable+0, usd+0 }'
)"
read -r d_tokens d_billable d_usd <<EOF2
$delta
EOF2
case "$d_tokens" in (''|*[!0-9]*) d_tokens=0 ;; esac
case "$d_billable" in (''|*[!0-9]*) d_billable=0 ;; esac
case "$d_usd" in (''|*[!0-9.]*) d_usd=0 ;; esac

new_tokens=$((prev_tokens + d_tokens))
new_billable=$((prev_billable + d_billable))
new_usd="$(awk -v a="$prev_usd" -v b="$d_usd" 'BEGIN{printf "%.6f", a+b}')"
printf '%s %s %s %s\n' "$cur_lines" "$new_tokens" "$new_usd" "$new_billable" > "${ck}.tmp.$$" 2>/dev/null && mv -f "${ck}.tmp.$$" "$ck" 2>/dev/null

# session.json — the cumulative session counters. `.tokens` (full) feeds spend/convergence;
# `.billable_tokens` (cache-read-excluded) is what the budget cap reads (Issue #2 follow-up).
sj="${state}/session.json"
jq -n --arg sid "$sid" --argjson tok "$new_tokens" --argjson usd "$new_usd" --argjson bill "$new_billable" \
  '{session_id:$sid, tokens:$tok, usd:$usd, billable_tokens:$bill}' > "${sj}.tmp.$$" 2>/dev/null && mv -f "${sj}.tmp.$$" "$sj" 2>/dev/null
exit 0
