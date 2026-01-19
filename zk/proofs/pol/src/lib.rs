//! # zk-pol
//!
//! ## Usage
//!
//! The library provides a single function, `prove`, which takes a set of
//! inputs and generates a proof. The function returns a tuple containing the
//! generated proof and the corresponding public inputs.
//! A normal flow of usage will involve the following steps:
//! 1. Fill out some `PolChainData` with the public inputs
//! 2. Fill out some `PolWalletData` with the private inputs
//! 3. Construct the `PolWitnessInputs` from the `PolChainData` and
//!    `PolWalletData`
//! 4. Call `prove` with the `PolWitnessInputs`
//! 5. Use the returned proof and public inputs to verify the proof
//!
//! ## Example
//!
//! ```ignore
//! use zk_pol::{prove, PolChainInputs, PolChainInputsData, PolWalletInputs, PolWalletInputsData};
//!
//! fn main() {
//!     let chain_data = PolChainInputsData {..};
//!     let wallet_data = PolWalletInputsData {..};
//!     let witness_inputs = PolWitnessInputs::from_chain_and_wallet_data(chain_data, wallet_data).unwrap();
//!     let (proof, inputs) = prove(&witness_inputs).unwrap();
//!     assert!(verify(&proof, &inputs).unwrap());
//! }

mod chain_inputs;
mod inputs;
mod lottery;
mod proving_key;
mod verification_key;
mod wallet_inputs;
mod witness;

use std::error::Error;

pub use chain_inputs::{PolChainInputs, PolChainInputsData};
use groth16::{
    CompressedGroth16Proof, Groth16Input, Groth16InputDeser, Groth16Proof, Groth16ProofJsonDeser,
};
pub use inputs::{PolVerifierInput, PolWitnessInputs, PolWitnessInputsData};
use thiserror::Error;
pub use wallet_inputs::{PolWalletInputs, PolWalletInputsData};
pub use witness::Witness;

pub use crate::lottery::{P, T0_CONSTANT, T1_CONSTANT, compute_lottery_values};
use crate::{inputs::PolVerifierInputJson, proving_key::POL_PROVING_KEY_PATH};

