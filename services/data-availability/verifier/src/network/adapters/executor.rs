use std::{fmt::Debug, marker::PhantomData};

use futures::Stream;
use kzgrs_backend::common::share::{DaShare, DaSharesCommitments};
use libp2p::PeerId;
use logos_blockchain_core::{da::BlobId, mantle::SignedMantleTx};
use logos_blockchain_da_network_core::SubnetworkId;
use logos_blockchain_da_network_service::{
    NetworkService,
    api::ApiAdapter as ApiAdapterTrait,
    backends::libp2p::{
        common::VerificationEvent,
        executor::{DaNetworkEvent, DaNetworkEventKind, DaNetworkExecutorBackend},
    },
    membership::{MembershipAdapter, handler::DaMembershipHandler},
    sdp::SdpAdapter as SdpAdapterTrait,
};
use overwatch::services::{ServiceData, relay::OutboundRelay};
use subnetworks_assignations::MembershipHandler;
use tokio_stream::StreamExt as _;

use crate::network::{NetworkAdapter, ValidationRequest, adapters::common::adapter_for};

adapter_for!(DaNetworkExecutorBackend, DaNetworkEventKind, DaNetworkEvent);
