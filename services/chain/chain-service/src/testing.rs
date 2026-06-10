use lb_core::mantle::{
    Note,
    ledger::{Inputs, Outputs, Utxo},
    ops::transfer::TransferOp,
};
use lb_key_management_system_keys::keys::ZkKey;
use num_bigint::BigUint;

/// Chain should start with `total_stake` ≥ 1
pub fn genesis_utxo() -> Utxo {
    let zk_sk = ZkKey::from(BigUint::from(1u64));
    let transfer = TransferOp::new(
        Inputs::new([]),
        Outputs::new([Note::new(1, zk_sk.to_public_key())]),
    );
    transfer
        .outputs
        .utxo_by_index(0, &transfer)
        .expect("genesis transfer should create one UTXO")
}