pub type PoLProof = CompressedGroth16Proof;

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
/// - `inputs`: A reference to `PolWitnessInputs`, which contains the necessary
///   data to generate the witness and construct the proof.
///
/// # Returns
/// - `Ok((PoLProof, PolVerifierInput))`: On success, returns a tuple containing
///   the generated proof (`PoLProof`) and the corresponding public inputs
///   (`PolVerifierInput`).
/// - `Err(ProveError)`: On failure, returns an error of type `ProveError`,
///   which can occur due to I/O errors or JSON (de)serialization errors.
///
/// # Errors
/// - Returns a `ProveError::Io` if an I/O error occurs while generating the
///   witness or proving from contents.
/// - Returns a `ProveError::Json` if there is an error during JSON
///   serialization or deserialization.
pub fn prove(inputs: &PolWitnessInputs) -> Result<(PoLProof, PolVerifierInput), ProveError> {
    let witness = witness::generate_witness(inputs).map_err(ProveError::Io)?;
    let (proof, verifier_inputs) =
        circuits_prover::prover_from_contents(POL_PROVING_KEY_PATH.as_path(), witness.as_ref())
            .map_err(ProveError::Io)?;
    let proof: Groth16ProofJsonDeser = serde_json::from_slice(&proof).map_err(ProveError::Json)?;
    let verifier_inputs: PolVerifierInputJson =
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
/// - `proof`: A reference to the proof (`PoLProof`) that needs verification.
/// - `public_inputs`: A reference to `PolVerifierInput`, which contains the
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
pub fn verify(proof: &PoLProof, public_inputs: &PolVerifierInput) -> Result<bool, VerifyError> {
    let inputs = public_inputs.to_inputs();
    let expanded_proof = Groth16Proof::try_from(proof).map_err(|_| VerifyError::Expansion)?;
    groth16::groth16_verify(verification_key::POL_VK.as_ref(), &expanded_proof, &inputs)
        .map_err(|e| VerifyError::ProofVerify(Box::new(e)))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use groth16::Fr;
    use num_bigint::BigUint;

    use super::*;

    #[expect(clippy::too_many_lines, reason = "For the sake of the test let it be")]
    #[test]
    fn test_full_flow() {
        let chain_data = PolChainInputsData {
            slot_number: 135,
            epoch_nonce: Fr::from(510u64),
            total_stake: 5000,
            aged_root: BigUint::from_str(
                "16524395010779500501330992017298834046369952285388149958144954382059764408785",
            )
            .unwrap()
            .into(),
            latest_root: BigUint::from_str(
                "20834259014818668135947196643658285248830563263522051603918285129600429840356",
            )
            .unwrap()
            .into(),
            leader_pk: (
                BigUint::from(123_456u32).into(),
                BigUint::from(654_321u32).into(),
            ),
        };
        let wallet_data = PolWalletInputsData {
            note_value: 50,
            transaction_hash: BigUint::from_str(
                "13842549853504288180344790242994935826382638205146161564439947279024930588677",
            )
            .unwrap()
            .into(),
            output_number: 8313,
            aged_path: [
                "19205963370485772287624660148655123421630250475080881163694783019817455316910",
                "842632995957934154490829566755258332089735588762651714354950816371199202948",
                "11945865010023840792771634133332309536300021618163415115524743891577396784532",
                "5678144262683681210286056464240523249603444097705978250116845087015739879001",
                "4543130200496840387394591809962125944112292744388860937179706396873037283532",
                "447945249172309706147685180974075378036414184792819934794462245951881132491",
                "10984937925416446764477546426399361164722183078917024384765615640208309773886",
                "8332301048884746072225650742152218830832205382305719788443252112373451390008",
                "7717740854062780302371479099548413295076442606971953244362182813969746810145",
                "5870767669864048222619695329761075412560256425762658309939091044024798841980",
                "1270394570818312305068877518098833848392252257574032052368322099978490528993",
                "4421806191049823072310631508282545862661318788050986720831912352661110024704",
                "9543220744965548298592940239013154329526126759818391282972665149911444460380",
                "11832520211727528097498428496026205325545655289771652718959221711574756323845",
                "8261267697784487706399178572428457281508840530987009187944836942560051990845",
                "7259688284711792346738578773128765401934616369296796752701532318548818675656",
                "20410766059815893992135193121776619328345084011501351449727415160723164671894",
                "19409422102345646102663034240923589054723801239665575001424649313393572448797",
                "2026478415805648536252469432565155067383923909238350287385051253116157491081",
                "21072644004334270034357972934702520036515345635901155899590483624455566877570",
                "12706352973548089350218643189492694819549197991366076961997024182862825891572",
                "20651842981778843765422134357942811568755148902817286832703401476578926932159",
                "19097976423347244117755030245169411514551470052335850544780149827855495070628",
                "10254517281922373005124180252373170574492353445403856807250684829458178341748",
                "4828393086141028784920329285136836834160589347467272527124976967197826969967",
                "7290848553264685940106614387259971050759824319302446980477663653328347224981",
                "13568116581247600056306081066087207159958446188434534967837954912335560567528",
                "19673474827150301713419447780509555813516015784591535595280509514259612835348",
                "14016712741485891526208755187452390849270211823944034841285668141997053149398",
                "20984444386080019249642854750185394191547415630952463570781495995361188304235",
                "20716527055704913432992003250600357730017309147012913323969779133930909800072",
                "14787709838677865776105327831675542255717739581860994014618609782788824576885",
            ]
            .into_iter()
            .map(|value| BigUint::from_str(value).unwrap().into())
            .collect(),
            aged_selector: [
                "0", "1", "1", "0", "1", "1", "0", "1", "0", "0", "1", "0", "1", "0", "0", "1",
                "0", "1", "1", "1", "1", "0", "1", "1", "1", "0", "0", "0", "0", "1", "1", "1",
            ]
            .into_iter()
            .map(|s| match s {
                "1" => true,
                "0" => false,
                _ => panic!("Invalid value for aged_selector"),
            })
            .collect(),
            latest_path: [
                "11709948088963960065647371537879293701565786386460016885512089239291870378840",
                "2120901090324525908662474041962168618570906102338290469215218624292046179330",
                "10497564299466181298229172082520996337033686748361035747293006440896232093004",
                "3425260074297640180775893568433815888755716019399506534916465904285025646859",
                "4583167269494370304032520085783972434215767473666242416208450810280024023142",
                "12731396449267797964642841435803473321701941536173578364548242959504253386489",
                "20876509042032154075839888677829108280893892531171309282531399417530514655652",
                "19667250381457364671121915728778240317672011326702060802555641657494453986631",
                "20151748203430641227294477190013185951512285826753694689236792791802228236409",
                "5296503914834159504532891530796715207930043126952673371378640399578370383358",
                "15204004164053373401239131144856853301410255568638848470242417306501189833079",
                "8433140840890890569751171491136473504934304014919055457596233419295898933678",
                "10455135882248302013748933783036206895663823903064929584400314540485565598947",
                "6618514926108954914636276316487586150653225326091109610916623668888506574898",
                "8472865127566208631416954061285083236000644409874339646609286596717483042203",
                "5163990834740603021957302291480735541147224500392813452221842181941789763448",
                "21866665746348412966861756211614206576737908673885956395292046005817224269106",
                "17218628924715383670149819392265573714457473274338484988014313554129536528070",
                "13526373749917680118104762439292019605872872007383862555414186681846583503283",
                "14211156411104090766360751999017487063048079319839607558904054235600169155160",
                "11718317735557902839618433161716283335065018548585948157910312975459461756987",
                "16829743339703043966744159514162467606976583091056897908269705566108589164163",
                "6011104885108544056095362574030165839534992991860687552312346356400158313655",
                "1562596472155451649509003097294568270949518819233462734625356240396837358704",
                "12754783870620687365721563814820635276715427311805413156076695645151610773481",
                "8290249361628686401202188242691875538991713007249069067400817079004557471604",
                "14976734201356719360023861818781680042905193161258424609193595233171839570457",
                "10727243010131564083538628277491833886599073630784978208903889116861519604518",
                "14768425552740035340703385928185193645196603681658457264517638180469299892809",
                "15946835915681372862173515288501920089675395784378131928249939063433663089244",
                "18200637942483578342976888834386216643273358976477818116888783794758722873887",
                "15874955117141602197688150287249257989589905293755369275305245316881887378284",
            ]
            .into_iter()
            .map(|value| BigUint::from_str(value).unwrap().into())
            .collect(),
            latest_selector: [
                "0", "1", "0", "0", "1", "1", "0", "0", "0", "0", "1", "0", "0", "1", "1", "1",
                "1", "1", "1", "0", "0", "1", "0", "1", "0", "0", "1", "1", "1", "1", "1", "0",
            ]
            .into_iter()
            .map(|s| match s {
                "1" => true,
                "0" => false,
                _ => panic!("Invalid value for aged_selector"),
            })
            .collect(),
            secret_key: BigUint::from_str(
                "7897218687652577456193628084912129251352759708723100638805247670738317482408",
            )
            .unwrap()
            .into(),
        };
        let witness_inputs =
            PolWitnessInputsData::from_chain_and_wallet_data(chain_data, wallet_data);

        let (proof, inputs) = prove(&witness_inputs.into()).unwrap();
        assert!(verify(&proof, &inputs).unwrap());
    }
}
