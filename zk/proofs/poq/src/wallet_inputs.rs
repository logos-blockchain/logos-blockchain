use groth16::{Field as _, Fr, Groth16Input, Groth16InputDeser};
use num_bigint::BigUint;
use serde::Serialize;

pub const AGED_NOTE_MERKLE_TREE_HEIGHT: usize = 32;
pub type NotePathAndSelectors = [(Fr, bool); AGED_NOTE_MERKLE_TREE_HEIGHT];

#[derive(Clone)]
pub struct PoQWalletInputs {
    pol_slot: Groth16Input,
    pol_secret_key: Groth16Input,
    pol_noteid_path_and_selectors: [(Groth16Input, Groth16Input); AGED_NOTE_MERKLE_TREE_HEIGHT],
    pol_note_tx_hash: Groth16Input,
    pol_note_output_number: Groth16Input,
    pol_note_value: Groth16Input,
}

pub struct PoQWalletInputsData {
    pub slot: u64,
    pub note_value: u64,
    pub transaction_hash: Fr,
    pub output_number: u64,
    pub aged_path_and_selectors: NotePathAndSelectors,
    pub secret_key: Fr,
}

#[derive(Serialize)]
pub struct PoQWalletInputsJson {
    #[serde(rename = "pol_sl")]
    pol_slot: Groth16InputDeser,
    #[serde(rename = "pol_note_value")]
    note_value: Groth16InputDeser,
    #[serde(rename = "pol_note_tx_hash")]
    transaction_hash: Groth16InputDeser,
    #[serde(rename = "pol_note_output_number")]
    output_number: Groth16InputDeser,
    #[serde(rename = "pol_noteid_path")]
    aged_path: [Groth16InputDeser; AGED_NOTE_MERKLE_TREE_HEIGHT],
    #[serde(rename = "pol_noteid_path_selectors")]
    aged_selector: [Groth16InputDeser; AGED_NOTE_MERKLE_TREE_HEIGHT],
    #[serde(rename = "pol_secret_key")]
    secret_key: Groth16InputDeser,
}
impl From<PoQWalletInputs> for PoQWalletInputsJson {
    fn from(
        PoQWalletInputs {
            pol_slot,
            pol_secret_key,
            pol_noteid_path_and_selectors,
            pol_note_tx_hash,
            pol_note_output_number,
            pol_note_value,
        }: PoQWalletInputs,
    ) -> Self {
        let (aged_path, aged_selector) = {
            let aged_path = pol_noteid_path_and_selectors.map(|(path, _)| (&path).into());
            let aged_selector =
                pol_noteid_path_and_selectors.map(|(_, selector)| (&selector).into());
            (aged_path, aged_selector)
        };

        Self {
            pol_slot: (&pol_slot).into(),
            note_value: (&pol_note_value).into(),
            transaction_hash: (&pol_note_tx_hash).into(),
            output_number: (&pol_note_output_number).into(),
            aged_path,
            aged_selector,
            secret_key: (&pol_secret_key).into(),
        }
    }
}

impl From<PoQWalletInputsData> for PoQWalletInputs {
    fn from(
        PoQWalletInputsData {
            slot,
            note_value,
            transaction_hash,
            output_number,
            aged_path_and_selectors,
            secret_key,
        }: PoQWalletInputsData,
    ) -> Self {
        Self {
            pol_slot: Groth16Input::new(Fr::from(BigUint::from(slot))),
            pol_secret_key: Groth16Input::new(Fr::from(BigUint::from(secret_key))),
            pol_noteid_path_and_selectors: aged_path_and_selectors.map(|(hash, selector)| {
                (
                    hash.into(),
                    Groth16Input::new(if selector { Fr::ONE } else { Fr::ZERO }),
                )
            }),
            pol_note_tx_hash: transaction_hash.into(),
            pol_note_output_number: Groth16Input::new(Fr::from(BigUint::from(output_number))),
            pol_note_value: Groth16Input::new(Fr::from(BigUint::from(note_value))),
        }
    }
}
