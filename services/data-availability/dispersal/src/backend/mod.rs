pub mod kzgrs;

use std::pin::Pin;

use lb_core::{
    da::{DaDispersal, DaEncoder},
    mantle::{
        SignedMantleTx,
        ops::channel::{ChannelId, Ed25519PublicKey, MsgId},
        tx_builder::MantleTxBuilder,
    },
    sdp::SessionNumber,
};
use overwatch::DynError;
use tokio::sync::oneshot;

use crate::adapters::{network::DispersalNetworkAdapter, wallet::DaWalletAdapter};

pub type DispersalTask = Pin<Box<dyn Future<Output = (ChannelId, Option<SignedMantleTx>)> + Send>>;

#[derive(Copy, Clone)]
pub struct InitialBlobOpArgs {
    pub channel_id: ChannelId,
    pub session: SessionNumber,
    pub parent_msg_id: MsgId,
    pub signer: Ed25519PublicKey,
}

#[async_trait::async_trait]
pub trait DispersalBackend {
    type Settings;
    type Encoder: DaEncoder;
    type Dispersal: DaDispersal<EncodedData = <Self::Encoder as DaEncoder>::EncodedData>;
    type NetworkAdapter: DispersalNetworkAdapter;
    type WalletAdapter: DaWalletAdapter;
    type BlobId: AsRef<[u8]> + Send + Copy;

    fn init(
        config: Self::Settings,
        network_adapter: Self::NetworkAdapter,
        wallet_adapter: Self::WalletAdapter,
    ) -> Self;

    async fn process_dispersal(
        &self,
        tx_builder: MantleTxBuilder,
        blob_op_args: InitialBlobOpArgs,
        data: Vec<u8>,
        sender: oneshot::Sender<Result<Self::BlobId, DynError>>,
    ) -> Result<DispersalTask, DynError>;
}
