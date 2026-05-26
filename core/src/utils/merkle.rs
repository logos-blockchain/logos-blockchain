use crate::{
    crypto::{Digest as _, Hash, Hasher},
    mantle::{Transaction, TxHash},
};

pub fn node(left: impl AsRef<[u8]>, right: impl AsRef<[u8]>) -> [u8; 32] {
    let mut hasher = Hasher::new();
    hasher.update(left.as_ref());
    hasher.update(right.as_ref());
    hasher.finalize().into()
}

// Calculates a 32-byte Merkle root of transactions
pub fn calculate_block_root<T: Transaction<Hash = TxHash>>(transactions: &[T]) -> Hash {
    let mut leaves: Vec<_> = transactions.iter().map(Transaction::hash).collect();

    let target_size = leaves.len().max(1).next_power_of_two();

    let zero_leaf: <T as Transaction>::Hash = [0u8; 32].into();
    leaves.resize(target_size, zero_leaf);

    while leaves.len() > 1 {
        leaves = leaves
            .chunks(2)
            .map(|pair| node(pair[0].0, pair[1].0).into())
            .collect();
    }

    leaves[0].into()
}

#[cfg(test)]
mod tests {
    use lb_key_management_system_keys::keys::ZkPublicKey;

    use super::*;
    use crate::mantle::{
        MantleTx, Note, Op,
        ledger::{Inputs, Outputs},
        ops::transfer::TransferOp,
    };

    fn create_random_tx(seed: u32) -> MantleTx {
        MantleTx(
            [Op::Transfer(TransferOp::new(
                Inputs::empty(),
                Outputs::new(vec![Note {
                    value: seed.into(),
                    pk: ZkPublicKey::zero(),
                }]),
            ))]
            .into(),
        )
    }

    #[test]
    fn test_root_two_elements() {
        let txs = vec![create_random_tx(0), create_random_tx(1)];
        let result = calculate_block_root(&txs);
        let leaf1 = txs[0].hash();
        let leaf2 = txs[1].hash();
        let expected = node(leaf1.0, leaf2.0);

        assert_eq!(result, expected);
    }

    #[test]
    fn test_root_single_element() {
        let txs = vec![create_random_tx(42)];
        let result = calculate_block_root(&txs);

        let expected = txs[0].hash();

        assert_eq!(result, Hash::from(expected));
    }

    #[test]
    fn test_root_empty_elements() {
        let txs: Vec<MantleTx> = vec![];
        let result = calculate_block_root(&txs);

        let expected = [0u8; 32];

        assert_eq!(result, Hash::from(expected));
    }
}
