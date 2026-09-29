//! The addresses of the wallet that are known before the node runs
//!
//! They are the receive addresses up to the first one that funding spends
//! from. The ones below it hold the stake.

use std::fmt::{self, Display, Formatter};

use lb_groth16::fr_to_bytes;
use lb_key_management_system_service::{
    hd::{MasterKey, Path},
    keys::ZkPublicKey,
};
use lb_wallet_service::hd::{Index, receive_path};

use crate::UserConfig;

pub struct Address {
    pub path: Path,
    pub public_key: ZkPublicKey,
    /// Whether funding never spends the notes of the address
    pub is_stake: bool,
}

impl Display for Address {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let kind = if self.is_stake { "Stake" } else { "Receive" };
        let public_key = hex::encode(fr_to_bytes(self.public_key.as_fr()));
        write!(f, "{kind} address {}: {public_key}", self.path)
    }
}

/// The stake addresses, followed by the first receive address.
#[must_use]
pub fn addresses(master: &MasterKey, funding_start_index: Index) -> Vec<Address> {
    (0..=funding_start_index)
        .map(|index| {
            let path = receive_path(index);
            Address {
                path,
                public_key: master.derive_key(&path).to_zk_key().to_public_key(),
                is_stake: index < funding_start_index,
            }
        })
        .collect()
}

/// The addresses of the wallet in the user config.
#[must_use]
pub fn addresses_from_config(user_config: &UserConfig) -> Vec<Address> {
    addresses(
        &user_config.kms.backend.master_key(),
        user_config.wallet.funding_start_index,
    )
}

#[cfg(test)]
mod tests {
    use lb_key_management_system_service::hd::MasterSeed;

    use super::*;

    // Test vectors of the spec
    const MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[test]
    fn addresses_below_the_funding_start_index_hold_the_stake() {
        let master = MasterSeed::from_mnemonic(&MNEMONIC.parse().unwrap(), "").to_key();

        let addresses = addresses(&master, 2);

        let paths = addresses
            .iter()
            .map(|address| (address.path.to_string(), address.is_stake))
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            [
                ("m/154'/0'/0'/0'".to_owned(), true),
                ("m/154'/0'/0'/1'".to_owned(), true),
                ("m/154'/0'/0'/2'".to_owned(), false),
            ]
        );
    }

    #[test]
    fn address_shows_its_path_and_public_key() {
        let master = MasterSeed::from_mnemonic(&MNEMONIC.parse().unwrap(), "").to_key();

        let address = addresses(&master, 0).remove(0).to_string();

        assert!(address.starts_with("Receive address m/154'/0'/0'/0': "));
        let public_key = address.rsplit(' ').next().unwrap();
        assert_eq!(hex::decode(public_key).unwrap().len(), 32);
    }
}
