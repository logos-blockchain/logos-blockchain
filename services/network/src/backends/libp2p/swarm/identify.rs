use std::collections::HashSet;

use lb_libp2p::{Multiaddr, Protocol, libp2p::identify};
use rand::RngCore;

use crate::backends::libp2p::swarm::SwarmHandler;

impl<R: Clone + Send + RngCore + 'static> SwarmHandler<R> {
    pub(super) fn handle_identify_event(&mut self, event: identify::Event) {
        match event {
            identify::Event::Received { peer_id, info, .. } => {
                tracing::trace!(
                    "Identified peer {} with addresses {:?}",
                    peer_id,
                    info.listen_addrs
                );
                let kad_protocol_names = self
                    .swarm
                    .get_kademlia_protocol_names()
                    .collect::<HashSet<_>>();
                if info
                    .protocols
                    .iter()
                    .any(|p| kad_protocol_names.contains(&p))
                {
                    tracing::trace!(
                        "Adding discovered node to Kademlia, seen addresses: {:?}",
                        info.listen_addrs
                    );
                    // we need to add the peer to the kademlia routing table
                    // in order to enable peer discovery
                    for addr in &info.listen_addrs {
                        if !is_kademlia_candidate_address(addr) {
                            tracing::trace!(
                                "Skipping non-routable identify address for Kademlia: {}",
                                addr
                            );
                            continue;
                        }
                        self.swarm.kademlia_add_address(peer_id, addr);
                    }
                }
            }
            event => {
                tracing::trace!("Identify event: {:?}", event);
            }
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
