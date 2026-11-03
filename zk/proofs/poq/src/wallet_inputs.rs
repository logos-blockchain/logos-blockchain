use lb_groth16::{AdditiveGroup as _, Field as _, Fr, Groth16Input, Groth16InputDeser};
use lb_pol::NOTE_SET_TREE_DEPTH;
use num_bigint::BigUint;
use serde::Serialize;

// note commitment in its set MMR, from the leaf level up
pub type AgedNotePathAndSelectors = [(Fr, bool); NOTE_SET_TREE_DEPTH];
// set leaf in the eligible tree, from the leaf level up
pub type AgedSetPathAndSelectors = [(Fr, bool); NOTE_SET_TREE_DEPTH];

#[derive(Clone)]
pub struct PoQWalletInputs {
    pol_slot: Groth16Input,
    pol_secret_key: Groth16Input,
    pol_note_value: Groth16Input,
    pol_note_nonce: Groth16Input,
    pol_commitment_aged_path_and_selectors: [(Groth16Input, Groth16Input); NOTE_SET_TREE_DEPTH],
    pol_nullifier_root_aged: Groth16Input,
    pol_set_aged_path_and_selectors: [(Groth16Input, Groth16Input); NOTE_SET_TREE_DEPTH],
}

pub struct PoQWalletInputsData {
    pub slot: u64,
    pub note_value: u64,
    pub note_nonce: Fr,
    pub commitment_aged_path_and_selectors: AgedNotePathAndSelectors,
    pub nullifier_root_aged: Fr,
    pub set_aged_path_and_selectors: AgedSetPathAndSelectors,
    pub pol_secret_key: Fr,
}

#[derive(Serialize)]
pub struct PoQWalletInputsJson {
    #[serde(rename = "pol_sl")]
    pol_slot: Groth16InputDeser,
    #[serde(rename = "pol_note_value")]
    note_value: Groth16InputDeser,
    // serde serializes arrays only up to length 32
    #[serde(rename = "pol_note_nonce")]
    note_nonce: Groth16InputDeser,
    #[serde(rename = "pol_cm_aged_path")]
    commitment_aged_path: Vec<Groth16InputDeser>,
    #[serde(rename = "pol_cm_aged_selectors")]
    commitment_aged_selectors: Vec<Groth16InputDeser>,
    #[serde(rename = "pol_nf_root_aged")]
    nullifier_root_aged: Groth16InputDeser,
    #[serde(rename = "pol_set_aged_path")]
    set_aged_path: Vec<Groth16InputDeser>,
    #[serde(rename = "pol_set_selectors")]
    set_selectors: Vec<Groth16InputDeser>,
    #[serde(rename = "pol_secret_key")]
    pol_secret_key: Groth16InputDeser,
}
impl From<PoQWalletInputs> for PoQWalletInputsJson {
    fn from(
        PoQWalletInputs {
            pol_slot,
            pol_secret_key,
            pol_note_value,
            pol_note_nonce,
            pol_commitment_aged_path_and_selectors,
            pol_nullifier_root_aged,
            pol_set_aged_path_and_selectors,
        }: PoQWalletInputs,
    ) -> Self {
        Self {
            pol_slot: (&pol_slot).into(),
            note_value: (&pol_note_value).into(),
            note_nonce: (&pol_note_nonce).into(),
            commitment_aged_path: pol_commitment_aged_path_and_selectors
                .iter()
                .map(|(path, _)| path.into())
                .collect(),
            commitment_aged_selectors: pol_commitment_aged_path_and_selectors
                .iter()
                .map(|(_, selector)| selector.into())
                .collect(),
            nullifier_root_aged: (&pol_nullifier_root_aged).into(),
            set_aged_path: pol_set_aged_path_and_selectors
                .iter()
                .map(|(path, _)| path.into())
                .collect(),
            set_selectors: pol_set_aged_path_and_selectors
                .iter()
                .map(|(_, selector)| selector.into())
                .collect(),
            pol_secret_key: (&pol_secret_key).into(),
        }
    }
}

impl From<PoQWalletInputsData> for PoQWalletInputs {
    fn from(
        PoQWalletInputsData {
            slot,
            note_value,
            note_nonce,
            commitment_aged_path_and_selectors,
            nullifier_root_aged,
            set_aged_path_and_selectors,
            pol_secret_key,
        }: PoQWalletInputsData,
    ) -> Self {
        Self {
            pol_slot: Groth16Input::new(Fr::from(BigUint::from(slot))),
            pol_secret_key: pol_secret_key.into(),
            pol_note_value: Groth16Input::new(Fr::from(BigUint::from(note_value))),
            pol_note_nonce: note_nonce.into(),
            pol_commitment_aged_path_and_selectors: commitment_aged_path_and_selectors.map(
                |(hash, selector)| {
                    (
                        hash.into(),
                        Groth16Input::new(if selector { Fr::ONE } else { Fr::ZERO }),
                    )
                },
            ),
            pol_nullifier_root_aged: nullifier_root_aged.into(),
            pol_set_aged_path_and_selectors: set_aged_path_and_selectors.map(|(hash, selector)| {
                (
                    hash.into(),
                    Groth16Input::new(if selector { Fr::ONE } else { Fr::ZERO }),
                )
            }),
        }
    }
}
