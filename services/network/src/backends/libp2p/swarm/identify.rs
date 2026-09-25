use std::collections::HashSet;

use lb_libp2p::{Multiaddr, PeerId, Protocol, libp2p::identify};
use lb_log_targets::network_service;
use rand::RngCore;

use crate::backends::libp2p::swarm::SwarmHandler;

const LOG_TARGET: &str = network_service::backends::libp2p::IDENTIFY;

impl<R: Clone + Send + RngCore + 'static> SwarmHandler<R> {
    pub(super) fn handle_identify_event(&mut self, event: identify::Event) {
        match event {
            identify::Event::Received { peer_id, info, .. } => {
                self.handle_identify_received(peer_id, info);
            }
            event => {
                tracing::trace!(target: LOG_TARGET, "Identify event: {:?}", event);
            }
        }
    }

    fn handle_identify_received(&mut self, peer_id: PeerId, info: identify::Info) {
        tracing::trace!(
            target: LOG_TARGET,
            "Identified peer {} with addresses {:?}",
            peer_id,
            info.listen_addrs
        );

        let advertised_protocols = info.protocols.into_iter().collect::<HashSet<_>>();
        let supports_kademlia =
            advertised_protocols.contains(&self.protocol_contract.kademlia_protocol);
        let supports_chainsync =
            advertised_protocols.contains(&self.protocol_contract.chain_sync_protocol);
        tracing::debug!(
            target: LOG_TARGET,
            peer = %peer_id,
            protocol_version = %info.protocol_version,
            supports_kademlia,
            supports_chainsync,
            protocols = ?advertised_protocols,
            "Classified peer protocol capabilities"
        );

        self.peer_advertised_protocols
            .insert(peer_id, advertised_protocols);

        if supports_kademlia {
            self.add_identified_kademlia_addresses(peer_id, &info.listen_addrs);
        }

        if !supports_chainsync {
            tracing::debug!(
                target: LOG_TARGET,
                "Peer {peer_id} is not chainsync eligible because it does not advertise the \
                configured chainsync protocol"
            );
        }
    }

    fn add_identified_kademlia_addresses(&mut self, peer_id: PeerId, listen_addrs: &[Multiaddr]) {
        tracing::trace!(
            target: LOG_TARGET,
            "Adding discovered node to Kademlia, seen addresses: {:?}",
            listen_addrs
        );
        // We need to add the peer to the Kademlia routing table in order to
        // enable peer discovery.
        for addr in listen_addrs {
            if !is_kademlia_candidate_address(addr) {
                tracing::trace!(
                    target: LOG_TARGET,
                    "Skipping non-routable identify address for Kademlia: {}",
                    addr
                );
                continue;
            }
            self.swarm.kademlia_add_address(peer_id, addr);
        }
    }
}

fn is_kademlia_candidate_address(addr: &Multiaddr) -> bool {
    // Tests run entirely on local/private interfaces; keep production
    // filtering enabled while allowing all identify addresses in test builds.
    let filter_identify_addrs = !cfg!(test);
    if !filter_identify_addrs {
        return true;
    }

    for protocol in addr {
        match protocol {
            Protocol::Ip4(ip) => {
                return !ip.is_loopback()
                    && !ip.is_private()
                    && !ip.is_unspecified()
                    && !ip.is_link_local();
            }
            Protocol::Ip6(ip) => {
                return !ip.is_loopback()
                    && !ip.is_unspecified()
                    && !ip.is_unique_local()
                    && !ip.is_unicast_link_local();
            }
            _ => {}
        }
    }

    true
}
