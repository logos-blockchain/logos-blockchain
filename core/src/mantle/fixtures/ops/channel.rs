use lb_codec::codec_fixtures;
use lb_key_management_system_keys::keys::{Ed25519PublicKey, Ed25519Signature};

use crate::{
    mantle::{
        channel::{SlotTimeframe, SlotTimeout},
        fixtures::ops::op_values::{
            CHANNEL_CONFIG, CHANNEL_CONFIG_PAYLOAD_HEX, CHANNEL_TRANSFER,
            CHANNEL_TRANSFER_PAYLOAD_HEX, CHANNEL_WITHDRAW, CHANNEL_WITHDRAW_PAYLOAD_HEX, DEPOSIT,
            DEPOSIT_PAYLOAD_HEX, INSCRIPTION, INSCRIPTION_PAYLOAD_HEX,
        },
        ledger::{Inputs, Outputs},
        ops::channel::{
            ChannelId, MsgId,
            channel_transfer::ChannelTransferOp,
            config::ChannelConfigOp,
            deposit::{DepositOp, Metadata},
            inscribe::InscriptionOp,
            withdraw::ChannelWithdrawOp,
        },
    },
    proofs::channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignature},
};

codec_fixtures!(ChannelId, Self::from([0u8; 32]) => "0000000000000000000000000000000000000000000000000000000000000000");
codec_fixtures!(MsgId, Self::from([0u8; 32]) => "0000000000000000000000000000000000000000000000000000000000000000");
codec_fixtures!(
    ChannelConfigOp,
    Self {
        channel: ChannelId::from([0u8; 32]),
        keys: [Ed25519PublicKey::from_bytes(&[0u8; _]).unwrap()].into(),
        posting_timeframe: SlotTimeframe::from(0u32),
        posting_timeout: SlotTimeout::from(0u32),
        configuration_threshold: 0u16,
        transfer_threshold: 0u16,
    } => "000000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    CHANNEL_CONFIG.clone() => CHANNEL_CONFIG_PAYLOAD_HEX,
);
codec_fixtures!(
    DepositOp,
    Self {
        channel_id: ChannelId::from([0u8; 32]),
        inputs: Inputs::empty(),
        metadata: Metadata::empty(),
    } => "00000000000000000000000000000000000000000000000000000000000000000000000000",
    DEPOSIT.clone() => DEPOSIT_PAYLOAD_HEX,
);
codec_fixtures!(
    InscriptionOp,
    Self {
        channel_id: ChannelId::from([0u8; 32]),
        inscription: b"genesis".into(),
        parent: MsgId::from([0u8; 32]),
        signer: Ed25519PublicKey::from_bytes(&[0u8; _]).unwrap(),
    } => "00000000000000000000000000000000000000000000000000000000000000000700000067656e6573697300000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    INSCRIPTION.clone() => INSCRIPTION_PAYLOAD_HEX,
);
codec_fixtures!(
    ChannelWithdrawOp,
    Self {
        channel_id: ChannelId::from([0u8; 32]),
        inputs: Inputs::empty(),
    } => "000000000000000000000000000000000000000000000000000000000000000000",
    CHANNEL_WITHDRAW.clone() => CHANNEL_WITHDRAW_PAYLOAD_HEX,
);
codec_fixtures!(
    ChannelTransferOp,
    Self {
        channel_id: ChannelId::from([0u8; 32]),
        inputs: Inputs::empty(),
        outputs: Outputs::empty(),
    } => "00000000000000000000000000000000000000000000000000000000000000000000",
    CHANNEL_TRANSFER.clone() => CHANNEL_TRANSFER_PAYLOAD_HEX,
);

codec_fixtures!(
    IndexedSignature,
    Self {
        channel_key_index: 1,
        signature: Ed25519Signature::from_bytes(&[0u8; _])
    } => "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000100"
);

codec_fixtures!(ChannelMultiSigProof,
    Self::try_new([].into()).unwrap() => "0000",
    Self::try_new([IndexedSignature::new(0, Ed25519Signature::from_bytes(&[0u8; _]))].into()).unwrap() => "0100000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
);
