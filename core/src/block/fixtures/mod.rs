use lb_codec::codec_fixtures;
use lb_cryptarchia_engine::Slot;
use lb_groth16::Fr;
use lb_key_management_system_keys::keys::{Ed25519PublicKey, Ed25519Signature};

use crate::{
    block::{BlockTransactionReferences, Proposal, References},
    header::{ContentId, Header, HeaderId},
    mantle::{TxHash, ops::leader_claim::VoucherCm},
    proofs::leader_proof::Groth16LeaderProof,
};

/// The three real references every reference fixture below is built from.
fn three_references() -> BlockTransactionReferences {
    [
        TxHash([0x01u8; 32]),
        TxHash([0x02u8; 32]),
        TxHash([0x03u8; 32]),
    ]
    .into()
}

fn header() -> Header {
    Header::new(
        HeaderId::from([0x11u8; 32]),
        ContentId::from([0x22u8; 32]),
        Slot::from(42u64),
        Groth16LeaderProof::from_parts(
            lb_pol::PoLProof::from_bytes(&[0x22u8; _]),
            Fr::from(0x5555u64),
            Ed25519PublicKey::from_bytes(&[0x33u8; _]).expect("valid key bytes"),
            VoucherCm::from(Fr::from(0x4444u64)),
        ),
    )
}

codec_fixtures!(
    References,
    Self { mempool_transactions: three_references() } => include_str!("references.hex")
);

// `header (299B) || references (32768B) || signature (64B)` — 33131 bytes,
// constant regardless of how many references are real.
codec_fixtures!(
    Proposal,
    Self {
        header: header(),
        references: References { mempool_transactions: three_references() },
        signature: Ed25519Signature::from_bytes(&[0x01u8; _]),
    } => include_str!("proposal.hex")
);
