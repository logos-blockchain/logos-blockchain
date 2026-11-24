#!/usr/bin/env bash
set -euo pipefail

VERSION="0.10.0"

current="$(taplo --version 2>/dev/null || echo "none")"

if [[ "$current" != "taplo $VERSION" ]]; then
  echo "taplo $VERSION required (found: $current). Install it with:" >&2
  echo "  cargo install taplo-cli --locked --version $VERSION" >&2
  exit 1
fi

exec taplo "$@"
