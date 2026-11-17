#!/usr/bin/env bash
# Evidence for finding 20261104T022437-3c6a52/2.
#
# The Trusted Setup Ceremony spec (as scoped for this repo) requires the key
# artefacts a node uses to be identified and verified against the ceremony's
# published output -- i.e. pinned/checked by hash and version. This script
# shows, from in-repo files only, that:
#   (1) the only pin for the key-bearing crates is a mutable git *tag*
#       (`tag = "v0.5.7"`) in the workspace Cargo.toml -- not a hash of the
#       proving/verification key bytes; and
#   (2) the verification-key loaders embed and parse those bytes with no hash
#       or version check whatsoever (just `serde_json::from_slice(...).expect`).
#
# It exits 0 (defect present) when NO sha256/checksum/digest pin of the key
# bytes is found anywhere near where the keys are selected or loaded.
set -u
cd "$(dirname "$0")/../.."
ROOT="$(pwd)"
echo "== repo root: $ROOT"

echo
echo "== (1) how the key-bearing crates are pinned in the workspace Cargo.toml:"
grep -nE 'lbc-(poc|pol|poq|signature)-sys|tag *= *"v0\.5\.7"' Cargo.toml || true

echo
echo "== (2) the zksign verification-key loader (embeds + parses, no integrity check):"
sed -n '1,40p' zk/proofs/zksign/src/verification_key/mod.rs

echo
echo "== (3) search for any pinned hash of the key bytes near selection/loading:"
HITS="$(grep -rniE 'sha-?256|checksum|b3sum|blake3' \
            Cargo.toml \
            zk/proofs/zksign/src/verification_key \
            zk/proofs/zksign/src/lib.rs \
            zk/proofs/pol/src/lib.rs \
            zk/proofs/poc/src/lib.rs \
            zk/proofs/poq/src/lib.rs \
            zk/circuits/verifier/src \
            zk/circuits/prover/src \
         2>/dev/null || true)"
if [ -n "$HITS" ]; then
    echo "FOUND hash/checksum references (unexpected):"
    echo "$HITS"
    echo "RESULT: a key-byte hash pin appears to exist -> defect NOT demonstrated"
    exit 1
fi

echo "(none found)"
echo
echo "RESULT: no hash/version verification of the embedded key artefacts exists;"
echo "the keys are trusted solely via a mutable git tag and loaded with .expect()."
exit 0
