use std::{collections::HashMap, fmt::Debug, pin::Pin};

use futures::{Stream, StreamExt as _};
use lb_core::{da::BlobId, header::HeaderId, sdp::SessionNumber};
use lb_da_network_core::SubnetworkId;
use lb_da_network_service::{
    DaNetworkMsg, NetworkService,
    api::ApiAdapter as ApiAdapterTrait,
    backends::libp2p::{
        common::{HistoricSamplingEvent, SamplingEvent},
        executor::{
            DaNetworkEvent, DaNetworkEventKind, DaNetworkExecutorBackend, ExecutorDaNetworkMessage,
        },
    },
    membership::{MembershipAdapter, handler::DaMembershipHandler},
    sdp::SdpAdapter as SdpAdapterTrait,
};
use lb_kzgrs_backend::common::share::{DaShare, DaSharesCommitments};
use lb_subnetworks_assignations::MembershipHandler;
use libp2p_identity::PeerId;
use overwatch::{
    DynError,
    services::{ServiceData, relay::OutboundRelay},
};
use tokio::sync::oneshot;

use crate::network::{CommitmentsEvent, NetworkAdapter, adapters::common::adapter_for};

adapter_for!(
    DaNetworkExecutorBackend,
    ExecutorDaNetworkMessage,
    DaNetworkEventKind,
    DaNetworkEvent
);
