use lb_core::{
    header::HeaderId,
    mantle::{
        Note, Value,
        gas::GasCost,
        notes::{NoteCommitment, ProgramId},
        ops::{
            ZkTransferProof,
            channel::{
                ChannelId,
                deposit::Metadata,
                inscribe::{Inscription, Step, serde_inscription},
            },
        },
        transactions::hash::TxHash,
    },
};
use lb_groth16::{Fr, serde::serde_fr};
use lb_key_management_system_keys::keys::ZkPublicKey;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct ChannelDepositRequestBody {
    pub tip: Option<HeaderId>,
    pub channel_id: ChannelId,
    pub amount: Value,
    /// The key the deposited note is paid to in the channel.
    pub holder_public_key: ZkPublicKey,
    pub metadata: Metadata,
    pub change_public_key: ZkPublicKey,
    pub funding_public_keys: Vec<ZkPublicKey>,
    pub max_tx_fee: GasCost,
}

#[derive(Serialize, Deserialize)]
pub struct ChannelDepositResponseBody {
    pub hash: TxHash,
    /// The note paid to the holder in the channel. A holder whose wallet is not
    /// this node's imports it to withdraw it.
    pub holder_note: Note,
}

/// A withdrawal of channel notes this node's wallet holds. Each is released
/// as a ledger note of the same value and key once the withdrawal is due.
#[derive(Serialize, Deserialize)]
pub struct ChannelWithdrawRequestBody {
    pub tip: Option<HeaderId>,
    pub channel_id: ChannelId,
    /// The channel notes to withdraw.
    pub notes: Vec<NoteCommitment>,
    pub change_public_key: ZkPublicKey,
    pub funding_public_keys: Vec<ZkPublicKey>,
    pub max_tx_fee: GasCost,
}

#[derive(Serialize, Deserialize)]
pub struct ChannelWithdrawResponseBody {
    pub hash: TxHash,
    /// The ledger notes the withdrawal releases, in the order of `notes`.
    pub outputs: Vec<Note>,
}

/// A user step, or a program deposit, built from the notes this node's wallet
/// holds in the channel.
#[derive(Serialize, Deserialize)]
pub struct ChannelStepRequestBody {
    pub tip: Option<HeaderId>,
    pub channel_id: ChannelId,
    /// The commitments the channel's buffer holds when the mini-block of the
    /// step starts, in order: the outputs of the earlier channel operations
    /// of the transaction and of the earlier mini-blocks of the inscription,
    /// as the sequencer fixed them. The step is proven at that position and
    /// may spend the ones the wallet knows the opening of. Leave it empty for
    /// a mini-block that starts on an empty buffer.
    #[serde(default)]
    pub appended: Vec<NoteCommitment>,
    /// The channel notes the step creates.
    pub payments: Vec<ChannelPayment>,
    /// Makes the step a program deposit.
    pub program_deposit: Option<ProgramDepositBody>,
    #[serde(with = "serde_inscription")]
    pub inscription: Inscription,
    pub change_public_key: ZkPublicKey,
    pub holder_public_keys: Vec<ZkPublicKey>,
}

#[derive(Serialize, Deserialize)]
pub struct ChannelPayment {
    pub public_key: ZkPublicKey,
    pub value: Value,
}

#[derive(Serialize, Deserialize)]
pub struct ProgramDepositBody {
    #[serde(with = "serde_fr")]
    pub program_id: ProgramId,
    pub value: Value,
    #[serde(with = "serde_fr")]
    pub intent: Fr,
}

/// The step and its proof, for the sequencer of the channel to inscribe in
/// a mini-block that starts behind the `appended` commitments of the request.
#[derive(Serialize, Deserialize)]
pub struct ChannelStepResponseBody {
    pub tip: HeaderId,
    pub step: Step,
    pub proof: ZkTransferProof,
    /// The openings of the notes the step creates, the change last. A payee
    /// whose wallet is not this node's imports its note to spend it.
    pub outputs: Vec<Note>,
}
