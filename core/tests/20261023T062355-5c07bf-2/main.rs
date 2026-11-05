//! Finding 20261023T062355-5c07bf/2 — Bedrock v1.1 Mantle spec, Appendix ›
//! Test Vectors › Operation Id (`SDP_ACTIVE` row) and Mantle Transaction Hash
//! ("Transaction with one of each operation" row).
//!
//! The Appendix gives the exact payload bytes of each operation and of a
//! transaction holding one of each; the code must read those bytes and hash
//! them to the listed `op_id` / transaction hash.

use lb_binary_codec::canonical::{BinaryDecode as _, BinaryEncode as _};
use lb_cryptarchia_engine::Epoch;
use logos_blockchain_core::{
    crypto::{Digest as _, Hasher},
    mantle::{Op, ops::sdp::SDPActiveOp, traits::Hashable as _, transactions::Ops},
    sdp::ActivityMetadata,
};

const SDP_ACTIVE_OPCODE: u8 = 0x22;
const SDP_ACTIVE_PAYLOAD: &str = "1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1f0000000000000001010a0000008a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020303030303030303030303030303030303030303030303030303030303030303";
const SDP_ACTIVE_OP_ID: &str = "76afa55f5733db75a982dc5ccabb5c6a7dab992eda78cdfd5f657f314e388354";

const ONE_OF_EACH_TX: &str = "0a000201000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000020300000000000000040000000000000000000000000000000000000000000000000000000000000005000000000000000600000000000000000000000000000000000000000000000000000000000000100707070707070707070707070707070707070707070707070707070707070707000000000000000000000000000000000000000000000000000000000000000002001398f62c6d1a457c51ba6a4b5f3dbd2f69fca93216218dc8997e416bd17d93cafd1724385aa0c75b64fb78cd602fa1d991fdebf76b13c58ed702eac835e9f6180a0000000b0000000c000d00110e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0b00000068656c6c6f206c6f676f730000000000000000000000000000000000000000000000000000000000000000d9bf2148748a85c89da5aad8ee0b0fc2d105fd39d41a4c796536354f0ae2900c121010101010101010101010101010101010101010101010101010101010101010011100000000000000000000000000000000000000000000000000000000000000100000006465706f7369742d6d6574616461746113121212121212121212121212121212121212121212121212121212121212121201130000000000000000000000000000000000000000000000000000000000000014141414141414141414141414141414141414141414141414141414141414141401150000000000000000000000000000000000000000000000000000000000000001160000000000000017000000000000000000000000000000000000000000000000000000000000002000010b00047f00000191020bb8cd0353470962558a6e0839022ae65c6b2723b32772e5c0c5f4776cb8e6a3e10ba2f319000000000000000000000000000000000000000000000000000000000000001a00000000000000000000000000000000000000000000000000000000000000211b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1d000000000000001c00000000000000000000000000000000000000000000000000000000000000221e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1f0000000000000001010a0000008a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c02020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202030303030303030303030303030303030303030303030303030303030303030330200000000000000000000000000000000000000000000000000000000000000021000000000000000000000000000000000000000000000000000000000000002200000000000000000000000000000000000000000000000000000000000000";
const ONE_OF_EACH_TX_HASH: &str = "11e6013847824badf33aa383cfbdb4b5b74a621acefc8296c21f48c4072e0e92";

fn op_id(payload: &[u8]) -> String {
    let mut preimage = b"OPERATION_ID_V1".to_vec();
    preimage.extend_from_slice(payload);
    hex::encode(<[u8; 32]>::from(Hasher::digest(&preimage)))
}

/// The `SDP_ACTIVE` vector's fields: declaration id `0x1e`×32, nonce 31, Blend
/// activity metadata with epoch 10, signing key `0x8a88…6f5c`, proof of quota
/// `0x02`×160 and proof of selection `0x03`×32. `ActiveMessage::sample()` is the
/// operation the vector was generated from; its fields are checked one by one
/// so this does not lean on the sample staying put.
fn vector_operation() -> SDPActiveOp {
    let operation = SDPActiveOp::sample();
    assert_eq!(operation.declaration_id.0, [0x1e; 32]);
    assert_eq!(operation.nonce, 0x1f);
    let ActivityMetadata::Blend(proof) = &operation.metadata;
    assert_eq!(proof.epoch, Epoch::new(10));
    assert_eq!(
        hex::encode(proof.signing_key.as_bytes()),
        "8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c"
    );
    assert_eq!(hex::encode(proof.proof_of_quota.encode()), "02".repeat(160));
    assert_eq!(hex::encode(proof.proof_of_selection.encode()), "03".repeat(32));
    operation
}

#[test]
fn sdp_active_operation_encodes_to_the_appendix_payload() {
    let op = Op::SDPActive(vector_operation());
    let encoded = op.encode();
    let payload = &encoded[1..];

    assert_eq!(
        hex::encode(payload),
        SDP_ACTIVE_PAYLOAD,
        "the code encodes the SDP_ACTIVE vector's operation in {} bytes, the Appendix payload is \
         {} bytes: the spec writes 0x01 0x01 (metadata type, then the Blend activity proof \
         version) between the nonce and the epoch, the code writes a single 0x01; op_id {} vs \
         Appendix {SDP_ACTIVE_OP_ID}",
        payload.len(),
        SDP_ACTIVE_PAYLOAD.len() / 2,
        op_id(payload),
    );
    assert_eq!(op_id(payload), SDP_ACTIVE_OP_ID);
}

#[test]
fn sdp_active_appendix_payload_decodes_and_hashes_to_its_op_id() {
    let mut bytes = vec![SDP_ACTIVE_OPCODE];
    bytes.extend(hex::decode(SDP_ACTIVE_PAYLOAD).unwrap());

    let (rest, op) = Op::decode(&bytes, &()).unwrap_or_else(|error| {
        panic!("the code rejects the Appendix SDP_ACTIVE payload: {error:?}")
    });

    assert!(rest.is_empty(), "{} trailing bytes", rest.len());
    let encoded = op.encode();
    assert_eq!(hex::encode(&encoded[1..]), SDP_ACTIVE_PAYLOAD);
    assert_eq!(op_id(&encoded[1..]), SDP_ACTIVE_OP_ID);
}

#[test]
fn one_of_each_operation_transaction_decodes_and_hashes_to_the_appendix_hash() {
    let bytes = hex::decode(ONE_OF_EACH_TX).unwrap();

    let (rest, ops) = Ops::decode(&bytes, &()).unwrap_or_else(|error| {
        panic!("the code rejects the Appendix 'one of each operation' transaction: {error:?}")
    });

    assert!(rest.is_empty(), "{} trailing bytes", rest.len());
    assert_eq!(ops.len(), 10);
    assert_eq!(hex::encode(ops.hash().0), ONE_OF_EACH_TX_HASH);
}
