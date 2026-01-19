use lb_groth16::{Fr, Groth16Input, Groth16InputDeser};
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone)]
pub struct PoCChainInputs {
    voucher_root: Groth16Input,
    mantle_tx_hash: Groth16Input,
}

pub struct PoCChainInputsData {
    pub voucher_root: Fr,
    pub mantle_tx_hash: Fr,
}

#[derive(Deserialize, Serialize)]
pub struct PoCChainInputsJson {
    voucher_root: Groth16InputDeser,
    mantle_tx_hash: Groth16InputDeser,
}

impl From<&PoCChainInputs> for PoCChainInputsJson {
    fn from(
        PoCChainInputs {
            voucher_root,
            mantle_tx_hash,
        }: &PoCChainInputs,
    ) -> Self {
        Self {
            voucher_root: voucher_root.into(),
            mantle_tx_hash: mantle_tx_hash.into(),
        }
    }
}

impl From<PoCChainInputsData> for PoCChainInputs {
    fn from(
        PoCChainInputsData {
            voucher_root,
            mantle_tx_hash,
        }: PoCChainInputsData,
    ) -> Self {
        Self {
            voucher_root: voucher_root.into(),
            mantle_tx_hash: mantle_tx_hash.into(),
        }
    }
}
