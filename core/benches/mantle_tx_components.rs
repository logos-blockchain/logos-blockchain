//! Benchmark mantle transaction components
//!
//! `black_box` strategy:
//! - Always wrap the **output** of each measured closure to prevent dead-code
//!   elimination.
//! - `bench_values` benches: `with_inputs` supplies a fresh runtime value each
//!   iteration — no input wrapping needed.
//! - `bench_local` benches: wrap the input only when it is a plain byte buffer
//!   that the compiler could constant-propagate (see `decode`). Rich struct
//!   types are not wrapped.

use blake2::Digest as _;
use divan::{Bencher, black_box};
use lb_groth16::{Fr, GROTH16_SAFE_BYTES_SIZE, fr_from_bytes_unchecked};
use lb_key_management_system_keys::keys::{Ed25519Key, Ed25519Signature, ZkKey};
use lb_poseidon2::Digest;
use logos_blockchain_core::{
    crypto::{Hasher, ZkHasher},
    mantle::{
        MantleTx, SignedMantleTx, Transaction as _, TxHash,
        encoding::{decode_signed_mantle_tx, encode_mantle_tx, encode_signed_mantle_tx},
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

// Helper fn to create an inscription `MantleTx`, no ledger inputs ot outputs.
fn make_inscription_tx(payload_size: usize) -> MantleTx {
    let signing_key = Ed25519Key::from_bytes(&[1; 32]);
    MantleTx {
        ops: vec![Op::ChannelInscribe(InscriptionOp {
            channel_id: ChannelId::from([0xAA; 32]),
            inscription: vec![0xAB; payload_size],
            parent: MsgId::from([0xBB; 32]),
            signer: signing_key.public_key(),
        })],
        execution_gas_price: 100,
        storage_gas_price: 50,
    }
}

// Helper fn to create a `SignedMantleTx`.
fn make_signed_tx(payload_size: usize) -> SignedMantleTx {
    let signing_key = Ed25519Key::from_bytes(&[1; 32]);
    let tx = make_inscription_tx(payload_size);
    let txhash = tx.hash();
    let op_sig = signing_key.sign_payload(&txhash.as_signing_bytes());
    SignedMantleTx::new(tx, vec![OpProof::Ed25519Sig(op_sig)]).unwrap()
}

// `Blake2b` wrapper function usign the defined `Hasher`.
fn blake2b(inputs: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Hasher::new();
    for input in inputs {
        hasher.update(input);
    }
    hasher.finalize().into()
}

// Measure encoding the Mantle transaction - this overhead can be subtracted
// from the Poseidon2 and nested Blake2b-Poseidon2 hash benches to isolate
// hashing only performance.
#[divan::bench(args = SIZES)]
fn bench_encode_mantle_tx(bencher: Bencher, size: usize) {
    let tx = make_inscription_tx(size);
    bencher.bench_local(|| black_box(encode_mantle_tx(&tx)));
}

// Poseidon2 hash directly over payload field-elements.
#[divan::bench(args = SIZES)]
fn bench_poseidon2_hash(bencher: Bencher, size: usize) {
    let tx = make_inscription_tx(size);
    bencher.bench_local(|| black_box(tx.hash()));
}

// Blake2b over encoded bytes, then Poseidon2 over the compact 32-byte
// digest.
#[divan::bench(args = SIZES)]
fn bench_blake2b_poseidon2_hash(bencher: Bencher, size: usize) {
    bencher
        .with_inputs(|| make_inscription_tx(size))
        .bench_values(|tx: MantleTx| {
            // Encoding is included here to compare fairly with the Poseidon2 hash function,
            // which includes it.
            let encoded = encode_mantle_tx(&tx);
            let digest = blake2b(&[encoded.as_slice()]);
            let frs: Vec<Fr> = digest
                .chunks(GROTH16_SAFE_BYTES_SIZE)
                .map(fr_from_bytes_unchecked)
                .collect();
            black_box(<ZkHasher as Digest>::digest(&frs))
        });
}

// Ed25519 signature over the tx hash (payload size has no influence here)
#[divan::bench()]
fn bench_sign_a_ed25519_payload(bencher: Bencher) {
    let signing_key = Ed25519Key::from_bytes(&[1; 32]);
    bencher
        .with_inputs(|| {
            let tx = make_inscription_tx(1);
            tx.hash()
        })
        .bench_values(|txhash: TxHash| {
            black_box(signing_key.sign_payload(&txhash.as_signing_bytes()))
        });
}

// ZkKey multi-sign proof (payload size has no influence here)
#[divan::bench()]
fn bench_sign_b_zk_key_multi_sign_no_keys(bencher: Bencher) {
    bencher
        .with_inputs(|| {
            let tx = make_inscription_tx(1);
            tx.hash()
        })
        .bench_values(|txhash: TxHash| black_box(ZkKey::multi_sign(&[], &txhash.0).unwrap()));
}

// Verify ops proofs
#[divan::bench(args = SIZES)]
fn bench_sign_c_mantle_tx_new_verify_ops_proofs_single_proof(bencher: Bencher, size: usize) {
    let signing_key = Ed25519Key::from_bytes(&[1; 32]);
    bencher
        .with_inputs(|| {
            let tx = make_inscription_tx(size);
            let txhash = tx.hash();
            let op_sig = signing_key.sign_payload(&txhash.as_signing_bytes());
            (tx, op_sig)
        })
        .bench_values(|(tx, op_sig): (MantleTx, Ed25519Signature)| {
            black_box(SignedMantleTx::new(tx, vec![OpProof::Ed25519Sig(op_sig)]).unwrap())
        });
}

// Sign:
// - Ed25519 signature over the tx hash
// - ZkKey multi-sign proof
// - Verify ops proofs (`tx.hash()` re-calculated here)
#[divan::bench(args = SIZES)]
fn bench_sign_d_fully_empty(bencher: Bencher, size: usize) {
    let signing_key = Ed25519Key::from_bytes(&[1; 32]);
    bencher
        .with_inputs(|| {
            let tx = make_inscription_tx(size);
            let txhash = tx.hash();
            (tx, txhash)
        })
        .bench_values(|(tx, txhash): (MantleTx, TxHash)| {
            let op_sig = signing_key.sign_payload(&txhash.as_signing_bytes());
            black_box(SignedMantleTx::new(tx, vec![OpProof::Ed25519Sig(op_sig)]).unwrap())
        });
}

// Encode a `SignedMantleTx` to bytes.
#[divan::bench(args = SIZES)]
fn bench_encode_signed_mantle_tx(bencher: Bencher, size: usize) {
    let signed_tx = make_signed_tx(size);
    bencher.bench_local(|| black_box(encode_signed_mantle_tx(&signed_tx)));
}

// Decode a `SignedMantleTx` from bytes.
#[divan::bench(args = SIZES)]
fn bench_decode_signed_mantle_tx(bencher: Bencher, size: usize) {
    let signed_tx = make_signed_tx(size);
    let encoded = encode_signed_mantle_tx(&signed_tx);
    bencher.bench_local(|| {
        let protected_input_slice = black_box(&encoded);
        black_box(decode_signed_mantle_tx(protected_input_slice))
    });
}
