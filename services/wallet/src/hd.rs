//! The HD keys of the wallet
//!
//! They are the note keys of a single account. Receive addresses are handed
//! out in the order of their index, and so are change addresses.

use std::{
    collections::HashMap,
    fmt::{Debug, Display},
};

use lb_key_management_system_service::{
    api::{KmsServiceApi, KmsServiceData},
    hd::{HardenedIndex, NoteRole, Path, u31},
    keys::{PublicKeyEncoding, ZkPublicKey},
};
use overwatch::services::AsServiceId;

use crate::{KeyId, KmsBackend, WalletServiceError};

/// The receive address that holds the stake
pub const STAKE_RECEIVE_INDEX: HardenedIndex = HardenedIndex::new(u31::new(0));

/// The receive address that receives the other funds, e.g. the fees and the
/// `PoW` rewards
pub const FUNDING_RECEIVE_INDEX: HardenedIndex = HardenedIndex::new(u31::new(1));

/// The initial next receive index, which is the next index
/// of [`FUNDING_RECEIVE_INDEX`].
pub const INITIAL_NEXT_RECEIVE_INDEX: HardenedIndex = HardenedIndex::new(u31::new(2));
pub const INITIAL_NEXT_CHANGE_INDEX: HardenedIndex = HardenedIndex::new(u31::new(0));

/// The account that the wallet operates on
const ACCOUNT: HardenedIndex = HardenedIndex::new(u31::new(0));

/// The path of the receive address at the index
#[must_use]
pub const fn receive_path(index: HardenedIndex) -> Path {
    note_path(NoteRole::Receive, index)
}

/// The path of the change address at the index
#[must_use]
pub const fn change_path(index: HardenedIndex) -> Path {
    note_path(NoteRole::Change, index)
}

/// The path of the voucher master key
#[must_use]
pub const fn voucher_master_path() -> Path {
    Path::VoucherMaster { account: ACCOUNT }
}

const fn note_path(role: NoteRole, index: HardenedIndex) -> Path {
    Path::Note {
        account: ACCOUNT,
        role,
        index,
    }
}

/// The indices below `next`, in ascending order
fn indices_below(next: HardenedIndex) -> impl Iterator<Item = HardenedIndex> {
    (0..next.child_number().value()).map(|child_number| HardenedIndex::new(u31::new(child_number)))
}

/// The HD keys that the wallet tracks: the receive addresses below
/// `next_receive_index` and the change addresses below `next_change_index`.
pub struct HdKeys {
    next_receive_index: HardenedIndex,
    next_change_index: HardenedIndex,
    /// The public key of every tracked key
    public_keys: HashMap<Path, ZkPublicKey>,
}

impl HdKeys {
    /// Asks the KMS for the public key of every tracked key.
    pub async fn fetch<Kms, RuntimeServiceId>(
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
        next_receive_index: HardenedIndex,
        next_change_index: HardenedIndex,
    ) -> Result<Self, WalletServiceError>
    where
        Kms: KmsServiceData<Backend = KmsBackend>,
        RuntimeServiceId: AsServiceId<Kms> + Debug + Display + Sync,
    {
        let paths = indices_below(next_receive_index)
            .map(receive_path)
            .chain(indices_below(next_change_index).map(change_path));
        let mut public_keys = HashMap::new();
        for path in paths {
            public_keys.insert(path, public_key_at(kms, path).await?);
        }

        Ok(Self {
            next_receive_index,
            next_change_index,
            public_keys,
        })
    }

    #[must_use]
    pub const fn next_receive_index(&self) -> HardenedIndex {
        self.next_receive_index
    }

    #[must_use]
    pub const fn next_change_index(&self) -> HardenedIndex {
        self.next_change_index
    }

    /// The tracked keys, by public key
    pub fn key_ids(&self) -> impl Iterator<Item = (ZkPublicKey, KeyId)> + '_ {
        self.public_keys
            .iter()
            .map(|(path, public_key)| (*public_key, KeyId::Hd(*path)))
    }

    /// Tracks the receive address at the index, whose public key is given.
    ///
    /// # Errors
    ///
    /// Returns an error if the index is not the next receive index.
    pub fn track_receive_key(
        &mut self,
        index: HardenedIndex,
        public_key: ZkPublicKey,
    ) -> Result<Path, WalletServiceError> {
        check_next_index(index, self.next_receive_index)?;
        let path = receive_path(index);
        self.public_keys.insert(path, public_key);
        self.next_receive_index = index
            .checked_next()
            .expect("Fewer than 2^31 receive addresses are handed out");
        Ok(path)
    }

    /// Tracks the change address at the index, whose public key is given.
    ///
    /// # Errors
    ///
    /// Returns an error if the index is not the next change index.
    pub fn track_change_key(
        &mut self,
        index: HardenedIndex,
        public_key: ZkPublicKey,
    ) -> Result<Path, WalletServiceError> {
        check_next_index(index, self.next_change_index)?;
        let path = change_path(index);
        self.public_keys.insert(path, public_key);
        self.next_change_index = index
            .checked_next()
            .expect("Fewer than 2^31 change addresses are handed out");
        Ok(path)
    }
}

