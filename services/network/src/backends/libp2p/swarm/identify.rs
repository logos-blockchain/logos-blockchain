use lb_libp2p::{Multiaddr, Protocol, libp2p::identify};
use lb_log_targets::network_service;
use rand::RngCore;

use crate::backends::libp2p::swarm::{PeerProtocolState, ProtocolContract, SwarmHandler};

const LOG_TARGET: &str = network_service::backends::libp2p::IDENTIFY;

impl<R: Clone + Send + RngCore + 'static> SwarmHandler<R> {
    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: address this in a dedicated refactor"
    )]
    pub(super) fn handle_identify_event(&mut self, event: identify::Event) {
        match event {
            identify::Event::Received { peer_id, info, .. } => {
                tracing::trace!(
                    target: LOG_TARGET,
                    "Identified peer {} with addresses {:?}",
                    peer_id,
                    info.listen_addrs
                );

                let (network_matches, supports_kademlia, supports_chainsync) =
                    protocol_capabilities(
                        &info.protocol_version,
                        &info.protocols,
                        &self.protocol_contract,
                    );
                tracing::debug!(
                    target: LOG_TARGET,
                    peer = %peer_id,
                    protocol_version = %info.protocol_version,
                    network_matches,
                    supports_kademlia,
                    supports_chainsync,
                    protocols = ?info.protocols,
                    "Classified peer protocol capabilities"
                );

                let state = classify_peer(&info.protocols, &self.protocol_contract);
                self.peer_protocol_states.insert(peer_id, state);
                let probe_addresses = self.clear_identity_probe(peer_id);

                if supports_kademlia {
                    tracing::trace!(
                        target: LOG_TARGET,
                        "Adding discovered node to Kademlia, seen addresses: {:?}",
                        info.listen_addrs
                    );
                    // we need to add the peer to the kademlia routing table
                    // in order to enable peer discovery
                    // These addresses were already accepted by Kademlia. Keep
                    // them even when they are private or otherwise unsuitable
                    // as newly advertised Identify addresses.
                    for addr in &probe_addresses {
                        self.swarm.kademlia_add_address(peer_id, addr);
                    }

                    for addr in &info.listen_addrs {
                        if !is_kademlia_candidate_address(
                            addr,
                            self.allow_non_public_identify_addresses,
                        ) {
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

                if state == PeerProtocolState::Unsupported {
                    tracing::debug!(
                        target: LOG_TARGET,
                        "Peer {peer_id} is not chainsync eligible because it does not advertise the \
                        configured chainsync protocol"
                    );
                }
            }
            identify::Event::Error { peer_id, .. } => {
                // An Identify failure leaves protocol support unknown. Keep the
                // last confirmed capability state, if any, but allow a future
                // routing update to schedule another probe.
                self.clear_identity_probe(peer_id);
            }
            event => {
                tracing::trace!(target: LOG_TARGET, "Identify event: {:?}", event);
            }
        }
    }
}

fn classify_peer(
    protocols: &[lb_libp2p::libp2p::StreamProtocol],
    contract: &ProtocolContract,
) -> PeerProtocolState {
    let supports_chainsync = protocols
        .iter()
        .any(|protocol| protocol.as_ref() == contract.chain_sync_protocol.as_str());

    if supports_chainsync {
        PeerProtocolState::Supported
    } else {
        PeerProtocolState::Unsupported
    }
}

fn protocol_capabilities(
    protocol_version: &str,
    protocols: &[lb_libp2p::libp2p::StreamProtocol],
    contract: &ProtocolContract,
) -> (bool, bool, bool) {
    let network_matches = protocol_version == contract.identify_protocol_version;
    let supports_kademlia = protocols
        .iter()
        .any(|protocol| protocol.as_ref() == contract.kademlia_protocol.as_str());
    let supports_chainsync = protocols
        .iter()
        .any(|protocol| protocol.as_ref() == contract.chain_sync_protocol.as_str());

    (network_matches, supports_kademlia, supports_chainsync)
}

fn is_kademlia_candidate_address(
    addr: &Multiaddr,
    allow_non_public_identify_addresses: bool,
) -> bool {
    if allow_non_public_identify_addresses {
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

#[cfg(test)]
mod tests {
    use lb_libp2p::libp2p::StreamProtocol;

    use super::*;

    fn contract() -> ProtocolContract {
        ProtocolContract {
            identify_protocol_version: "/network/1.0.0".into(),
            kademlia_protocol: "/network/kad/1.0.0".into(),
            chain_sync_protocol: "/network/chainsync/1.0.0".into(),
        }
    }

    fn classify(_protocol_version: &str, protocols: &[&'static str]) -> PeerProtocolState {
        let protocols = protocols
            .iter()
            .map(|protocol| StreamProtocol::new(protocol))
            .collect::<Vec<_>>();
        classify_peer(&protocols, &contract())
    }

    #[test]
    fn protocol_contract_classification() {
        let cases = [
            (
                "exact contract",
                "/network/1.0.0",
                vec!["/network/kad/1.0.0", "/network/chainsync/1.0.0"],
                PeerProtocolState::Supported,
            ),
            (
                "network version mismatch",
                "/other-network/1.0.0",
                vec!["/network/kad/1.0.0", "/network/chainsync/1.0.0"],
                PeerProtocolState::Supported,
            ),
            (
                "different Kademlia protocol",
                "/network/1.0.0",
                vec!["/other-network/kad/1.0.0", "/network/chainsync/1.0.0"],
                PeerProtocolState::Supported,
            ),
            (
                "missing Kademlia protocol",
                "/network/1.0.0",
                vec!["/network/chainsync/1.0.0"],
                PeerProtocolState::Supported,
            ),
            (
                "different chainsync protocol",
                "/network/1.0.0",
                vec!["/network/kad/1.0.0", "/network/chainsync/0.9.0"],
                PeerProtocolState::Unsupported,
            ),
            (
                "missing chainsync protocol",
                "/network/1.0.0",
                vec!["/network/kad/1.0.0"],
                PeerProtocolState::Unsupported,
            ),
        ];

        for (name, protocol_version, protocols, expected) in cases {
            assert_eq!(classify(protocol_version, &protocols), expected, "{name}");
        }
    }

    #[test]
    fn non_public_identify_address_admission_is_configurable() {
        let loopback = "/ip4/127.0.0.1/udp/3000/quic-v1".parse().unwrap();
        let public = "/ip4/8.8.8.8/udp/3000/quic-v1".parse().unwrap();

        assert!(!is_kademlia_candidate_address(&loopback, false));
        assert!(is_kademlia_candidate_address(&loopback, true));
        assert!(is_kademlia_candidate_address(&public, false));
    }
}
