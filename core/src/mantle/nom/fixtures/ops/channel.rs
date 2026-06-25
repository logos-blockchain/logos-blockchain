use lb_core_macros::nom_wire_fixtures;
use lb_key_management_system_keys::keys::Ed25519PublicKey;

use crate::mantle::{
    channel::{SlotTimeframe, SlotTimeout},
    ledger::{Inputs, Outputs},
    ops::{
        Op,
        channel::{
            ChannelId, MsgId,
            config::ChannelConfigOp,
            deposit::{DepositOp, Metadata},
            inscribe::InscriptionOp,
            withdraw::ChannelWithdrawOp,
        },
    },
};

nom_wire_fixtures!(ChannelId, ChannelId::from([0u8; 32]) => "0000000000000000000000000000000000000000000000000000000000000000");
nom_wire_fixtures!(MsgId, MsgId::from([0u8; 32]) => "0000000000000000000000000000000000000000000000000000000000000000");
nom_wire_fixtures!(
    ChannelConfigOp,
    Self {
        channel: ChannelId::from([0u8; 32]),
        keys: [Ed25519PublicKey::from_bytes(&[0u8; _]).unwrap()].into(),
        posting_timeframe: SlotTimeframe::from(0u32),
        posting_timeout: SlotTimeout::from(0u32),
        configuration_threshold: 0u16,
        withdraw_threshold: 0u16,
    } => "000000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
);
nom_wire_fixtures!(
    DepositOp,
    Self {
        channel_id: ChannelId::from([0u8; 32]),
        inputs: Inputs::empty(),
        metadata: Metadata::empty(),
    } => "00000000000000000000000000000000000000000000000000000000000000000000000000",
);
nom_wire_fixtures!(
    InscriptionOp,
    Self {
        channel_id: ChannelId::from([0u8; 32]),
        inscription: b"genesis".into(),
        parent: MsgId::from([0u8; 32]),
        signer: Ed25519PublicKey::from_bytes(&[0u8; _]).unwrap(),
    } => "00000000000000000000000000000000000000000000000000000000000000000700000067656e6573697300000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
);
nom_wire_fixtures!(
    ChannelWithdrawOp,
    Self {
        channel_id: ChannelId::from([0u8; 32]),
        outputs: Outputs::empty(),
        withdraw_nonce: 0u32,
    } => "00000000000000000000000000000000000000000000000000000000000000000000000000",
);

// We just check that the enum discriminant tag is encoded correctly, so a
// single fixture is fine here.
nom_wire_fixtures!(
    Op,
    Self::ChannelInscribe(
        InscriptionOp::fixtures()
            .into_iter()
            .next()
            .expect("InscriptionOp has a fixture")
            .value
    ) => "1100000000000000000000000000000000000000000000000000000000000000000700000067656e6573697300000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
);
