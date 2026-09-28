//! The HD keys of the wallet
//!
//! They are the note keys of a single account. Receive addresses are handed
//! out in the order of their index, and so are change addresses.
//!
//! The receive addresses below [`HdKeys::funding_start_index`] hold the stake:
//! funding never spends their notes, because a note has to age again to lead
//! after it is spent.

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

/// The index of a note key, which is the child number of the last level of
/// its path.
pub type Index = u32;

/// The account that the wallet operates on
const ACCOUNT: HardenedIndex = HardenedIndex::new(u31::new(0));

/// The path of the receive address at the index
#[must_use]
pub fn receive_path(index: Index) -> Path {
    note_path(NoteRole::Receive, index)
}

/// The path of the change address at the index
#[must_use]
pub fn change_path(index: Index) -> Path {
    note_path(NoteRole::Change, index)
}

fn note_path(role: NoteRole, index: Index) -> Path {
    let index = u31::try_new(index).expect("Index of a note key is below 2^31");
    Path::Note {
        account: ACCOUNT,
        role,
        index: HardenedIndex::new(index),
    }
}

/// The HD keys that the wallet tracks
pub struct HdKeys {
    /// The first receive index that funding spends from
    funding_start_index: Index,
    next_receive_index: Index,
    next_change_index: Index,
    /// The public key of every tracked key
    public_keys: HashMap<Path, ZkPublicKey>,
}

impl HdKeys {
    /// Asks the KMS for the public keys of the receive addresses below
    /// `next_receive_index` and of the change addresses below
    /// `next_change_index`.
    pub async fn fetch<Kms, RuntimeServiceId>(
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
        funding_start_index: Index,
        next_receive_index: Index,
        next_change_index: Index,
    ) -> Result<Self, WalletServiceError>
    where
        Kms: KmsServiceData<Backend = KmsBackend>,
        RuntimeServiceId: AsServiceId<Kms> + Debug + Display + Sync,
    {
        // The stake addresses and the first receive address are tracked from
        // the start, since they are funded before the wallet hands any out.
        let next_receive_index = next_receive_index.max(funding_start_index + 1);

        let paths = (0..next_receive_index)
            .map(receive_path)
            .chain((0..next_change_index).map(change_path));
        let mut public_keys = HashMap::new();
        for path in paths {
            public_keys.insert(path, public_key_at(kms, path).await?);
        }

        Ok(Self {
            funding_start_index,
            next_receive_index,
            next_change_index,
            public_keys,
        })
    }

    #[must_use]
    pub const fn next_receive_index(&self) -> Index {
        self.next_receive_index
    }

    #[must_use]
    pub const fn next_change_index(&self) -> Index {
        self.next_change_index
    }

    /// The tracked keys, by public key
    pub fn key_ids(&self) -> impl Iterator<Item = (ZkPublicKey, KeyId)> + '_ {
        self.public_keys
            .iter()
            .map(|(path, public_key)| (*public_key, KeyId::Path(*path)))
    }

    /// The public keys whose notes funding spends: every tracked key but the
    /// ones of the stake addresses.
    pub fn spendable_public_keys(&self) -> Vec<ZkPublicKey> {
        (self.funding_start_index..self.next_receive_index)
            .map(receive_path)
            .chain((0..self.next_change_index).map(change_path))
            .filter_map(|path| self.public_keys.get(&path).copied())
            .collect()
    }

    /// Tracks the next receive address, whose public key is given.
    pub fn add_receive_key(&mut self, public_key: ZkPublicKey) -> Path {
        let path = receive_path(self.next_receive_index);
        self.public_keys.insert(path, public_key);
        self.next_receive_index += 1;
        path
    }

    /// Tracks the next change address, whose public key is given.
    pub fn add_change_key(&mut self, public_key: ZkPublicKey) -> Path {
        let path = change_path(self.next_change_index);
        self.public_keys.insert(path, public_key);
        self.next_change_index += 1;
        path
    }
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
        .public_key(KeyId::Path(path))
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
        assert_eq!(receive_path(0).to_string(), "m/154'/0'/0'/0'");
        assert_eq!(receive_path(7).to_string(), "m/154'/0'/0'/7'");
        assert_eq!(change_path(0).to_string(), "m/154'/0'/1'/0'");
        assert_eq!(change_path(7).to_string(), "m/154'/0'/1'/7'");
    }

    #[test]
    fn stake_addresses_are_not_spendable() {
        let mut keys = keys(2, 3, 0);
        assert_eq!(keys.spendable_public_keys(), [public_key(2)]);

        keys.add_change_key(public_key(100));
        assert_eq!(
            keys.spendable_public_keys(),
            [public_key(2), public_key(100)]
        );
    }

    #[test]
    fn added_keys_take_the_next_indices() {
        let mut keys = keys(1, 2, 0);

        assert_eq!(keys.add_receive_key(public_key(2)), receive_path(2));
        assert_eq!(keys.add_receive_key(public_key(3)), receive_path(3));
        assert_eq!(keys.add_change_key(public_key(100)), change_path(0));
        assert_eq!(keys.next_receive_index(), 4);
        assert_eq!(keys.next_change_index(), 1);
        assert_eq!(keys.key_ids().count(), 5);
    }

    /// Keys whose receive address at index `i` has [`public_key(i)`].
    fn keys(
        funding_start_index: Index,
        next_receive_index: Index,
        next_change_index: Index,
    ) -> HdKeys {
        HdKeys {
            funding_start_index,
            next_receive_index,
            next_change_index,
            public_keys: (0..next_receive_index)
                .map(|index| (receive_path(index), public_key(index)))
                .collect(),
        }
    }

    fn public_key(seed: u32) -> ZkPublicKey {
        ZkPublicKey::new(seed.into())
    }
}
