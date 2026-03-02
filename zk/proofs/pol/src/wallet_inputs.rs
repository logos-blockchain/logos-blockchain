use lb_groth16::{Field as _, Fr, Groth16Input, Groth16InputDeser};
use num_bigint::BigUint;
use serde::Serialize;

pub const AGED_NOTE_MERKLE_TREE_HEIGHT: usize = 32;
pub type AgedNotePath = [Fr; AGED_NOTE_MERKLE_TREE_HEIGHT];
pub type AgedSelectorPath = [bool; AGED_NOTE_MERKLE_TREE_HEIGHT];
pub const LATEST_NOTE_MERKLE_TREE_HEIGHT: usize = 32;
pub type LatestNotePath = [Fr; LATEST_NOTE_MERKLE_TREE_HEIGHT];
pub type LatestSelectorPath = [bool; LATEST_NOTE_MERKLE_TREE_HEIGHT];

/// Public inputs of the POL cirmcom circuit as circuit field values.
#[derive(Clone, Debug)]
pub struct PolWalletInputs {
    note_value: Groth16Input,
    transaction_hash: Groth16Input,
    output_number: Groth16Input,
    aged_path: [Groth16Input; AGED_NOTE_MERKLE_TREE_HEIGHT], // leaf-to-root
    aged_selectors: [Groth16Input; AGED_NOTE_MERKLE_TREE_HEIGHT], // root-to-leaf
    latest_path: [Groth16Input; LATEST_NOTE_MERKLE_TREE_HEIGHT], // leaf-to-root
    latest_selectors: [Groth16Input; LATEST_NOTE_MERKLE_TREE_HEIGHT], // root-to-leaf
    secret_key: Groth16Input,
}

/// Private inputs of the POL cirmcom circuit to be provided by the wallet.
#[derive(Clone, Debug)]
pub struct PolWalletInputsData {
    pub note_value: u64,
    pub transaction_hash: Fr,
    pub output_number: u64,
    pub aged_path: AgedNotePath,              // leaf-to-root
    pub aged_selectors: AgedSelectorPath,     // root-to-leaf
    pub latest_path: LatestNotePath,          // leaf-to-root
    pub latest_selectors: LatestSelectorPath, // root-to-leaf
    pub secret_key: Fr,
}

#[derive(Serialize)]
pub struct PolWalletInputsJson {
    #[serde(rename = "v")]
    note_value: Groth16InputDeser,
    #[serde(rename = "note_tx_hash")]
    transaction_hash: Groth16InputDeser,
    #[serde(rename = "note_output_number")]
    output_number: Groth16InputDeser,
    #[serde(rename = "noteid_aged_path")]
    aged_path: [Groth16InputDeser; AGED_NOTE_MERKLE_TREE_HEIGHT], // leaf-to-root
    #[serde(rename = "noteid_aged_selectors")]
    aged_selectors: [Groth16InputDeser; AGED_NOTE_MERKLE_TREE_HEIGHT], // root-to-leaf
    #[serde(rename = "noteid_latest_path")]
    latest_path: [Groth16InputDeser; LATEST_NOTE_MERKLE_TREE_HEIGHT], // leaf-to-root
    #[serde(rename = "noteid_latest_selectors")]
    latest_selectors: [Groth16InputDeser; LATEST_NOTE_MERKLE_TREE_HEIGHT], // root-to-leaf
    secret_key: Groth16InputDeser,
}
impl From<&PolWalletInputs> for PolWalletInputsJson {
    fn from(
        PolWalletInputs {
            note_value,
            transaction_hash,
            output_number,
            aged_path,
            aged_selectors,
            latest_path,
            latest_selectors,
            secret_key,
        }: &PolWalletInputs,
    ) -> Self {
        Self {
            note_value: note_value.into(),
            transaction_hash: transaction_hash.into(),
            output_number: output_number.into(),
            aged_path: aged_path.map(|path| (&path).into()),
            aged_selectors: aged_selectors.map(|selector| (&selector).into()),
            latest_path: latest_path.map(|path| (&path).into()),
            latest_selectors: latest_selectors.map(|selector| (&selector).into()),
            secret_key: secret_key.into(),
        }
    }
}

impl From<PolWalletInputsData> for PolWalletInputs {
    fn from(
        PolWalletInputsData {
            note_value,
            transaction_hash,
            output_number,
            aged_path,
            aged_selectors,
            latest_path,
            latest_selectors,
            secret_key,
        }: PolWalletInputsData,
    ) -> Self {
        Self {
            note_value: Groth16Input::new(Fr::from(BigUint::from(note_value))),
            transaction_hash: transaction_hash.into(),
            output_number: Groth16Input::new(Fr::from(BigUint::from(output_number))),
            aged_path: aged_path.map(Into::into),
            aged_selectors: aged_selectors
                .map(|selector| Groth16Input::new(if selector { Fr::ONE } else { Fr::ZERO })),
            latest_path: latest_path.map(Into::into),
            latest_selectors: latest_selectors
                .map(|selector| Groth16Input::new(if selector { Fr::ONE } else { Fr::ZERO })),
            secret_key: secret_key.into(),
        }
    }
}
