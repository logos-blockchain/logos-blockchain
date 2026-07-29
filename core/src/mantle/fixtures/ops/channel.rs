use lb_key_management_system_keys::keys::{Ed25519PublicKey, Ed25519Signature};
use lb_wire::wire_fixtures;

use crate::{
    mantle::{
        channel::{SlotTimeframe, SlotTimeout},
        ledger::{Inputs, Outputs},
        ops::{
            Op,
            channel::{
                ChannelId, MsgId,
                channel_transfer::ChannelTransferOp,
                config::ChannelConfigOp,
                deposit::{DepositOp, Metadata},
                inscribe::InscriptionOp,
                withdraw::ChannelWithdrawOp,
            },
        },
    },
    proofs::channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignature},
};

wire_fixtures!(ChannelId, Self::from([0u8; 32]) => "0000000000000000000000000000000000000000000000000000000000000000");
wire_fixtures!(MsgId, Self::from([0u8; 32]) => "0000000000000000000000000000000000000000000000000000000000000000");
wire_fixtures!(
    ChannelConfigOp,
    Self {
        channel: ChannelId::from([0u8; 32]),
        keys: [Ed25519PublicKey::from_bytes(&[0u8; _]).unwrap()].into(),
        posting_timeframe: SlotTimeframe::from(0u32),
        posting_timeout: SlotTimeout::from(0u32),
        configuration_threshold: 0u16,
        transfer_threshold: 0u16,
    } => "000000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
);
wire_fixtures!(
    DepositOp,
    Self {
        channel_id: ChannelId::from([0u8; 32]),
        inputs: Inputs::empty(),
        metadata: Metadata::empty(),
    } => "00000000000000000000000000000000000000000000000000000000000000000000000000",
);
wire_fixtures!(
    InscriptionOp,
    Self {
        channel_id: ChannelId::from([0u8; 32]),
        inscription: b"genesis".into(),
        parent: MsgId::from([0u8; 32]),
        signer: Ed25519PublicKey::from_bytes(&[0u8; _]).unwrap(),
    } => "00000000000000000000000000000000000000000000000000000000000000000700000067656e6573697300000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
);
wire_fixtures!(
    ChannelWithdrawOp,
    Self {
        channel_id: ChannelId::from([0u8; 32]),
        inputs: Inputs::empty(),
    } => "000000000000000000000000000000000000000000000000000000000000000000",
);
wire_fixtures!(
    ChannelTransferOp,
    Self {
        channel_id: ChannelId::from([0u8; 32]),
        inputs: Inputs::empty(),
        outputs: Outputs::empty(),
    } => "00000000000000000000000000000000000000000000000000000000000000000000",
);

// We just check that the enum discriminant tag is encoded correctly, so a
// single fixture is fine here.
wire_fixtures!(
    Op,
    Self::ChannelInscribe(
        InscriptionOp::fixtures()
            .into_iter()
            .next()
            .expect("InscriptionOp has a fixture")
            .value
    ) => "1100000000000000000000000000000000000000000000000000000000000000000700000067656e6573697300000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
);

wire_fixtures!(
    IndexedSignature,
    Self {
        channel_key_index: 1,
        signature: Ed25519Signature::from_bytes(&[0u8; _])
    } => "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000100"
);

wire_fixtures!(ChannelMultiSigProof,
    Self::try_new([].into()).unwrap() => "0000",
    Self::try_new([IndexedSignature::new(0, Ed25519Signature::from_bytes(&[0u8; _]))].into()).unwrap() => "0100000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
);
