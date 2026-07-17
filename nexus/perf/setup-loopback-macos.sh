#!/usr/bin/env bash
# macOS only. Linux routes all of 127.0.0.0/8 to loopback with no setup, but
# macOS only brings up 127.0.0.1, so the simulated fleet's extra addresses
# (127.0.0.2 ..) must be aliased onto lo0 before the perf run.
#
# Usage:   sudo ./setup-loopback-macos.sh <count> [base_octet]
#   count       number of switches (matches --switches)
#   base_octet  first last-octet (matches --base-octet, default 2)
#
# Teardown:  sudo ./setup-loopback-macos.sh down <count> [base_octet]
set -euo pipefail

if [[ "$(uname)" != "Darwin" ]]; then
  echo "not macOS; no aliases needed on Linux." >&2
  exit 0
fi

action="up"
if [[ "${1:-}" == "down" ]]; then action="down"; shift; fi
count="${1:?usage: setup-loopback-macos.sh [down] <count> [base_octet]}"
base="${2:-2}"

for ((n = 0; n < count; n++)); do
  x=$((base + n))
  ip="127.0.$((x / 256)).$((x % 256))"
  [[ "$ip" == "127.0.0.1" ]] && continue
  if [[ "$action" == "up" ]]; then
    ifconfig lo0 alias "$ip" up 2>/dev/null || true
  else
    ifconfig lo0 -alias "$ip" 2>/dev/null || true
  fi
done
echo "lo0 aliases ${action} for ${count} addresses from 127.0.0.${base}"
