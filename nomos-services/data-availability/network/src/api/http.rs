use std::{fmt::Debug, net::IpAddr};

use async_trait::async_trait;
use common_http_client::CommonHttpClient;
use kzgrs_backend::common::share::{DaShare, DaSharesCommitments};
use libp2p_identity::PeerId;
use multiaddr::Multiaddr;
use nomos_core::da::BlobId;
use nomos_da_network_core::{SubnetworkId, addressbook::AddressBookHandler};
use rand::prelude::IteratorRandom as _;
use serde::{Deserialize, Serialize};
use subnetworks_assignations::MembershipHandler;
use tracing::error;
use url::Url;

use crate::api::ApiAdapter;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiAdapterSettings {
    pub api_port: u16,
    pub is_secure: bool,
}

#[derive(Clone)]
pub struct HttApiAdapter<Membership, Addressbook> {
    pub client: CommonHttpClient,
    pub membership: Membership,
    pub addressbook: Addressbook,
    pub api_port: u16,
    pub protocol: String,
}

impl<Membership, Addressbook> HttApiAdapter<Membership, Addressbook>
where
    Membership: MembershipHandler<NetworkId = SubnetworkId, Id = PeerId>
        + Clone
        + Debug
        + Send
        + Sync
        + 'static,
    Addressbook: AddressBookHandler<Id = PeerId> + Clone + Debug + Send + Sync + 'static,
{
    #[expect(dead_code, reason = "deprecated, will be removed soon")]
    fn random_peer_address(&self) -> Option<Url> {
        let peer_address = self
            .membership
            .members()
            .iter()
            .filter_map(|peer| self.addressbook.get_address(peer))
            .choose(&mut rand::thread_rng())?;

        let host = get_ip_from_multiaddr(&peer_address)?;
        match Url::parse(&format!("{}://{host}:{}", self.protocol, self.api_port)) {
            Ok(url) => Some(url),
            Err(e) => {
                error!("Failed to parse URL: {}", e);
                None
            }
        }
    }
}

#[async_trait]
impl<Membership, Addressbook> ApiAdapter for HttApiAdapter<Membership, Addressbook>
where
    Membership: MembershipHandler<NetworkId = SubnetworkId, Id = PeerId>
        + Clone
        + Debug
        + Send
        + Sync
        + 'static,
    Addressbook: AddressBookHandler<Id = PeerId> + Clone + Debug + Send + Sync + 'static,
{
    type Settings = ApiAdapterSettings;
    type Share = DaShare;
    type BlobId = BlobId;
    type Commitments = DaSharesCommitments;
    type Membership = Membership;
    type Addressbook = Addressbook;

    fn new(settings: Self::Settings, membership: Membership, addressbook: Addressbook) -> Self {
        Self {
            client: CommonHttpClient::new(None),
            membership,
            addressbook,
            api_port: settings.api_port,
            protocol: if settings.is_secure {
                "https".to_owned()
            } else {
                "http".to_owned()
            },
        }
    }
}

fn get_ip_from_multiaddr(multiaddr: &Multiaddr) -> Option<IpAddr> {
    multiaddr.iter().find_map(|protocol| match protocol {
        multiaddr::Protocol::Ip4(ipv4) => Some(IpAddr::V4(ipv4)),
        multiaddr::Protocol::Ip6(ipv6) => Some(IpAddr::V6(ipv6)),
        _ => None,
    })
}
