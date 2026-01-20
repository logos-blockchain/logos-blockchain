pub mod libp2p;
use std::{pin::Pin, time::Duration};

use futures::Stream;
use lb_core::{da::BlobId, mantle::SignedMantleTx, sdp::SessionNumber};
use lb_da_network_core::SubnetworkId;
use lb_kzgrs_backend::common::share::DaShare;
use overwatch::{
    DynError,
    services::{ServiceData, relay::OutboundRelay},
};

#[async_trait::async_trait]
pub trait DispersalNetworkAdapter {
    type NetworkService: ServiceData;
    type SubnetworkId;
    fn new(outbound_relay: OutboundRelay<<Self::NetworkService as ServiceData>::Message>) -> Self;

    async fn disperse_share(
        &self,
        subnetwork_id: Self::SubnetworkId,
        da_share: DaShare,
    ) -> Result<(), DynError>;

    async fn disperse_tx(
        &self,
        subnetwork_id: Self::SubnetworkId,
        tx: SignedMantleTx,
    ) -> Result<(), DynError>;

    async fn dispersal_events_stream(
        &self,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<(BlobId, Self::SubnetworkId), DynError>> + Send>>,
        DynError,
    >;

    async fn get_blob_samples(
        &self,
        blob_id: BlobId,
        session: SessionNumber,
        subnets: &[SubnetworkId],
        cooldown: Duration,
    ) -> Result<(), DynError>;
}
