//! Benchmark mantle transaction components

use blake2::{Blake2b, Digest as _};
/// Approach: Wrap all inputs to measured functions and all intermediate results
/// that might be optimized away.
use divan::{Bencher, black_box};
use lb_groth16::{Fr, GROTH16_SAFE_BYTES_SIZE, fr_from_bytes_unchecked};
use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey};
use lb_poseidon2::Digest;
use logos_blockchain_core::{
    crypto::ZkHasher,
    mantle::{
        MantleTx, SignedMantleTx, Transaction as _, TxHash,
        encoding::{decode_signed_mantle_tx, encode_mantle_tx, encode_signed_mantle_tx},
        ledger::Tx as LedgerTx,
        ops::{
            Op, OpProof,
            channel::{ChannelId, MsgId, inscribe::InscriptionOp},
        },
    },
};

fn main() {
    divan::main();
}

/// Payload sizes in bytes: 1 KB → 4 MB.
const SIZES: &[usize] = &[
    64,
    256,
    1024,
    4 * 1024,
    64 * 1024,
    512 * 1024,
    1024 * 1024,
    2 * 1024 * 1024,
    4 * 1024 * 1024,
];

fn make_inscription_tx(payload_size: usize) -> MantleTx {
    let signing_key = Ed25519Key::from_bytes(&[1; 32]);
    MantleTx {
        ops: vec![Op::ChannelInscribe(InscriptionOp {
            channel_id: ChannelId::from([0xAA; 32]),
            inscription: vec![0xAB; payload_size],
            parent: MsgId::from([0xBB; 32]),
            signer: signing_key.public_key(),
        })],
        ledger_tx: LedgerTx::new(vec![], vec![]),
        execution_gas_price: 100,
        storage_gas_price: 50,
    }
}

fn make_signed_tx(payload_size: usize) -> SignedMantleTx {
    let signing_key = Ed25519Key::from_bytes(&[1; 32]);
    let tx = make_inscription_tx(payload_size);
    let txhash = tx.hash();
    let op_sig = signing_key.sign_payload(&txhash.as_signing_bytes());
    SignedMantleTx::new(
        tx,
        vec![OpProof::Ed25519Sig(op_sig)],
        ZkKey::multi_sign(&[], &txhash.0).unwrap(),
    )
    .unwrap()
}

fn blake2b(inputs: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Blake2b::new();
    for input in inputs {
        hasher.update(input);
    }
    hasher.finalize().into()
}

/// Poseidon2 hash directly over payload field-elements.
#[divan::bench(args = SIZES)]
fn poseidon2_hash(bencher: Bencher, size: usize) {
    let tx = make_inscription_tx(size);
    bencher.bench_local(|| black_box(black_box(&tx).hash()));
}

/// Blake2b-512 over encoded bytes, then Poseidon2 over the compact 64-byte
/// digest.
#[divan::bench(args = SIZES)]
fn blake2b_poseidon2_hash(bencher: Bencher, size: usize) {
    let tx = make_inscription_tx(size);
    bencher.bench_local(|| {
        let encoded = black_box(encode_mantle_tx(black_box(&tx)));
        let digest = blake2b(&[encoded.as_slice()]);
        let frs: Vec<Fr> = black_box(digest)
            .chunks(GROTH16_SAFE_BYTES_SIZE)
            .map(fr_from_bytes_unchecked)
            .collect();
        black_box(<ZkHasher as Digest>::digest(black_box(&frs)))
    });
}

/// Ed25519 sign + `ZkKey` multi-sign.
/// Uses `with_inputs` so a fresh `MantleTx` (taken by value) is provided each
/// iteration — `SignedMantleTx::new` requires ownership.
#[divan::bench(args = SIZES)]
fn sign(bencher: Bencher, size: usize) {
    let signing_key = Ed25519Key::from_bytes(&[1; 32]);
    bencher
        .with_inputs(|| {
            let tx = make_inscription_tx(size);
            let txhash = tx.hash();
            (tx, txhash)
        })
        .bench_values(|(tx, txhash): (MantleTx, TxHash)| {
            let op_sig = signing_key.sign_payload(&black_box(txhash).as_signing_bytes());
            black_box(
                SignedMantleTx::new(
                    black_box(tx),
                    vec![OpProof::Ed25519Sig(black_box(op_sig))],
                    ZkKey::multi_sign(&[], &black_box(txhash).0).unwrap(),
                )
                .unwrap(),
            )
        });
}

/// Encode a `SignedMantleTx` to bytes.
#[divan::bench(args = SIZES)]
fn encode(bencher: Bencher, size: usize) {
    let signed_tx = make_signed_tx(size);
    bencher.bench_local(|| black_box(encode_signed_mantle_tx(black_box(&signed_tx))));
}

/// Decode a `SignedMantleTx` from bytes.
#[divan::bench(args = SIZES)]
fn decode(bencher: Bencher, size: usize) {
    let signed_tx = make_signed_tx(size);
    let encoded = encode_signed_mantle_tx(&signed_tx);
    bencher.bench_local(|| black_box(decode_signed_mantle_tx(black_box(&encoded))));
}
