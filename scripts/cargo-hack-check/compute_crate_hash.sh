#!/usr/bin/env bash
set -euo pipefail

# Determine which SHA256 command to use
SHA256=(sha256sum)  # Linux
if ! command -v sha256sum >/dev/null 2>&1; then
    SHA256=(shasum -a 256)  # macOS
fi

compute_directory_tree_hash() {
    local directory="$1"
    (
        cd "$directory" || exit 1
        find . -type f \
            ! -path './target/*' \
            ! -path './.git/*' \
            -print0 \
        | LC_ALL=C sort -z \
        | xargs -0 "${SHA256[@]}" \
        | "${SHA256[@]}" \
        | cut -d ' ' -f1
    )
}

if [[ $# -ne 2 ]]; then
    echo "Usage: $0 <crate_directory> <workspace_cargo_lock>" >&2; exit 1;
fi

crate_directory="$1"
workspace_cargo_lock="$2"

# Hash the crate directory contents (working tree)
directory_tree_hash="$(compute_directory_tree_hash "$crate_directory")"

# Cargo.lock hash from repo root (script is run at workspace root)
lock_hash="NO_LOCK"
if [[ -f "$workspace_cargo_lock" ]]; then
    lock_hash="$("${SHA256[@]}" "$workspace_cargo_lock" | cut -d ' ' -f1)"
fi

# Combine both into one deterministic hash
printf '%s\n%s\n' "$directory_tree_hash" "$lock_hash" \
    | "${SHA256[@]}" \
    | cut -d ' ' -f1
