//! The libp2p protocol and gossipsub topic names of a deployment, derived from
//! it rather than configured.
//!
//! A protocol whose messages depend on the rules in force is bound to a fork:
//! its name carries the fork digest, so nodes on different forks neither talk
//! to each other nor share topics. Kademlia and identify only find and describe
//! peers of the same chain, so they are bound to the chain: their names carry
//! the genesis block ID instead, and peer discovery keeps working across the
//! forks of a chain.
use lb_core::{era::ForkDigest, header::HeaderId};
use lb_libp2p::protocol_name::StreamProtocol;

/// The prefix of every name.
const NAMESPACE: &str = "/logos-blockchain";

/// What the name of a protocol or a topic is bound to.
#[derive(Clone, Copy, Debug)]
pub enum ProtocolScope {
    /// The chain that starts from the genesis block with this ID, on every
    /// fork of it.
    Chain(HeaderId),
    /// The fork with this digest.
    Fork(ForkDigest),
}

impl ProtocolScope {
    /// The name of `protocol` in this scope:
    /// `/logos-blockchain/<scope>/<protocol>`, the scope being the genesis
    /// block ID or the fork digest, in hex.
    #[must_use]
    pub fn to_string_with_name(self, name: &str) -> String {
        let scope = match self {
            Self::Chain(genesis_id) => hex::encode(<[u8; 32]>::from(genesis_id)),
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
    use lb_core::{era::ForkDigest, header::HeaderId};

    use super::ProtocolScope;

    #[test]
    fn a_name_carries_the_fork_digest_or_the_genesis_block_id_it_is_bound_to() {
        let fork = ProtocolScope::Fork(ForkDigest::from([0xab; 32]));
        assert_eq!(
            fork.to_string_with_name("mempool"),
            format!("/logos-blockchain/{}/mempool", "ab".repeat(32))
        );
        assert_eq!(
            fork.to_stream_protocol_with_name("blend").as_ref(),
            format!("/logos-blockchain/{}/blend", "ab".repeat(32))
        );
        assert_eq!(
            ProtocolScope::Chain(HeaderId::from([0xcd; 32]))
                .to_stream_protocol_with_name("kad")
                .as_ref(),
            format!("/logos-blockchain/{}/kad", "cd".repeat(32))
        );
    }
}
