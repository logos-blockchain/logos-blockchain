pub mod mock;

use lb_core::{
    da::BlobId,
    mantle::{
        SignedMantleTx,
        ops::channel::{ChannelId, Ed25519PublicKey, MsgId},
        tx_builder::MantleTxBuilder,
    },
    sdp::SessionNumber,
};

use crate::backend::InitialBlobOpArgs;

#[derive(Clone)]
pub struct BlobOpArgs {
    pub channel_id: ChannelId,
    pub session: SessionNumber,
    pub parent_msg_id: MsgId,
    pub blob_id: BlobId,
    pub blob_size: usize,
    pub signer: Ed25519PublicKey,
}

impl BlobOpArgs {
    #[must_use]
    pub const fn from_initial(args: InitialBlobOpArgs, blob_id: BlobId, blob_size: usize) -> Self {
        Self {
            channel_id: args.channel_id,
            session: args.session,
            parent_msg_id: args.parent_msg_id,
            signer: args.signer,
            blob_id,
            blob_size,
        }
    }
}

#[async_trait::async_trait]
pub trait DaWalletAdapter {
    type Error;

    // TODO: Pass relay when wallet service is defined.
    fn new() -> Self;

    fn blob_tx(
        &self,
        tx_builder: MantleTxBuilder,
        blob_op_args: BlobOpArgs,
    ) -> Result<SignedMantleTx, Self::Error>;
}
