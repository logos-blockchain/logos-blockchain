use std::collections::HashMap;

use lb_key_management_system_service::{
    backend::preload::KeyId,
    hd::{Mnemonic, Passphrase},
    keys::Key,
};
use serde::Deserialize;

use crate::cli::config::keystore::{KeyTitle, Keystore};

/// The title that a legacy keystore gives to the voucher master key
const MISSPELLED_VOUCHER_MASTER: &str = "VaucherMaster";
const VOUCHER_MASTER: &str = "VoucherMaster";

/// The keystore of a node that has no HD wallet
#[derive(Deserialize)]
pub struct LegacyKeystore {
    /// The id of each key, which is the hex of its public key
    public_keys: HashMap<String, KeyId>,
    secret_keys: HashMap<String, Key>,
}

impl LegacyKeystore {
    /// The titles of the keys, by the ids that the node knows them under.
    pub fn titles_by_key_id(&self) -> HashMap<KeyId, KeyTitle> {
        self.public_keys
            .iter()
            .map(|(title, key_id)| (key_id.clone(), upgraded_title(title)))
            .collect()
    }

    /// The keystore that has the keys under their titles, along with the
    /// mnemonic.
    pub fn upgrade(self, mnemonic: Mnemonic, passphrase: Option<Passphrase>) -> Keystore {
        let mut keystore = Keystore::empty(mnemonic, passphrase);
        for (title, key) in self.secret_keys {
            keystore.set(upgraded_title(&title), key);
        }
        keystore
    }
}

fn upgraded_title(title: &str) -> KeyTitle {
    if title == MISSPELLED_VOUCHER_MASTER {
        VOUCHER_MASTER.into()
    } else {
        title.into()
    }
}

/// The title of the key that the vouchers of a legacy wallet are derived
/// from
pub const fn voucher_master_title() -> &'static str {
    VOUCHER_MASTER
}