fn check_next_index(
    actual: HardenedIndex,
    expected: HardenedIndex,
) -> Result<(), WalletServiceError> {
    if actual == expected {
        Ok(())
    } else {
        Err(WalletServiceError::UnexpectedHdKeyIndex { expected, actual })
    }
}

#[cfg(test)]
impl HdKeys {
    /// Keys whose receive address at index `i` has the public key `i`, and
    /// whose change address at index `i` has the public key `1000 + i`.
    #[must_use]
    pub fn for_tests(next_receive_index: HardenedIndex, next_change_index: HardenedIndex) -> Self {
        let public_key = |index: HardenedIndex, offset: u32| {
            ZkPublicKey::new((offset + index.child_number().value()).into())
        };
        Self {
            next_receive_index,
            next_change_index,
            public_keys: indices_below(next_receive_index)
                .map(|index| (receive_path(index), public_key(index, 0)))
                .chain(
                    indices_below(next_change_index)
                        .map(|index| (change_path(index), public_key(index, 1000))),
                )
                .collect(),
        }
    }
}

/// The hardened index of the child number
#[cfg(test)]
#[must_use]
pub const fn index(child_number: u32) -> HardenedIndex {
    HardenedIndex::new(u31::new(child_number))
}

/// Asks the KMS for the public key of the key at the path.
pub async fn public_key_at<Kms, RuntimeServiceId>(
    kms: &KmsServiceApi<Kms, RuntimeServiceId>,
    path: Path,
) -> Result<ZkPublicKey, WalletServiceError>
where
    Kms: KmsServiceData<Backend = KmsBackend>,
    RuntimeServiceId: AsServiceId<Kms> + Debug + Display + Sync,
{
    let public_key = kms
        .public_key(KeyId::Hd(path))
        .await
        .map_err(WalletServiceError::KmsApi)?;
    let PublicKeyEncoding::Zk(public_key) = public_key else {
        return Err(WalletServiceError::KmsApi(
            "Expected the ZK public key of an HD key".into(),
        ));
    };
    Ok(public_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_in_account_0() {
        assert_eq!(receive_path(index(0)).to_string(), "m/154'/0'/0'/0'");
        assert_eq!(receive_path(index(7)).to_string(), "m/154'/0'/0'/7'");
        assert_eq!(change_path(index(0)).to_string(), "m/154'/0'/1'/0'");
        assert_eq!(change_path(index(7)).to_string(), "m/154'/0'/1'/7'");
        assert_eq!(voucher_master_path().to_string(), "m/154'/0'/2'");
    }

    #[test]
    fn tracked_keys_take_the_next_indices() {
        let mut keys = HdKeys::for_tests(index(2), index(0));

        assert_eq!(
            keys.track_receive_key(index(2), public_key(2)).unwrap(),
            receive_path(index(2))
        );
        assert_eq!(
            keys.track_receive_key(index(3), public_key(3)).unwrap(),
            receive_path(index(3))
        );
        assert_eq!(
            keys.track_change_key(index(0), public_key(1000)).unwrap(),
            change_path(index(0))
        );
        assert_eq!(keys.next_receive_index(), index(4));
        assert_eq!(keys.next_change_index(), index(1));
        assert_eq!(keys.key_ids().count(), 5);
    }

    #[test]
    fn only_the_next_index_is_tracked() {
        let mut keys = HdKeys::for_tests(index(2), index(1));

        for wrong in [1, 3].map(index) {
            assert!(matches!(
                keys.track_receive_key(wrong, public_key(500)),
                Err(WalletServiceError::UnexpectedHdKeyIndex { expected, actual })
                    if expected == index(2) && actual == wrong
            ));
        }
        for wrong in [0, 2].map(index) {
            assert!(matches!(
                keys.track_change_key(wrong, public_key(500)),
                Err(WalletServiceError::UnexpectedHdKeyIndex { expected, actual })
                    if expected == index(1) && actual == wrong
            ));
        }

        assert_eq!(keys.next_receive_index(), index(2));
        assert_eq!(keys.next_change_index(), index(1));
        assert_eq!(keys.key_ids().count(), 3);
    }

    fn public_key(seed: u32) -> ZkPublicKey {
        ZkPublicKey::new(seed.into())
    }
}
