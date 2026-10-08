//! The libp2p protocol and gossipsub topic names of a deployment, derived from
//! it rather than configured.
//!
//! A protocol whose messages depend on the rules in force is bound to a fork:
//! its name carries the fork digest, so nodes on different forks neither talk
//! to each other nor share topics. Kademlia and identify only find and describe
//! peers of the same chain, so they are bound to the chain: their names carry
//! the chain ID instead, and peer discovery keeps working across the forks of a
//! chain.
use lb_core::{era::ForkDigest, mantle::transactions::genesis_tx::ChainId};
use lb_libp2p::protocol_name::StreamProtocol;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

/// The prefix of every name.
const NAMESPACE: &str = "/logos-blockchain";

/// Every character but RFC 3986's unreserved ones, which percent-encoding
/// leaves as they are: letters, digits, `-`, `.`, `_` and `~`.
const RESERVED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// What the name of a protocol or a topic is bound to.
#[derive(Clone, Copy, Debug)]
pub enum ProtocolScope<'chain> {
    /// The chain with this ID, on every fork of it.
    Chain(&'chain ChainId),
    /// The fork with this digest.
    Fork(ForkDigest),
}

impl ProtocolScope<'_> {
    /// The name of `protocol` in this scope:
    /// `/logos-blockchain/<scope>/<protocol>`, the scope being the
    /// percent-encoded chain ID or the fork digest in hex.
    #[must_use]
    pub fn to_string_with_name(self, name: &str) -> String {
        let scope = match self {
            Self::Chain(chain_id) => {
                utf8_percent_encode(AsRef::<str>::as_ref(chain_id), RESERVED).to_string()
            }
            Self::Fork(fork_digest) => hex::encode(<[u8; 32]>::from(fork_digest)),
        };
        format!("{NAMESPACE}/{scope}/{name}")
    }

    /// The stream protocol `protocol` in this scope, named by [`Self::name`].
    #[must_use]
    pub fn to_stream_protocol_with_name(self, name: &str) -> StreamProtocol {
        libp2p::StreamProtocol::try_from_owned(self.to_string_with_name(name))
            .unwrap()
            .into()
    }
}

#[cfg(test)]
mod tests {
    use lb_core::{era::ForkDigest, mantle::transactions::genesis_tx::ChainId};

    use super::ProtocolScope;

    fn chain_id(chain_id: &str) -> ChainId {
        ChainId::try_from(chain_id.to_owned()).unwrap()
    }

    #[test]
    fn a_name_carries_the_fork_digest_or_the_chain_id_it_is_bound_to() {
        let fork = ProtocolScope::Fork(ForkDigest::from([0xab; 32]));
        assert_eq!(
            fork.to_string_with_name("mempool"),
            format!("/logos-blockchain/{}/mempool", "ab".repeat(32))
        );
        assert_eq!(
            fork.to_stream_protocol_with_name("blend").as_ref(),
            format!("/logos-blockchain/{}/blend", "ab".repeat(32))
        );
        let chain_id = chain_id("logos-testnet");
        assert_eq!(
            ProtocolScope::Chain(&chain_id)
                .to_stream_protocol_with_name("kad")
                .as_ref(),
            "/logos-blockchain/logos-testnet/kad"
        );
    }

    #[test]
    fn a_chain_id_is_percent_encoded_but_for_unreserved_characters() {
        let chain_id = chain_id("0.3.0-rc.5_x~ a/b,c=d \u{e9}");
        assert_eq!(
            ProtocolScope::Chain(&chain_id).to_string_with_name("kad"),
            "/logos-blockchain/0.3.0-rc.5_x~%20a%2Fb%2Cc%3Dd%20%C3%A9/kad"
        );
    }
}
