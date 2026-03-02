use lb_groth16::{Field as _, Fr, Groth16Input, Groth16InputDeser};
use serde::Serialize;

pub const VOUCHER_MERKLE_TREE_HEIGHT: usize = 32;
pub type VoucherPathAndSelector = [(Fr, bool); VOUCHER_MERKLE_TREE_HEIGHT];

#[derive(Clone)]
pub struct PoCWalletInputs {
    secret_voucher: Groth16Input,
    voucher_merkle_path_and_selectors: [(Groth16Input, Groth16Input); VOUCHER_MERKLE_TREE_HEIGHT],
}

#[derive(Clone, Debug)]
pub struct PoCWalletInputsData {
    pub secret_voucher: Fr,
    pub voucher_merkle_path_and_selectors: VoucherPathAndSelector,
}

#[derive(Serialize)]
pub struct PoCWalletInputsJson {
    secret_voucher: Groth16InputDeser,
    voucher_merkle_path: [Groth16InputDeser; VOUCHER_MERKLE_TREE_HEIGHT],
    voucher_merkle_path_selectors: [Groth16InputDeser; VOUCHER_MERKLE_TREE_HEIGHT],
}
impl From<&PoCWalletInputs> for PoCWalletInputsJson {
    fn from(
        PoCWalletInputs {
            secret_voucher,
            voucher_merkle_path_and_selectors,
        }: &PoCWalletInputs,
    ) -> Self {
        let (voucher_path, voucher_selectors) = {
            let voucher_path = voucher_merkle_path_and_selectors.map(|(path, _)| (&path).into());
            let voucher_selectors =
                voucher_merkle_path_and_selectors.map(|(_, selector)| (&selector).into());
            (voucher_path, voucher_selectors)
        };
        Self {
            secret_voucher: secret_voucher.into(),
            voucher_merkle_path: voucher_path,
            voucher_merkle_path_selectors: voucher_selectors,
        }
    }
}

impl From<PoCWalletInputsData> for PoCWalletInputs {
    fn from(
        PoCWalletInputsData {
            secret_voucher,
            voucher_merkle_path_and_selectors,
        }: PoCWalletInputsData,
    ) -> Self {
        Self {
            secret_voucher: secret_voucher.into(),
            voucher_merkle_path_and_selectors: voucher_merkle_path_and_selectors.map(
                |(value, selector)| {
                    (
                        value.into(),
                        Groth16Input::new(if selector { Fr::ONE } else { Fr::ZERO }),
                    )
                },
            ),
        }
    }
}
