mod blend_inputs;
mod chain_inputs;
mod common_inputs;
mod inputs;
mod proving_key;
mod verification_key;
mod wallet_inputs;
mod witness;

use std::error::Error;

pub use blend_inputs::{
    CORE_MERKLE_TREE_HEIGHT, CorePathAndSelectors, PoQBlendInputs, PoQBlendInputsData,
};
pub use chain_inputs::{PoQChainInputs, PoQChainInputsData, PoQInputsFromDataError};
pub use common_inputs::{PoQCommonInputs, PoQCommonInputsData};
use groth16::{
    CompressedGroth16Proof, Groth16Input, Groth16InputDeser, Groth16Proof, Groth16ProofJsonDeser,
};
pub use inputs::{PoQVerifierInput, PoQVerifierInputData, PoQWitnessInputs};
use thiserror::Error;
pub use wallet_inputs::{
    AGED_NOTE_MERKLE_TREE_HEIGHT, NotePathAndSelectors, PoQWalletInputs, PoQWalletInputsData,
};
pub use witness::Witness;

use crate::{inputs::PoQVerifierInputJson, proving_key::POQ_PROVING_KEY_PATH};

pub type PoQProof = CompressedGroth16Proof;

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
/// - `inputs`: A reference to `PoQWitnessInputs`, which contains the necessary
///   data to generate the witness and construct the proof.
///
/// # Returns
/// - `Ok((PoQProof, PoQVerifierInput))`: On success, returns a tuple containing
///   the generated proof (`PoQProof`) and the corresponding public inputs
///   (`PoQVerifierInput`).
/// - `Err(ProveError)`: On failure, returns an error of type `ProveError`,
///   which can occur due to I/O errors or JSON (de)serialization errors.
///
/// # Errors
/// - Returns a `ProveError::Io` if an I/O error occurs while generating the
///   witness or proving from contents.
/// - Returns a `ProveError::Json` if there is an error during JSON
///   serialization or deserialization.
pub fn prove(inputs: PoQWitnessInputs) -> Result<(PoQProof, PoQVerifierInput), ProveError> {
    let witness = witness::generate_witness(inputs).map_err(ProveError::Io)?;
    let (proof, verifier_inputs) =
        circuits_prover::prover_from_contents(POQ_PROVING_KEY_PATH.as_path(), witness.as_ref())
            .map_err(ProveError::Io)?;
    let proof: Groth16ProofJsonDeser = serde_json::from_slice(&proof).map_err(ProveError::Json)?;
    let verifier_inputs: PoQVerifierInputJson =
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
/// - `proof`: A reference to the proof (`PoQProof`) that needs verification.
/// - `public_inputs`: A reference to `PoQVerifierInput`, which contains the
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
pub fn verify(proof: &PoQProof, public_inputs: PoQVerifierInput) -> Result<bool, VerifyError> {
    let inputs = public_inputs.to_inputs();
    let expanded_proof = Groth16Proof::try_from(proof).map_err(|_| VerifyError::Expansion)?;
    groth16::groth16_verify(verification_key::POQ_VK.as_ref(), &expanded_proof, &inputs)
        .map_err(|e| VerifyError::ProofVerify(Box::new(e)))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use num_bigint::BigUint;

    use super::*;

    #[test]
    #[expect(clippy::too_many_lines, reason = "Test function.")]
    fn test_core_node_full_flow() {
        let blend_data = PoQBlendInputsData {
            core_sk: BigUint::from_str(
                "6576495977526760241501499963811136028674473098047722736567052812753357178145",
            )
            .unwrap()
            .into(),
            core_path_and_selectors: [
                (
                    "13664394408134714148474230123476522725121022841643115538772760906625244311247",
                    false,
                ),
                (
                    "19991080109804084229148819955447485980685271866236940378355513894509403472825",
                    false,
                ),
                (
                    "1719423980074249650282196742833822060536614400613876573240720330836944657046",
                    true,
                ),
                (
                    "9139059421670197230454683564023894631544291367010097325710917953251896109820",
                    true,
                ),
                (
                    "5364655248740251237070591917425080374871240308613634110599105641060308262480",
                    false,
                ),
                (
                    "4321492523488456904492279703871677986518013774282254436225897785632983582392",
                    false,
                ),
                (
                    "2990224148203398705081847530695215902031679797575769158839150593627687374477",
                    true,
                ),
                (
                    "10115668570671746589151792513284313334215992216493690986604010084064526087897",
                    false,
                ),
                (
                    "6861547258030107481535610157091754819359096245516659866713074178195667725661",
                    false,
                ),
                (
                    "7263098007194293142502159691381240540663513841811088748578355471278661748398",
                    true,
                ),
                (
                    "19870876389339998317953427375552419155167215853924118334240834433312282250010",
                    true,
                ),
                (
                    "8986537640578106247026054824010082082887537390181434985471297569735311570527",
                    true,
                ),
                (
                    "2677828888032047082692090535060454810065355495500936244017540612071241172074",
                    false,
                ),
                (
                    "17555659938930335545491158932916791395314469587225130013830235195468776678581",
                    true,
                ),
                (
                    "9071399479181997918431555676567419518917037685697025275563780306843446304571",
                    false,
                ),
                (
                    "6281992864280083991852918191693022891803454129450254484599792207873685708287",
                    false,
                ),
                (
                    "17218725285443376817865240748479799866804154572691475341349156521930190101705",
                    false,
                ),
                (
                    "14439152042823579940992841514440450319175689090721997229482427278436880180442",
                    true,
                ),
                (
                    "4375092350636859316334567236773062868645005901112681926345752432026107616905",
                    true,
                ),
                (
                    "2391466682452790260991116539507901841740149925567473331054306831262572734122",
                    false,
                ),
            ]
            .map(|(value, selector)| (BigUint::from_str(value).unwrap().into(), selector)),
        };
        let chain_data = PoQChainInputsData {
            session: 150,
            core_root: BigUint::from_str(
                "10774149910279330054096178616484626574938100628643657398591620611653283350567",
            )
            .unwrap()
            .into(),
            pol_ledger_aged: BigUint::from_str(
                "5899427723120145663289932094176488517692686385716511263841021495659364229709",
            )
            .unwrap()
            .into(),
            pol_epoch_nonce: BigUint::from_str(
                "14296132653141589082634452040005706132398442975462231668085056116416860738326",
            )
            .unwrap()
            .into(),
            total_stake: 5000,
        };
        let common_data = PoQCommonInputsData {
            core_quota: 15,
            leader_quota: 10,
            message_key: (
                BigUint::from(123_456u32).into(),
                BigUint::from(654_321u32).into(),
            ),
            selector: false,
            index: 6,
        };

        let witness_inputs =
            PoQWitnessInputs::from_core_node_data(chain_data, common_data, blend_data).unwrap();
        let (proof, inputs) = prove(witness_inputs).unwrap();
        let key_nullifier = inputs.key_nullifier.into_inner();
        // Test that verifying with the inputs returned by `prove` works.
        assert!(verify(&proof, inputs).unwrap());

        // Test that verifying with the reconstructed inputs inside the verifier context
        // works.
        let recomputed_verify_inputs = PoQVerifierInputData {
            core_quota: common_data.core_quota,
            core_root: chain_data.core_root,
            k_part_one: common_data.message_key.0,
            k_part_two: common_data.message_key.1,
            key_nullifier,
            leader_quota: common_data.leader_quota,
            pol_epoch_nonce: chain_data.pol_epoch_nonce,
            pol_ledger_aged: chain_data.pol_ledger_aged,
            session: chain_data.session,
            total_stake: chain_data.total_stake,
        };
        assert!(verify(&proof, recomputed_verify_inputs.into()).unwrap());
    }

    #[expect(clippy::too_many_lines, reason = "For the sake of the test let it be")]
    #[test]
    fn test_leader_full_flow() {
        let chain_data = PoQChainInputsData {
            session: 150,
            core_root: BigUint::from_str(
                "11932007478822307154060471648284351639702201082133930350572683284818742022376",
            )
            .unwrap()
            .into(),
            pol_ledger_aged: BigUint::from_str(
                "3342945051364076680578394748596576255113072679391591642880837961076314972848",
            )
            .unwrap()
            .into(),
            pol_epoch_nonce: BigUint::from_str(
                "11175261890624684044649299918101786672368403302354398651534872142981877782699",
            )
            .unwrap()
            .into(),
            total_stake: 5000,
        };
        let common_data = PoQCommonInputsData {
            core_quota: 15,
            leader_quota: 10,
            message_key: (
                BigUint::from(123_456u32).into(),
                BigUint::from(654_321u32).into(),
            ),
            selector: true,
            index: 6,
        };
        let wallet_data = PoQWalletInputsData {
            slot: 2_966_761_920,
            note_value: 50,
            transaction_hash: BigUint::from_str(
                "2329282056243366650798295627018033051629488564285541185679647538827693341905",
            )
            .unwrap()
            .into(),
            output_number: 939,
            aged_path_and_selectors: [
                (
                    "11421803042917225627078901218595334185635793494732072039365745057621038812605",
                    true,
                ),
                (
                    "16169325055187512554252523735961738240564294866494072628885024705839052661540",
                    true,
                ),
                (
                    "13656910552343360495181196437444817159012126550371459801043514651056011592431",
                    false,
                ),
                (
                    "12449448263899051439932276028399740866954101672665020621064512907852896180600",
                    true,
                ),
                (
                    "9885876318176059648673564567836315509808157847432213345713880847517777412402",
                    false,
                ),
                (
                    "8388384044942534377652016263681525995454565014077139337139691792794605158759",
                    false,
                ),
                (
                    "19235582700862041818897072745866691959011049676952434709510674286292920807586",
                    true,
                ),
                (
                    "20395204849666255127128061293127220286095933826612425507561746733356595329205",
                    true,
                ),
                (
                    "9792237822684654427827473660300770828015365397927307769302890576073867096291",
                    true,
                ),
                (
                    "14079764112025655291976470097660114251011231109847561011521815867773307300092",
                    true,
                ),
                (
                    "2067627040071925958630121165903165914396130574920765783200205766592922273940",
                    true,
                ),
                (
                    "11868870016995490053316691978613163802307585966396648283452535241128386725435",
                    false,
                ),
                (
                    "7279049315994133684852941934863027518847276009575066241366658887613953973411",
                    false,
                ),
                (
                    "1653700858957932143991083596354839511712596833499052382473339976736220545455",
                    true,
                ),
                (
                    "14593791812533011392330970050125225769124415454833286335602775158366056740177",
                    true,
                ),
                (
                    "15914545113232326797695053101280159396305447124529609345850784179045489818165",
                    true,
                ),
                (
                    "12662726781317585032946137898723100436309385600280503938290557115123483697307",
                    true,
                ),
                (
                    "12794954106056182456059399831190158694070172097205191410351234102340878136610",
                    false,
                ),
                (
                    "9139468540558771360630099778350062729951097762632752410540325438044212613391",
                    false,
                ),
                (
                    "14427444938386674320013309938491006982580864902951055386283865455921637245718",
                    false,
                ),
                (
                    "16217323319160426166397199974441670931472458277592563266744427901413561452567",
                    true,
                ),
                (
                    "5808913222465608048971511479506235998696112918992016407762164600898363761443",
                    true,
                ),
                (
                    "14139659546950817665773560726774860101672140396656643863078950566542608882454",
                    true,
                ),
                (
                    "9629332877235332256602865161617849630375965416260459249008709304721048200537",
                    false,
                ),
                (
                    "15194682846338116581211354907699755414549403072108697153631776325415459532015",
                    true,
                ),
                (
                    "16388444833249387234308154394290310584264985017796614640314011031393000328142",
                    false,
                ),
                (
                    "12893209696859870840964135760414645596172125459127736744879723498588409315420",
                    true,
                ),
                (
                    "4252466064505889568654035534564515419775193443498121206766286051578204786863",
                    true,
                ),
                (
                    "4143206562946824264148426319984303092795894066542641394672778393375938922561",
                    false,
                ),
                (
                    "9596842943859750941026234720461866030080253052472491176442559534941513029841",
                    true,
                ),
                (
                    "4874668976049850093271116246322279706660345846619050886670675494003997035667",
                    false,
                ),
                (
                    "20880637667070744859491879413494925400228379072955349596713793323424330258245",
                    false,
                ),
            ]
            .map(|(value, selector)| (BigUint::from_str(value).unwrap().into(), selector)),
            secret_key: BigUint::from_str(
                "17558656186380761716563192806052137702710836103853413805442046164934618201080",
            )
            .unwrap()
            .into(),
        };

        let witness_inputs =
            PoQWitnessInputs::from_leader_data(chain_data, common_data, wallet_data).unwrap();
        let (proof, inputs) = prove(witness_inputs).unwrap();
        let key_nullifier = inputs.key_nullifier.into_inner();
        // Test that verifying with the inputs returned by `prove` works.
        assert!(verify(&proof, inputs).unwrap());

        // Test that verifying with the reconstructed inputs inside the verifier context
        // works.
        let recomputed_verify_inputs = PoQVerifierInputData {
            core_quota: common_data.core_quota,
            core_root: chain_data.core_root,
            k_part_one: common_data.message_key.0,
            k_part_two: common_data.message_key.1,
            key_nullifier,
            leader_quota: common_data.leader_quota,
            pol_epoch_nonce: chain_data.pol_epoch_nonce,
            pol_ledger_aged: chain_data.pol_ledger_aged,
            session: chain_data.session,
            total_stake: chain_data.total_stake,
        };
        assert!(verify(&proof, recomputed_verify_inputs.into()).unwrap());
    }
}
