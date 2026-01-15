use groth16::{Field as _, Fr, Groth16Input, Groth16InputDeser};
use num_bigint::BigUint;
use serde::Serialize;

/// Public inputs of the POL cirmcom circuit as circuit field values.
#[derive(Clone, Debug)]
pub struct PolWalletInputs {
    note_value: Groth16Input,
    transaction_hash: Groth16Input,
    output_number: Groth16Input,
    aged_path: Vec<Groth16Input>,
    aged_selector: Vec<Groth16Input>,
    latest_path: Vec<Groth16Input>,
    latest_selector: Vec<Groth16Input>,
    secret_key: Groth16Input,
}

/// Private inputs of the POL cirmcom circuit to be provided by the wallet.
#[derive(Clone, Debug)]
pub struct PolWalletInputsData {
    pub note_value: u64,
    pub transaction_hash: Fr,
    pub output_number: u64,
    pub aged_path: Vec<Fr>,
    pub aged_selector: Vec<bool>,
    pub latest_path: Vec<Fr>,
    pub latest_selector: Vec<bool>,
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
    aged_path: Vec<Groth16InputDeser>,
    #[serde(rename = "noteid_aged_selectors")]
    aged_selector: Vec<Groth16InputDeser>,
    #[serde(rename = "noteid_latest_path")]
    latest_path: Vec<Groth16InputDeser>,
    #[serde(rename = "noteid_latest_selectors")]
    latest_selector: Vec<Groth16InputDeser>,
    secret_key: Groth16InputDeser,
}
impl From<&PolWalletInputs> for PolWalletInputsJson {
    fn from(
        PolWalletInputs {
            note_value,
            transaction_hash,
            output_number,
            aged_path,
            aged_selector,
            latest_path,
            latest_selector,
            secret_key,
        }: &PolWalletInputs,
    ) -> Self {
        Self {
            note_value: note_value.into(),
            transaction_hash: transaction_hash.into(),
            output_number: output_number.into(),
            aged_path: aged_path.iter().map(Into::into).collect(),
            aged_selector: aged_selector.iter().map(Into::into).collect(),
            latest_path: latest_path.iter().map(Into::into).collect(),
            latest_selector: latest_selector.iter().map(Into::into).collect(),
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
            aged_selector,
            latest_path,
            latest_selector,
            secret_key,
        }: PolWalletInputsData,
    ) -> Self {
        Self {
            note_value: Groth16Input::new(Fr::from(BigUint::from(note_value))),
            transaction_hash: transaction_hash.into(),
            output_number: Groth16Input::new(Fr::from(BigUint::from(output_number))),
            aged_path: aged_path.into_iter().map(Into::into).collect(),
            aged_selector: aged_selector
                .into_iter()
                .map(|value: bool| Groth16Input::new(if value { Fr::ONE } else { Fr::ZERO }))
                .collect(),
            latest_path: latest_path.into_iter().map(Into::into).collect(),
            latest_selector: latest_selector
                .into_iter()
                .map(|value: bool| Groth16Input::new(if value { Fr::ONE } else { Fr::ZERO }))
                .collect(),
            secret_key: secret_key.into(),
        }
    }
}
