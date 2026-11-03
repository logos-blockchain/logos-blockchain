//! The digests naming a chain's eras, and the fork a node follows.
//!
//! An era is a range of epochs governed by one set of parameters. Its digest
//! commits to the epoch it starts at and to those parameters. A fork is the
//! sequence of eras a chain has activated since genesis; its digest commits to
//! the chain's genesis block, which carries the chain ID, and to the digest of
//! each of those eras in order, so two nodes agree on a fork digest exactly
//! when they follow the same chain under the same rules.

use core::fmt::{self, Debug, Formatter};

use blake2::Digest as _;
use lb_binary_codec::canonical::{BinaryCodec, BinaryEncode, codec_fixtures};
use lb_time::Epoch;
pub use lb_time::Era;

use crate::{
    crypto::Hasher,
    header::HeaderId,
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
    /// `blake2b256(b"FORK_DIGEST_V1" || genesis_id || era_0 || … || era_n)`,
    /// over the canonical encodings of the genesis block ID and of the digest
    /// of each activated era, in activation order.
    #[must_use]
    pub fn compute<EraDigests>(genesis_id: HeaderId, era_digests: EraDigests) -> Self
    where
        EraDigests: Iterator<Item = EraDigest>,
    {
        let mut hasher = Hasher::new();
        hasher.update(FORK_DIGEST_V1);
        hasher.update(genesis_id.encode());
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
    use lb_time::Epoch;

    use super::{EraDigest, ForkDigest};
    use crate::header::HeaderId;

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
            hex::encode(ForkDigest::compute(genesis_id, [first_era()].into_iter()).0),
            "624be61709aca6632bd65d1c2eda2ca0e3f5a27bd5851fcb5685ff5faf2cf244"
        );
        assert_eq!(
            hex::encode(ForkDigest::compute(genesis_id, [first_era(), second_era()].into_iter()).0),
            "c9c4a350c6c6a900df04998294477a40bf10d5f85bffc782de139db202e26b57"
        );
        assert_eq!(
            hex::encode(
                ForkDigest::compute(HeaderId::from([0x11u8; 32]), [first_era()].into_iter()).0
            ),
            "ab0a3faffb1c6cdaeacd0ba2de92a1f61b7dc1d7414b170cf3ed5b5ce3fb693c"
        );
    }

    #[test]
    fn every_input_moves_the_fork_digest() {
        let genesis_id = HeaderId::from([0u8; 32]);
        let base = ForkDigest::compute(genesis_id, [first_era()].into_iter());
        for other in [
            ForkDigest::compute(HeaderId::from([1u8; 32]), [first_era()].into_iter()),
            ForkDigest::compute(genesis_id, [second_era()].into_iter()),
            ForkDigest::compute(genesis_id, [first_era(), second_era()].into_iter()),
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
