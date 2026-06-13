#!/usr/bin/env bash
# tf-hook.sh — the bash shim every hook command invokes. A compiled binary cannot be the
# hook command directly: verify-prereqs Check L runs `bash -n` + a smoke-exec on each
# hooks.json command, and an ELF fails `bash -n`. This script DOES parse as bash, resolves
# the correct per-arch `tf` binary, and exec's it — forwarding stdin, args, and exit code
# unchanged. Cron/hook env is minimal, so we resolve absolute paths here.
#
# The repo ships NO binaries (they are not committed — keeps git lean). Resolution order:
#   1. a locally built/copied bin/tf-<arch>-<os> (dev tree, CI build),
#   2. a previously cached download under $XDG_CACHE_HOME/token-fairness/<version>/,
#   3. a lazy download of the per-arch asset from the pinned GitHub release (checksum-verified),
#   4. a `tf` on PATH (cargo-install / dev build),
#   5. fail soft (exit 0) — hooks are `|| true`, so a missing binary never blocks the session.
#
#   bash tf-hook.sh <verb> [args…]
set -uo pipefail

ROOT="${CLAUDE_PLUGIN_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
arch="$(uname -m 2>/dev/null || echo unknown)"
os="$(uname -s 2>/dev/null | tr 'A-Z' 'a-z' || echo unknown)"
case "$os" in linux*) os=linux ;; darwin*) os=darwin ;; esac
name="tf-${arch}-${os}"

# (1) shipped/built binary in the plugin tree.
bin="${ROOT}/bin/${name}"
if [ -x "$bin" ]; then exec "$bin" "$@"; fi

# Version (for the release tag + cache key), read from the plugin manifest.
ver="$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "${ROOT}/.claude-plugin/plugin.json" 2>/dev/null | head -1)"
cache="${XDG_CACHE_HOME:-$HOME/.cache}/token-fairness/${ver:-unknown}"
cbin="${cache}/${name}"

# (2) previously cached download.
if [ -x "$cbin" ]; then exec "$cbin" "$@"; fi

# (3) lazy download from the pinned release, checksum-verified, then cache + exec.
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | awk '{print $1}'
  fi
}
if [ -n "$ver" ] && command -v curl >/dev/null 2>&1; then
  base="https://github.com/agentic-underground/token-fairness/releases/download/v${ver}"
  mkdir -p "$cache" 2>/dev/null || true
  tmp="${cbin}.tmp.$$"
  if curl -fsSL "${base}/${name}" -o "$tmp" 2>/dev/null; then
    ok=1
    sums="${cache}/SHA256SUMS"
    curl -fsSL "${base}/SHA256SUMS" -o "$sums" 2>/dev/null || true
    want="$(awk -v f="$name" '$2 ~ ("(^|/)" f "$") {print $1}' "$sums" 2>/dev/null | head -1)"
    got="$(sha256_of "$tmp")"
    # If we have BOTH a published sum and a hashing tool, they MUST match; else trust TLS.
    if [ -n "$want" ] && [ -n "$got" ] && [ "$want" != "$got" ]; then ok=0; fi
    if [ "$ok" = 1 ]; then chmod +x "$tmp" 2>/dev/null && mv -f "$tmp" "$cbin" 2>/dev/null && exec "$cbin" "$@"; fi
    rm -f "$tmp" 2>/dev/null || true
  fi
fi

# (4) a `tf` on PATH.
if command -v tf >/dev/null 2>&1; then exec tf "$@"; fi

# (5) fail soft.
echo "tf-hook: no tf binary for ${arch}-${os} (no local build, none cached, download unavailable, none on PATH)" >&2
exit 0
