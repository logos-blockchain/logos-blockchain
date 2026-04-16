use serde::{Deserialize, Serialize};

use crate::{
    crypto::{Digest as _, Hash, Hasher},
    mantle::{
        NoteId,
        encoding::encode_transfer_op,
        ledger::Outputs,
        ops::{OPERATION_ID_V1, OpId},
    },
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TransferOp {
    pub inputs: Vec<NoteId>,
    pub outputs: Outputs,
}

impl TransferOp {
    #[must_use]
    pub const fn new(inputs: Vec<NoteId>, outputs: Outputs) -> Self {
        Self { inputs, outputs }
    }
}

impl OpId for TransferOp {
    fn op_id(&self) -> Hash {
        let mut encoded_bytes: Vec<u8> = OPERATION_ID_V1.clone();
        encoded_bytes.extend(encode_transfer_op(self));
        Hasher::digest(&encoded_bytes).into()
    }
}

#[cfg(test)]
mod test {

    use lb_key_management_system_keys::keys::ZkPublicKey;
    use lb_poseidon2::Fr;
    use num_bigint::BigUint;

    use super::*;
    use crate::mantle::{Note, Utxo};

    #[test]
    fn test_utxo_by_index() {
        let pk0 = ZkPublicKey::from(Fr::from(BigUint::from(0u8)));
        let pk1 = ZkPublicKey::from(Fr::from(BigUint::from(1u8)));
        let pk2 = ZkPublicKey::from(Fr::from(BigUint::from(2u8)));
        let transfer = TransferOp {
            inputs: vec![NoteId(BigUint::from(0u8).into())],
            outputs: Outputs::new(vec![
                Note::new(100, pk0),
                Note::new(200, pk1),
                Note::new(300, pk2),
            ]),
        };
        assert_eq!(
            transfer.outputs.utxo_by_index(0, &transfer),
            Some(Utxo {
                op_id: transfer.op_id(),
                output_index: 0,
                note: Note::new(100, pk0),
            })
        );
        assert_eq!(
            transfer.outputs.utxo_by_index(1, &transfer),
            Some(Utxo {
                op_id: transfer.op_id(),
                output_index: 1,
                note: Note::new(200, pk1),
            })
        );
        assert_eq!(
            transfer.outputs.utxo_by_index(2, &transfer),
            Some(Utxo {
                op_id: transfer.op_id(),
                output_index: 2,
                note: Note::new(300, pk2),
            })
        );

        assert!(transfer.outputs.utxo_by_index(3, &transfer).is_none());
    }
}
