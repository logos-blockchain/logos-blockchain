#!/usr/bin/env bash
# Evidence for finding 20261104T022437-3c6a52/2.
#
# The Trusted Setup Ceremony spec (as scoped for this repo) requires the key
# artefacts a node uses to be identified and verified against the ceremony's
# published output -- i.e. pinned/checked by hash and version. The finding was
# that:
#   (1) the only pin for the key-bearing crates is a mutable git *tag*
#       (`tag = "v0.5.7"`) in the workspace Cargo.toml -- not a hash of the
#       proving/verification key bytes; and
#   (2) the key loaders embed and use those bytes with no hash or version
#       check whatsoever (just `serde_json::from_slice(...).expect`).
#
# This script checks, from in-repo files only, that for each of the four
# circuits a sha256 pin of the proving key and of the verification key sits
# next to its loader, and that the embedded key bytes are only reached through
# those loaders, which go through the fail-closed `pinned_key` check.
#
# It used to exit 0 while the defect was present. It now exits 0 only when
# every key is pinned and checked, and 1 when a pin or a check is missing.
set -u
cd "$(dirname "$0")/../.."
ROOT="$(pwd)"
echo "== repo root: $ROOT"

echo
echo "== (1) how the key-bearing crates are pinned in the workspace Cargo.toml:"
grep -nE 'lbc-(poc|pol|poq|signature)-sys|tag *= *"v0\.5\.7"' Cargo.toml || true

echo
echo "== (2) the zksign verification-key loader:"
sed -n '1,40p' zk/proofs/zksign/src/verification_key/mod.rs

FAILED=0
fail() {
    echo "MISSING: $1"
    FAILED=1
}

echo
echo "== (3) pinned sha256 of the key bytes, checked where each key is loaded:"
for circuit in poc pol poq zksign; do
    for key in proving_key verification_key; do
        loader="zk/proofs/$circuit/src/$key/mod.rs"
        pin="$(echo "$key" | tr '[:lower:]' '[:upper:]')_SHA256"
        if [ ! -f "$loader" ]; then
            fail "$loader"
            continue
        fi
        # The pin: a 64 hex digit literal assigned to the `*_SHA256` constant.
        if grep -A1 -E "^const $pin: &str =" "$loader" | grep -qE '"[0-9a-f]{64}";'; then
            grep -n -A1 -E "^const $pin: &str =" "$loader" | sed "s|^|$loader:|"
        else
            fail "sha256 pin $pin in $loader"
        fi
        # The check: the loader hands the embedded bytes and the pin to `pinned_key`.
        if ! grep -A4 -F 'pinned_key(' "$loader" | grep -qF "$pin"; then
            fail "pinned_key(.., $pin) check in $loader"
        fi
    done
done

echo
echo "== (4) uses of the embedded key bytes outside the pinned loaders:"
HITS="$(grep -rnE 'artifacts::(PROVING|VERIFICATION)_KEY' --include='*.rs' \
                 --exclude-dir=.git --exclude-dir=target --exclude-dir=.cache . 2>/dev/null \
            | grep -vE '^\./zk/proofs/(poc|pol|poq|zksign)/src/(proving|verification)_key/mod\.rs:' \
            || true)"
if [ -n "$HITS" ]; then
    echo "$HITS"
    fail "embedded key bytes are used without going through a pinned loader"
else
    echo "(none found)"
fi

echo
if [ "$FAILED" -ne 0 ]; then
    echo "RESULT: the embedded key artefacts are not all pinned by hash and checked"
    echo "when loaded -> defect present"
    exit 1
fi

echo "RESULT: every embedded proving/verification key is pinned by sha256 and"
echo "checked against that pin when it is loaded."
exit 0
