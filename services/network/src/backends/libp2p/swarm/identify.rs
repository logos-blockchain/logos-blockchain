use std::collections::HashSet;

use lb_libp2p::{Multiaddr, Protocol, libp2p::identify};
use rand::RngCore;

use crate::backends::libp2p::swarm::SwarmHandler;

impl<R: Clone + Send + RngCore + 'static> SwarmHandler<R> {
    pub(super) fn handle_identify_event(&mut self, event: identify::Event) {
        match event {
            identify::Event::Received { peer_id, info, .. } => {
                tracing::debug!(
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
                    tracing::debug!(
                        "Adding discovered node to Kademlia, seen addresses: {:?}",
                        info.listen_addrs
                    );
                    // we need to add the peer to the kademlia routing table
                    // in order to enable peer discovery
                    for addr in &info.listen_addrs {
                        if !is_kademlia_candidate_address(addr) {
                            tracing::debug!(
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
                tracing::debug!("Identify event: {:?}", event);
            }
        }
    }
}

fn is_kademlia_candidate_address(addr: &Multiaddr) -> bool {
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

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use lb_libp2p::Multiaddr;

    use super::is_kademlia_candidate_address;

    #[test]
    fn filters_non_routable_ipv4_addresses() {
        let loopback = Multiaddr::from_str("/ip4/127.0.0.1/udp/1234/quic-v1").unwrap();
        let private_192 = Multiaddr::from_str("/ip4/192.168.64.1/udp/1234/quic-v1").unwrap();
        let private_10 = Multiaddr::from_str("/ip4/10.7.3.131/udp/1234/quic-v1").unwrap();
        let public = Multiaddr::from_str("/ip4/8.8.8.8/udp/1234/quic-v1").unwrap();

        assert!(!is_kademlia_candidate_address(&loopback));
        assert!(!is_kademlia_candidate_address(&private_192));
        assert!(!is_kademlia_candidate_address(&private_10));
        assert!(is_kademlia_candidate_address(&public));
    }
}
