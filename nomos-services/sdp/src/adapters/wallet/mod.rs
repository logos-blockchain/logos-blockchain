pub mod mock;

use key_management_system_keys::keys::ZkPublicKey;
use nomos_core::{
    mantle::{NoteId, SignedMantleTx, tx_builder::MantleTxBuilder},
    sdp::{ActiveMessage, DeclarationMessage, WithdrawMessage},
};

#[async_trait::async_trait]
pub trait SdpWalletAdapter {
    type Error;

    // TODO: Pass relay when wallet service is defined.
    fn new() -> Self;

    fn declare_tx(
        &self,
        tx_builder: MantleTxBuilder,
        declaration: Box<DeclarationMessage>,
    ) -> Result<SignedMantleTx, Self::Error>;

    fn withdraw_tx(
        &self,
        tx_builder: MantleTxBuilder,
        withdrawn_message: WithdrawMessage,
        zk_id: ZkPublicKey,
        locked_note_id: NoteId,
    ) -> Result<SignedMantleTx, Self::Error>;

    fn active_tx(
        &self,
        tx_builder: MantleTxBuilder,
        active_message: ActiveMessage,
        zk_id: ZkPublicKey,
    ) -> Result<SignedMantleTx, Self::Error>;
}
