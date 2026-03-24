use std::sync::LazyLock;

use lb_groth16::{Fr, GROTH16_SAFE_BYTES_SIZE, fr_from_bytes, fr_from_bytes_unchecked};
use lb_poseidon2::Digest;
use serde::{Deserialize, Serialize};

use crate::{
    crypto::ZkHasher,
    mantle::{
        Note, NoteId, Transaction, TransactionHasher, TxHash, Utxo, encoding::encode_transfer_op,
    },
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TransferOp {
    pub inputs: Vec<NoteId>,
    pub outputs: Vec<Note>,
}

static LEDGER_TXHASH_V1_FR: LazyLock<Fr> =
    LazyLock::new(|| fr_from_bytes(b"LEDGER_TXHASH_V1").expect("Constant should be valid Fr"));

impl TransferOp {
    #[must_use]
    pub const fn new(inputs: Vec<NoteId>, outputs: Vec<Note>) -> Self {
        Self { inputs, outputs }
    }

    #[must_use]
    pub fn as_signing_frs(&self) -> Vec<Fr> {
        // constants and structure as defined in the Mantle spec:
        // https://www.notion.so/Mantle-Specification-21c261aa09df810c8820fab1d78b53d9
        let encoded_bytes = encode_transfer_op(self);
        let frs = encoded_bytes
            .as_slice()
            .chunks(GROTH16_SAFE_BYTES_SIZE)
            .map(fr_from_bytes_unchecked);
        std::iter::once(*LEDGER_TXHASH_V1_FR).chain(frs).collect()
    }

    #[must_use]
    pub fn utxo_by_index(&self, index: usize) -> Option<Utxo> {
        self.outputs.get(index).map(|note| Utxo {
            tx_hash: self.hash(),
            output_index: index,
            note: *note,
        })
    }

    pub fn utxos(&self) -> impl Iterator<Item = Utxo> + '_ {
        let tx_hash = self.hash();
        self.outputs
            .iter()
            .enumerate()
            .map(move |(index, note)| Utxo {
                tx_hash,
                output_index: index,
                note: *note,
            })
    }
}

impl Transaction for TransferOp {
    const HASHER: TransactionHasher<Self> =
        |op| <ZkHasher as Digest>::digest(&op.as_signing_frs()).into();
    type Hash = TxHash;

    fn as_signing_frs(&self) -> Vec<Fr> {
        Self::as_signing_frs(self)
    }
}
