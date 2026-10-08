//! The digests naming a chain's eras, and the fork a node follows.
//!
//! An era is a range of epochs governed by one set of parameters. Its digest
//! commits to the epoch it starts at and to those parameters. A fork is the
//! sequence of eras a chain has activated since genesis; its digest commits to
//! the chain's genesis block, its chain ID and the digest of each of those eras
//! in order, so two nodes agree on a fork digest exactly when they follow the
//! same chain under the same rules.

use core::fmt::{self, Debug, Formatter};

use blake2::Digest as _;
use lb_binary_codec::canonical::{BinaryCodec, BinaryEncode, codec_fixtures};
use lb_cryptarchia_engine::Epoch;
pub use lb_cryptarchia_engine::era::EraNumber;

use crate::{
    crypto::Hasher,
    header::HeaderId,
    mantle::transactions::genesis_tx::ChainId,
    utils::{display_hex_bytes_newtype, serde_bytes_newtype},
};

/// The digest of an era: of the epoch it starts at, and of its parameters.
#[derive(Clone, Copy, PartialEq, Eq, Hash, BinaryCodec)]
pub struct EraDigest([u8; 32]);

const ERA_DIGEST_V1: &[u8] = b"ERA_DIGEST_V1";

impl EraDigest {
    /// `blake2b256(b"ERA_DIGEST_V1" || first_epoch || parameters)`, over the
    /// canonical encodings of the era's first epoch and of its parameters.
    #[must_use]
    pub fn compute<Parameters>(first_era_epoch: Epoch, parameters: &Parameters) -> Self
    where
        Parameters: BinaryEncode,
    {
        let mut hasher = Hasher::new();
        hasher.update(ERA_DIGEST_V1);
        hasher.update(first_era_epoch.encode());
        hasher.update(parameters.encode());
        Self(hasher.finalize().into())
    }
}

impl Debug for EraDigest {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "EraDigest({})", hex::encode(self.0))
    }
}

impl From<[u8; 32]> for EraDigest {
    fn from(digest: [u8; 32]) -> Self {
        Self(digest)
    }
}

impl From<EraDigest> for [u8; 32] {
    fn from(digest: EraDigest) -> Self {
        digest.0
    }
}

display_hex_bytes_newtype!(EraDigest);
serde_bytes_newtype!(EraDigest, 32);
codec_fixtures!(EraDigest, Self([0x11u8; 32]) => "1111111111111111111111111111111111111111111111111111111111111111");

/// The digest of a fork: the chain of eras a chain has activated since its
/// genesis.
#[derive(Clone, Copy, PartialEq, Eq, Hash, BinaryCodec)]
pub struct ForkDigest([u8; 32]);

const FORK_DIGEST_V1: &[u8] = b"FORK_DIGEST_V1";

impl ForkDigest {
    /// `blake2b256(b"FORK_DIGEST_V1" || genesis_id || chain_id || era_0 || … ||
    /// era_n)`, over the canonical encodings of the genesis block ID, of the
    /// chain ID, and of the digest of each activated era, in activation order.
    #[must_use]
    pub fn compute<EraDigests>(
        genesis_id: HeaderId,
        chain_id: &ChainId,
        era_digests: EraDigests,
    ) -> Self
    where
        EraDigests: Iterator<Item = EraDigest>,
    {
        let mut hasher = Hasher::new();
        hasher.update(FORK_DIGEST_V1);
        hasher.update(genesis_id.encode());
        hasher.update(chain_id.encode());
        for era in era_digests {
            hasher.update(era.0);
        }
        Self(hasher.finalize().into())
    }
}

impl Debug for ForkDigest {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "ForkDigest({})", hex::encode(self.0))
    }
}

impl From<[u8; 32]> for ForkDigest {
    fn from(digest: [u8; 32]) -> Self {
        Self(digest)
    }
}

impl From<ForkDigest> for [u8; 32] {
    fn from(digest: ForkDigest) -> Self {
        digest.0
    }
}

display_hex_bytes_newtype!(ForkDigest);
serde_bytes_newtype!(ForkDigest, 32);
codec_fixtures!(ForkDigest, Self([0x22u8; 32]) => "2222222222222222222222222222222222222222222222222222222222222222");

#[cfg(test)]
mod tests {
    use lb_cryptarchia_engine::Epoch;

    use super::{EraDigest, ForkDigest};
    use crate::{header::HeaderId, mantle::transactions::genesis_tx::ChainId};

    fn chain_id(chain_id: &str) -> ChainId {
        ChainId::try_from(chain_id.to_owned()).unwrap()
    }

    fn first_era() -> EraDigest {
        EraDigest::compute(Epoch::new(0), &[0x11u8; 4])
    }

    fn second_era() -> EraDigest {
        EraDigest::compute(Epoch::new(7), &[0x22u8; 4])
    }

    #[test]
    fn era_digest_vectors() {
        assert_eq!(
            hex::encode(first_era().0),
            "6e274dc467ddb28cc92006d1cc949913c3b6adf1fcf52a3951f12e005468e085"
        );
        assert_eq!(
            hex::encode(second_era().0),
            "03a95afcc8e1f39a53f1d35cef5aed3b44f1dc4fe2029acc0d92c35d2bb0a728"
        );
    }

    #[test]
    fn fork_digest_vectors() {
        let genesis_id = HeaderId::from([0u8; 32]);
        assert_eq!(
            hex::encode(
                ForkDigest::compute(genesis_id, &chain_id("test"), [first_era()].into_iter()).0,
            ),
            "10a064258df21b594855a84e4d13083922bec0fd11f9f4b6ed4f6486b27e5f1e"
        );
        assert_eq!(
            hex::encode(
                ForkDigest::compute(
                    genesis_id,
                    &chain_id("test"),
                    [first_era(), second_era()].into_iter(),
                )
                .0
            ),
            "9e1ae6b6fc0768dc545005de63dc7b41b3339c6e1c8e39905042ed00d00f7726"
        );
        assert_eq!(
            hex::encode(
                ForkDigest::compute(
                    HeaderId::from([0x11u8; 32]),
                    &chain_id("logos-blockchain-testnet"),
                    [first_era()].into_iter()
                )
                .0
            ),
            "42c090e5565ff31f69c9ef575b3c86b3a8c7c92f95b7104c168947043b85bb09"
        );
    }

    #[test]
    fn every_input_moves_the_fork_digest() {
        let genesis_id = HeaderId::from([0u8; 32]);
        let base = ForkDigest::compute(genesis_id, &chain_id("test"), [first_era()].into_iter());
        for other in [
            ForkDigest::compute(
                HeaderId::from([1u8; 32]),
                &chain_id("test"),
                [first_era()].into_iter(),
            ),
            ForkDigest::compute(genesis_id, &chain_id("tesu"), [first_era()].into_iter()),
            ForkDigest::compute(genesis_id, &chain_id("test"), [second_era()].into_iter()),
            ForkDigest::compute(
                genesis_id,
                &chain_id("test"),
                [first_era(), second_era()].into_iter(),
            ),
        ] {
            assert_ne!(base, other);
        }
    }

    #[test]
    fn digests_serialize_as_hex() {
        let json = serde_json::to_string(&ForkDigest::from([0xabu8; 32])).unwrap();
        assert_eq!(json, format!("\"{}\"", "ab".repeat(32)));
        assert_eq!(
            serde_json::from_str::<ForkDigest>(&json).unwrap(),
            ForkDigest::from([0xabu8; 32])
        );
    }
}
