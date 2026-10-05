//! The libp2p protocol and gossipsub topic names of a deployment, derived from
//! it rather than configured.
//!
//! The names of the protocols whose messages depend on the rules in force carry
//! the fork digest, so nodes on different forks neither talk to each other nor
//! share topics. Kademlia and identify only find and describe peers of the same
//! chain, so their names carry the chain ID instead, and peer discovery keeps
//! working across the forks of a chain.
use lb_core::{era::ForkDigest, mantle::transactions::genesis_tx::ChainId};
use lb_libp2p::protocol_name::StreamProtocol;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

/// Every character but RFC 3986's unreserved ones, which percent-encoding
/// leaves as they are: letters, digits, `-`, `.`, `_` and `~`.
const RESERVED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// The protocol and topic names of a chain's fork.
#[derive(Debug, Clone)]
pub struct ProtocolNames {
    /// The stream protocol of Blend's core and edge connections.
    pub blend: StreamProtocol,
    /// The stream protocol of chain sync.
    pub chain_sync: StreamProtocol,
    /// The Kademlia stream protocol.
    pub kademlia: StreamProtocol,
    /// The protocol version identify advertises to peers.
    pub identify: StreamProtocol,
    /// The gossipsub topic of block proposals, which Blend also broadcasts on.
    pub cryptarchia_topic: String,
    /// The gossipsub topic of transactions.
    pub mempool_topic: String,
}

impl ProtocolNames {
    /// The prefix of every name.
    pub const NAMESPACE: &str = "/logos-blockchain";

    /// The names of the chain `chain_id`, on the fork named `fork_digest`:
    /// `NAMESPACE/<fork digest in hex>/<protocol>` for the fork's protocols,
    /// and `NAMESPACE/<percent-encoded chain ID>/<protocol>` for Kademlia and
    /// identify.
    #[must_use]
    pub fn derive(chain_id: &ChainId, fork_digest: ForkDigest) -> Self {
        let fork = hex::encode(<[u8; 32]>::from(fork_digest));
        let chain = utf8_percent_encode(AsRef::<str>::as_ref(chain_id), RESERVED).to_string();
        Self {
            blend: stream_protocol(&fork, "blend"),
            chain_sync: stream_protocol(&fork, "chainsync"),
            kademlia: stream_protocol(&chain, "kad"),
            identify: stream_protocol(&chain, "identify"),
            cryptarchia_topic: gossipsub_name(&fork, "cryptarchia"),
            mempool_topic: gossipsub_name(&fork, "mempool"),
        }
    }
}

fn gossipsub_name(scope: &str, protocol: &str) -> String {
    format!("{}/{scope}/{protocol}", ProtocolNames::NAMESPACE)
}

fn stream_protocol(scope: &str, protocol: &str) -> StreamProtocol {
    libp2p::StreamProtocol::try_from_owned(format!(
        "{}/{scope}/{protocol}",
        ProtocolNames::NAMESPACE
    ))
    .unwrap()
    .into()
}

#[cfg(test)]
mod tests {
    use lb_core::{era::ForkDigest, mantle::transactions::genesis_tx::ChainId};

    use super::ProtocolNames;

    fn chain_id(chain_id: &str) -> ChainId {
        ChainId::try_from(chain_id.to_owned()).unwrap()
    }

    #[test]
    fn fork_protocols_carry_the_fork_digest_and_discovery_the_chain_id() {
        let names = ProtocolNames::derive(&chain_id("logos-testnet"), ForkDigest::from([0xab; 32]));
        let fork = "ab".repeat(32);
        assert_eq!(
            names.blend.as_ref(),
            format!("/logos-blockchain/{fork}/blend")
        );
        assert_eq!(
            names.chain_sync.as_ref(),
            format!("/logos-blockchain/{fork}/chainsync")
        );
        assert_eq!(
            names.cryptarchia_topic,
            format!("/logos-blockchain/{fork}/cryptarchia")
        );
        assert_eq!(
            names.mempool_topic,
            format!("/logos-blockchain/{fork}/mempool")
        );
        assert_eq!(
            names.kademlia.as_ref(),
            "/logos-blockchain/logos-testnet/kad"
        );
        assert_eq!(
            names.identify.as_ref(),
            "/logos-blockchain/logos-testnet/identify"
        );
    }

    #[test]
    fn a_chain_id_is_percent_encoded_but_for_unreserved_characters() {
        let names = ProtocolNames::derive(
            &chain_id("0.3.0-rc.5_x~ a/b,c=d \u{e9}"),
            ForkDigest::from([0; 32]),
        );
        assert_eq!(
            names.kademlia.as_ref(),
            "/logos-blockchain/0.3.0-rc.5_x~%20a%2Fb%2Cc%3Dd%20%C3%A9/kad"
        );
    }

    #[test]
    fn a_new_fork_changes_the_fork_protocols_but_not_discovery() {
        let chain_id = chain_id("logos-testnet");
        let before = ProtocolNames::derive(&chain_id, ForkDigest::from([1; 32]));
        let after = ProtocolNames::derive(&chain_id, ForkDigest::from([2; 32]));
        assert_ne!(before.blend.as_ref(), after.blend.as_ref());
        assert_ne!(before.chain_sync.as_ref(), after.chain_sync.as_ref());
        assert_ne!(before.cryptarchia_topic, after.cryptarchia_topic);
        assert_ne!(before.mempool_topic, after.mempool_topic);
        assert_eq!(before.kademlia.as_ref(), after.kademlia.as_ref());
        assert_eq!(before.identify.as_ref(), after.identify.as_ref());
    }
}
