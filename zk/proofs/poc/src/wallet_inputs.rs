use lb_groth16::{Field as _, Fr, Groth16Input, Groth16InputDeser};
use serde::Serialize;

#[derive(Clone)]
pub struct PoCWalletInputs {
    secret_voucher: Groth16Input,
    voucher_merkle_path: Vec<Groth16Input>,
    voucher_merkle_path_selectors: Vec<Groth16Input>,
}

pub struct PoCWalletInputsData {
    pub secret_voucher: Fr,
    pub voucher_merkle_path: Vec<Fr>,
    pub voucher_merkle_path_selectors: Vec<bool>,
}

#[derive(Serialize)]
pub struct PoCWalletInputsJson {
    secret_voucher: Groth16InputDeser,
    voucher_merkle_path: Vec<Groth16InputDeser>,
    voucher_merkle_path_selectors: Vec<Groth16InputDeser>,
}
impl From<&PoCWalletInputs> for PoCWalletInputsJson {
    fn from(
        PoCWalletInputs {
            secret_voucher,
            voucher_merkle_path,
            voucher_merkle_path_selectors,
        }: &PoCWalletInputs,
    ) -> Self {
        Self {
            secret_voucher: secret_voucher.into(),
            voucher_merkle_path: voucher_merkle_path.iter().map(Into::into).collect(),
            voucher_merkle_path_selectors: voucher_merkle_path_selectors
                .iter()
                .map(Into::into)
                .collect(),
        }
    }
}

impl From<PoCWalletInputsData> for PoCWalletInputs {
    fn from(
        PoCWalletInputsData {
            secret_voucher,
            voucher_merkle_path,
            voucher_merkle_path_selectors,
        }: PoCWalletInputsData,
    ) -> Self {
        Self {
            secret_voucher: secret_voucher.into(),
            voucher_merkle_path: voucher_merkle_path.into_iter().map(Into::into).collect(),
            voucher_merkle_path_selectors: voucher_merkle_path_selectors
                .into_iter()
                .map(|value: bool| Groth16Input::new(if value { Fr::ONE } else { Fr::ZERO }))
                .collect(),
        }
    }
}
