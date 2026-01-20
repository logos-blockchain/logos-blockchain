//! # zk-poc
//!
//! ## Usage
//!
//! The library provides a single function, `prove`, which takes a set of
//! inputs and generates a proof. The function returns a tuple containing the
//! generated proof and the corresponding public inputs.
//! A normal flow of usage will involve the following steps:
//! 1. Fill out some `PoCChainData` with the public inputs
//! 2. Fill out some `PoCWalletData` with the private inputs
//! 3. Construct the `PoCWitnessInputs` from the `PoCChainData` and
//!    `PoCWalletData`
//! 4. Call `prove` with the `PoCWitnessInputs`
//! 5. Use the returned proof and public inputs to verify the proof
//!
//! ## Example
//!
//! ```ignore
//! use zk_poc::{prove, PoCChainInputs, PoCChainInputsData, PoCWalletInputs, PoCWalletInputsData};
//!
//! fn main() {
//!     let chain_data = PoCChainInputsData {..};
//!     let wallet_data = PoCWalletInputsData {..};
//!     let witness_inputs = PoCWitnessInputs::from_chain_and_wallet_data(chain_data, wallet_data);
//!     let (proof, inputs) = prove(&witness_inputs).unwrap();
//!     assert!(verify(&proof, &inputs).unwrap());
//! }

mod chain_inputs;
mod inputs;
mod proving_key;
mod verification_key;
mod wallet_inputs;
mod witness;

use core::fmt::Debug;
use std::error::Error;

pub use chain_inputs::{PoCChainInputs, PoCChainInputsData};
pub use inputs::PoCWitnessInputs;
use lb_groth16::{
    CompressedGroth16Proof, Groth16Input, Groth16InputDeser, Groth16Proof, Groth16ProofJsonDeser,
};
use thiserror::Error;
pub use wallet_inputs::{PoCWalletInputs, PoCWalletInputsData};
pub use witness::Witness;

use crate::{
    inputs::{PoCVerifierInput, PoCVerifierInputJson},
    proving_key::POC_PROVING_KEY_PATH,
};

pub type PoCProof = CompressedGroth16Proof;

#[derive(Debug, Error)]
pub enum ProveError {
    #[error(transparent)]
    Io(std::io::Error),
    #[error(transparent)]
    Json(serde_json::Error),
    #[error("Error parsing Groth16 input: {0:?}")]
    Groth16JsonInput(<Groth16Input as TryFrom<Groth16InputDeser>>::Error),
    #[error(transparent)]
    Groth16JsonProof(<Groth16Proof as TryFrom<Groth16ProofJsonDeser>>::Error),
}

///
/// This function generates a proof for the given set of inputs.
///
/// # Arguments
/// - `inputs`: A reference to `PoCWitnessInputs`, which contains the necessary
///   data to generate the witness and construct the proof.
///
/// # Returns
/// - `Ok((PoCProof, PoCVerifierInput))`: On success, returns a tuple containing
///   the generated proof (`PoCProof`) and the corresponding public inputs
///   (`PoCVerifierInput`).
/// - `Err(ProveError)`: On failure, returns an error of type `ProveError`,
///   which can occur due to I/O errors or JSON (de)serialization errors.
///
/// # Errors
/// - Returns a `ProveError::Io` if an I/O error occurs while generating the
///   witness or proving from contents.
/// - Returns a `ProveError::Json` if there is an error during JSON
///   serialization or deserialization.
pub fn prove(inputs: &PoCWitnessInputs) -> Result<(PoCProof, PoCVerifierInput), ProveError> {
    let witness = witness::generate_witness(inputs).map_err(ProveError::Io)?;
    let (proof, verifier_inputs) =
        lb_circuits_prover::prover_from_contents(POC_PROVING_KEY_PATH.as_path(), witness.as_ref())
            .map_err(ProveError::Io)?;
    let proof: Groth16ProofJsonDeser = serde_json::from_slice(&proof).map_err(ProveError::Json)?;
    let verifier_inputs: PoCVerifierInputJson =
        serde_json::from_slice(&verifier_inputs).map_err(ProveError::Json)?;
    let proof: Groth16Proof = proof.try_into().map_err(ProveError::Groth16JsonProof)?;
    Ok((
        CompressedGroth16Proof::try_from(&proof).unwrap(),
        verifier_inputs
            .try_into()
            .map_err(ProveError::Groth16JsonInput)?,
    ))
}

#[derive(Debug)]
pub enum VerifyError {
    Expansion,
    ProofVerify(Box<dyn Error>),
}

