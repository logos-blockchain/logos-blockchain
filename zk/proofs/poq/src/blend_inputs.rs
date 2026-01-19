use lb_groth16::{Field as _, Fr, Groth16Input, Groth16InputDeser};
use serde::{Deserialize, Serialize};

pub const CORE_MERKLE_TREE_HEIGHT: usize = 20;
pub type CorePathAndSelectors = [(Fr, bool); CORE_MERKLE_TREE_HEIGHT];

#[derive(Clone)]
pub struct PoQBlendInputs {
    pub core_sk: Groth16Input,
    pub core_path_and_selectors: [(Groth16Input, Groth16Input); CORE_MERKLE_TREE_HEIGHT],
}

pub struct PoQBlendInputsData {
    pub core_sk: Fr,
    pub core_path_and_selectors: CorePathAndSelectors,
}

#[derive(Deserialize, Serialize)]
pub struct PoQBlendInputsJson {
    core_sk: Groth16InputDeser,
    core_path: [Groth16InputDeser; CORE_MERKLE_TREE_HEIGHT],
    core_path_selectors: [Groth16InputDeser; CORE_MERKLE_TREE_HEIGHT],
}

impl From<PoQBlendInputs> for PoQBlendInputsJson {
    fn from(
        PoQBlendInputs {
            core_sk,
            core_path_and_selectors,
        }: PoQBlendInputs,
    ) -> Self {
        let (core_path, core_path_selectors) = {
            let core_path = core_path_and_selectors.map(|(path, _)| (&path).into());
            let core_path_selectors =
                core_path_and_selectors.map(|(_, selector)| (&selector).into());

            (core_path, core_path_selectors)
        };
        Self {
            core_sk: (&core_sk).into(),
            core_path,
            core_path_selectors,
        }
    }
}

impl From<PoQBlendInputsData> for PoQBlendInputs {
    fn from(
        PoQBlendInputsData {
            core_sk,
            core_path_and_selectors,
        }: PoQBlendInputsData,
    ) -> Self {
        Self {
            core_sk: core_sk.into(),
            core_path_and_selectors: core_path_and_selectors.map(|(value, selector)| {
                (
                    value.into(),
                    Groth16Input::new(if selector { Fr::ONE } else { Fr::ZERO }),
                )
            }),
        }
    }
}
