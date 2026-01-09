#!/usr/bin/env bash
set -euo pipefail

# Wrapper over `nix develop` to simplify usage as a shell in GitHub Actions.
#
# Usage:
#   nix-develop.sh .#myDevShell {0}
#   nix-develop.sh ./subproject#myDevShell {0}
#   nix-develop.sh github:org/repo#myDevShell {0}
#   NIX_DEVELOP_FLAGS="--accept-flake-config" nix-develop.sh .#myDevShell {0}
#
# Required setup (run BEFORE any step that uses `shell: nix-develop.sh ... {0}`):
#   - uses: actions/checkout@v4                                   # Make repo files (this script) available
#   - name: Setup Nix
#     run: |
#       chmod +x .github/scripts/nix-develop.sh
#       echo "$PWD/.github/scripts" >> "$GITHUB_PATH"             # Adds scripts directory to PATH to simplify usage
#       echo /nix/var/nix/profiles/default/bin >> "$GITHUB_PATH"  # Ensure `nix` is on PATH (sometimes it's not)
#
# GHA Example:
#   - name: Run inside devShell
#     shell: nix-develop.sh .#myDevShell {0}
#     run: |
#       echo "Hello from myDevShell!"

if [[ $# -lt 2 ]]; then
  echo "Usage: $0 <dev-shell-or-flake-ref> <script-file> [args...]" >&2
  exit 2
fi

flake_ref="$1"
script_file="$2"
shift 2

# Optional extra flags via env (e.g., NIX_DEVELOP_FLAGS="--accept-flake-config")
NIX_DEVELOP_FLAGS="${NIX_DEVELOP_FLAGS:-}"

exec nix develop "$NIX_DEVELOP_FLAGS" "$flake_ref" -c bash -eo pipefail "$script_file" "$@"