///
/// This function verifies a proof against a set of public inputs.
///
/// # Arguments
///
/// - `proof`: A reference to the proof (`PoCProof`) that needs verification.
/// - `public_inputs`: A reference to `PoCVerifierInput`, which contains the
///   public inputs against which the proof is verified.
///
/// # Returns
///
/// - `Ok(true)`: If the proof is successfully verified against the public
///   inputs.
/// - `Ok(false)`: If the proof is invalid when compared with the public inputs.
/// - `Err`: If an error occurs during the verification process.
///
/// # Errors
///
/// - Returns an error if there is an issue with the verification key or the
///   underlying verification process fails.
pub fn verify(proof: &PoCProof, public_inputs: &PoCVerifierInput) -> Result<bool, VerifyError> {
    let inputs = public_inputs.to_inputs();
    let expanded_proof = Groth16Proof::try_from(proof).map_err(|_| VerifyError::Expansion)?;
    lb_groth16::groth16_verify(verification_key::POC_VK.as_ref(), &expanded_proof, &inputs)
        .map_err(|e| VerifyError::ProofVerify(Box::new(e)))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use num_bigint::BigUint;

    use super::*;

    #[test]
    fn test_full_flow() {
        let chain_data = PoCChainInputsData {
            voucher_root: BigUint::from_str(
                "4694207789091791842767403892845814582412778251658086141388247386912594634423",
            )
            .unwrap()
            .into(),
            mantle_tx_hash: BigUint::from_str(
                "825129823987156176788572577965633113263714540089141007937027440495856488048",
            )
            .unwrap()
            .into(),
        };
        let wallet_data = PoCWalletInputsData {
            secret_voucher: BigUint::from_str(
                "13243819124136744636166793246357979658061323366812242500631238497607425128018",
            )
            .unwrap()
            .into(),
            voucher_merkle_path: [
                "13486337320733907330010546950370351844673817587612426971506452705298326031473",
                "6224193506265526448637771339270091520305376546296886520247427351480863734266",
                "6567499370713843810819271087662217590656396941930315146743609759198754940570",
                "13494491356065907652246909636302131401078485893019417390813203693514522977906",
                "17849253425324902809518045756166048294144378287211807440747495189122601459019",
                "5140896134581367664722372800715282016288820633156108219076360964489520131881",
                "19114130069857062191260737695617035267713981721196836878802394056748410464681",
                "13019384442202167768520326456274945008730617376564863716408545205299666754959",
                "11205282490624635464094339686506363388297238420955840445457962750287665495821",
                "7988063627799620087165205030983895952496352849209499138130810328150243231380",
                "10798677067717778974824770675808239551576455405312903121312889709004876132374",
                "9164718197625713018148236595386458811928091118418548328261930587656145438223",
                "11405337654528852642134489221464792628639614486235621929335000871580307551174",
                "13167972235865493894917743609682685044552664268947587892267231276263979752221",
                "9026542248065545660446398068062700544487456544532813762269777507157103817775",
                "17613027152373254068272763545712130211542359655065695200356672471032242940295",
                "7484005040933111792051759192184465723267069869609748643574783112649861694763",
                "4589965457494320971633738056129766158458955050954466569368407525254167587440",
                "6768812507910859897712341494676426634516953117847345931483876498428284716825",
                "11794237730678025418895512858422757517711285819026635460374140695459188703346",
                "16518863605029356855354914197401958283083428138335866729257330092745202070547",
                "20148700888595516932816876717291006332617741999497420338204048752556141640194",
                "5586637360667679143399755683034926364416159048246727169282229126426563097233",
                "15168751100320422031564933640366618447254404845230006822312017634519914352173",
                "7890810180469084427324454877052705430280334324489783393200362042580230098221",
                "6262912585699446136015308623507292052825697125913925579005942588182626230205",
                "140163579323849698377273841930015875116611019927691532671290856544269768353",
                "13362034579449105354310180422317900430267793013938141447731171784947008543146",
                "12646542999637111903372820523810106463134095358794052109514745122984515974444",
                "3218464671766447041523606538331126741874161413042484633435444095746544529155",
                "6761973853748840902874998979943390731680974635284124116058530257565654384071",
                "20251898099693466062391100655318262518310844013768315826978801732242229964307",
            ]
            .into_iter()
            .map(|value| BigUint::from_str(value).unwrap().into())
            .collect(),
            voucher_merkle_path_selectors: [
                "0", "1", "1", "0", "0", "1", "0", "0", "0", "0", "0", "0", "0", "1", "0", "1",
                "0", "1", "0", "0", "1", "0", "1", "0", "1", "1", "0", "1", "1", "1", "0", "0",
            ]
            .into_iter()
            .map(|s| match s {
                "1" => true,
                "0" => false,
                _ => panic!("Invalid value for aged_selector"),
            })
            .collect(),
        };
        let witness_inputs = PoCWitnessInputs::from_chain_and_wallet_data(chain_data, wallet_data);

        let (proof, inputs) = prove(&witness_inputs).unwrap();
        assert!(verify(&proof, &inputs).unwrap());
    }
}
