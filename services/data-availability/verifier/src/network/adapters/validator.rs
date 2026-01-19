use std::{fmt::Debug, marker::PhantomData};

use futures::Stream;
use lb_core::{da::BlobId, mantle::SignedMantleTx};
use lb_da_network_core::SubnetworkId;
use lb_da_network_service::{
    NetworkService,
    api::ApiAdapter as ApiAdapterTrait,
    backends::libp2p::{
        common::VerificationEvent,
        validator::{DaNetworkEvent, DaNetworkEventKind, DaNetworkValidatorBackend},
    },
    membership::{MembershipAdapter, handler::DaMembershipHandler},
    sdp::SdpAdapter as SdpAdapterTrait,
};
use lb_kzgrs_backend::common::share::{DaShare, DaSharesCommitments};
use lb_subnetworks_assignations::MembershipHandler;
use libp2p::PeerId;
use overwatch::services::{ServiceData, relay::OutboundRelay};
use tokio_stream::StreamExt as _;

use crate::network::{NetworkAdapter, ValidationRequest, adapters::common::adapter_for};

adapter_for!(
    DaNetworkValidatorBackend,
    DaNetworkEventKind,
    DaNetworkEvent
);
