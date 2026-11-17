use lb_groth16::{AdditiveGroup as _, Field as _, Fr, Groth16Input, Groth16InputDeser};
use lb_pol::NOTE_SET_TREE_DEPTH;
use num_bigint::BigUint;
use serde::Serialize;

// note commitment in its set MMR, from the leaf level up
pub type AgedNotePathAndSelectors = [(Fr, bool); NOTE_SET_TREE_DEPTH];
// set leaf in the eligible tree, from the leaf level up
pub type AgedSetPathAndSelectors = [(Fr, bool); NOTE_SET_TREE_DEPTH];
// low leaf of the note nullifier in the frozen IMT of its set, from the leaf
// level up
pub type AgedLowLeafPathAndSelectors = [(Fr, bool); NOTE_SET_TREE_DEPTH];

#[derive(Clone)]
pub struct PoQWalletInputs {
    pol_slot: Groth16Input,
    pol_nullifier_key: Groth16Input,
    pol_staking_secret_key: Groth16Input,
    pol_note_value: Groth16Input,
    pol_note_nonce: Groth16Input,
    pol_note_program: Groth16Input,
    pol_note_data: Groth16Input,
    pol_commitment_aged_path_and_selectors: [(Groth16Input, Groth16Input); NOTE_SET_TREE_DEPTH],
    pol_low_leaf_nullifier: Groth16Input,
    pol_low_leaf_next_nullifier: Groth16Input,
    pol_low_leaf_path_and_selectors: [(Groth16Input, Groth16Input); NOTE_SET_TREE_DEPTH],
    pol_set_aged_path_and_selectors: [(Groth16Input, Groth16Input); NOTE_SET_TREE_DEPTH],
}

pub struct PoQWalletInputsData {
    pub slot: u64,
    pub note_value: u64,
    pub note_nonce: Fr,
    // the program and the data of the note, 0 for a user note
    pub note_program: Fr,
    pub note_data: Fr,
    pub commitment_aged_path_and_selectors: AgedNotePathAndSelectors,
    pub low_leaf_aged_nullifier: Fr,
    pub low_leaf_aged_next_nullifier: Fr,
    pub low_leaf_aged_path_and_selectors: AgedLowLeafPathAndSelectors,
    pub set_aged_path_and_selectors: AgedSetPathAndSelectors,
    // the nullifier key derives the public key and the nullifier of the note,
    // the staking secret key its staking key and the key nullifier
    pub pol_nullifier_key: Fr,
    pub pol_staking_secret_key: Fr,
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
    pol_note_program: Groth16InputDeser,
    pol_note_data: Groth16InputDeser,
    #[serde(rename = "pol_cm_aged_path")]
    commitment_aged_path: Vec<Groth16InputDeser>,
    #[serde(rename = "pol_cm_aged_selectors")]
    commitment_aged_selectors: Vec<Groth16InputDeser>,
    #[serde(rename = "pol_low_leaf_nf")]
    low_leaf_nullifier: Groth16InputDeser,
    #[serde(rename = "pol_low_leaf_next_nf")]
    low_leaf_next_nullifier: Groth16InputDeser,
    #[serde(rename = "pol_low_leaf_path")]
    low_leaf_path: Vec<Groth16InputDeser>,
    #[serde(rename = "pol_low_leaf_selectors")]
    low_leaf_selectors: Vec<Groth16InputDeser>,
    #[serde(rename = "pol_set_aged_path")]
    set_aged_path: Vec<Groth16InputDeser>,
    #[serde(rename = "pol_set_selectors")]
    set_selectors: Vec<Groth16InputDeser>,
    pol_nullifier_key: Groth16InputDeser,
    pol_staking_secret_key: Groth16InputDeser,
}
impl From<PoQWalletInputs> for PoQWalletInputsJson {
    fn from(
        PoQWalletInputs {
            pol_slot,
            pol_nullifier_key,
            pol_staking_secret_key,
            pol_note_value,
            pol_note_nonce,
            pol_note_program,
            pol_note_data,
            pol_commitment_aged_path_and_selectors,
            pol_low_leaf_nullifier,
            pol_low_leaf_next_nullifier,
            pol_low_leaf_path_and_selectors,
            pol_set_aged_path_and_selectors,
        }: PoQWalletInputs,
    ) -> Self {
        Self {
            pol_slot: (&pol_slot).into(),
            note_value: (&pol_note_value).into(),
            note_nonce: (&pol_note_nonce).into(),
            pol_note_program: (&pol_note_program).into(),
            pol_note_data: (&pol_note_data).into(),
            commitment_aged_path: pol_commitment_aged_path_and_selectors
                .iter()
                .map(|(path, _)| path.into())
                .collect(),
            commitment_aged_selectors: pol_commitment_aged_path_and_selectors
                .iter()
                .map(|(_, selector)| selector.into())
                .collect(),
            low_leaf_nullifier: (&pol_low_leaf_nullifier).into(),
            low_leaf_next_nullifier: (&pol_low_leaf_next_nullifier).into(),
            low_leaf_path: pol_low_leaf_path_and_selectors
                .iter()
                .map(|(path, _)| path.into())
                .collect(),
            low_leaf_selectors: pol_low_leaf_path_and_selectors
                .iter()
                .map(|(_, selector)| selector.into())
                .collect(),
            set_aged_path: pol_set_aged_path_and_selectors
                .iter()
                .map(|(path, _)| path.into())
                .collect(),
            set_selectors: pol_set_aged_path_and_selectors
                .iter()
                .map(|(_, selector)| selector.into())
                .collect(),
            pol_nullifier_key: (&pol_nullifier_key).into(),
            pol_staking_secret_key: (&pol_staking_secret_key).into(),
        }
    }
}

impl From<PoQWalletInputsData> for PoQWalletInputs {
    fn from(
        PoQWalletInputsData {
            slot,
            note_value,
            note_nonce,
            note_program,
            note_data,
            commitment_aged_path_and_selectors,
            low_leaf_aged_nullifier,
            low_leaf_aged_next_nullifier,
            low_leaf_aged_path_and_selectors,
            set_aged_path_and_selectors,
            pol_nullifier_key,
            pol_staking_secret_key,
        }: PoQWalletInputsData,
    ) -> Self {
        Self {
            pol_slot: Groth16Input::new(Fr::from(BigUint::from(slot))),
            pol_nullifier_key: pol_nullifier_key.into(),
            pol_staking_secret_key: pol_staking_secret_key.into(),
            pol_note_value: Groth16Input::new(Fr::from(BigUint::from(note_value))),
            pol_note_nonce: note_nonce.into(),
            pol_note_program: note_program.into(),
            pol_note_data: note_data.into(),
            pol_commitment_aged_path_and_selectors: commitment_aged_path_and_selectors.map(
                |(hash, selector)| {
                    (
                        hash.into(),
                        Groth16Input::new(if selector { Fr::ONE } else { Fr::ZERO }),
                    )
                },
            ),
            pol_low_leaf_nullifier: low_leaf_aged_nullifier.into(),
            pol_low_leaf_next_nullifier: low_leaf_aged_next_nullifier.into(),
            pol_low_leaf_path_and_selectors: low_leaf_aged_path_and_selectors.map(
                |(hash, selector)| {
                    (
                        hash.into(),
                        Groth16Input::new(if selector { Fr::ONE } else { Fr::ZERO }),
                    )
                },
            ),
            pol_set_aged_path_and_selectors: set_aged_path_and_selectors.map(|(hash, selector)| {
                (
                    hash.into(),
                    Groth16Input::new(if selector { Fr::ONE } else { Fr::ZERO }),
                )
            }),
        }
    }
}
