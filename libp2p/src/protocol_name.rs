use core::ops::{Deref, DerefMut};

use lb_core::{era::ForkDigest, header::HeaderId};
use libp2p::StreamProtocol as Libp2pStreamProtocol;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Struct wrapping around a `StreamProtocol` to make it serializable and
/// catch any invalid protocol names already at config reading instead of later
/// on.
pub struct StreamProtocol(
    #[serde(
        serialize_with = "serialize_stream_protocol",
        deserialize_with = "deserialize_stream_protocol"
    )]
    Libp2pStreamProtocol,
);

fn serialize_stream_protocol<S>(
    protocol_name: &Libp2pStreamProtocol,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    protocol_name.as_ref().serialize(serializer)
}

fn deserialize_stream_protocol<'de, D>(deserializer: D) -> Result<Libp2pStreamProtocol, D::Error>
where
    D: Deserializer<'de>,
{
    let protocol_name = String::deserialize(deserializer)?;
    Libp2pStreamProtocol::try_from_owned(protocol_name).map_err(Error::custom)
}

impl StreamProtocol {
    #[must_use]
    pub const fn new(protocol_name: &'static str) -> Self {
        Self(Libp2pStreamProtocol::new(protocol_name))
    }

    #[must_use]
    pub fn into_inner(self) -> Libp2pStreamProtocol {
        self.0
    }
}

impl Deref for StreamProtocol {
    type Target = Libp2pStreamProtocol;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for StreamProtocol {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<StreamProtocol> for Libp2pStreamProtocol {
    fn from(value: StreamProtocol) -> Self {
        value.0
    }
}

impl From<Libp2pStreamProtocol> for StreamProtocol {
    fn from(value: Libp2pStreamProtocol) -> Self {
        Self(value)
    }
}

/// The prefix of every name.
const NAMESPACE: &str = "/logos-blockchain";

/// What the name of a protocol or a topic is bound to.
///
/// A protocol whose messages depend on the rules in force is bound to a fork:
/// its name carries the fork digest, so nodes on different forks neither talk
/// to each other nor share topics. Kademlia and identify only find and describe
/// peers of the same chain, so they are bound to the chain: their names carry
/// the genesis block ID instead, and peer discovery keeps working across the
/// forks of a chain.
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

    /// The stream protocol `protocol` in this scope, named by
    /// [`Self::to_string_with_name`].
    #[must_use]
    pub fn to_stream_protocol_with_name(self, name: &str) -> StreamProtocol {
        Libp2pStreamProtocol::try_from_owned(self.to_string_with_name(name))
            .unwrap()
            .into()
    }

    /// The fork digest in `name`, if `name` has the shape of a name bound to a
    /// fork: `/logos-blockchain/<fork digest>/<protocol>`. A name bound to the
    /// chain has the same shape, with the genesis block ID where the digest
    /// goes, so it reads as a digest too: tell the chain's names apart first.
    #[must_use]
    pub fn fork_digest_in(name: &str) -> Option<ForkDigest> {
        let (protocol_fork_digest_hex, _) = name
            .strip_prefix(NAMESPACE)?
            .strip_prefix('/')?
            .split_once('/')?;
        let protocol_fork_digest = {
            let mut buff = [0; 32];
            hex::decode_to_slice(protocol_fork_digest_hex, &mut buff).ok()?;
            buff
        };
        Some(ForkDigest::from(protocol_fork_digest))
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

    #[test]
    fn the_fork_digest_is_read_back_from_a_name_bound_to_its_fork() {
        let fork_digest = ForkDigest::from([0xab; 32]);
        assert_eq!(
            ProtocolScope::fork_digest_in(
                &ProtocolScope::Fork(fork_digest).to_string_with_name("chainsync")
            ),
            Some(fork_digest)
        );
        assert_eq!(ProtocolScope::fork_digest_in("/ipfs/id/1.0.0"), None);
        assert_eq!(
            ProtocolScope::fork_digest_in(&format!(
                "/logos-blockchain/{}/chainsync",
                "ab".repeat(31)
            )),
            None
        );
    }
}
