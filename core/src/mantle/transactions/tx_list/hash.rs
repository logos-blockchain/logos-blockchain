use crate::{
    crypto::{Digest as _, Hasher},
    mantle::{TxHash, traits::Hashable},
};

pub fn tx_hasher<Tx: Hashable<Hash = TxHash>>(tx: &Tx) -> TxHash {
    let bytes: [u8; 32] = Hasher::digest(tx.as_signing()).into();
    TxHash::from(bytes)
}
