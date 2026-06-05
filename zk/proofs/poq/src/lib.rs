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
pub use inputs::{PoQVerifierInput, PoQVerifierInputData, PoQWitnessInputs};
use lb_groth16::{CompressedGroth16Proof, Groth16Proof, Groth16ProofJsonDeser};
use lb_log_targets::proofs;
pub use lb_pol::AGED_NOTE_MERKLE_TREE_HEIGHT;
use tracing::error;
pub use wallet_inputs::{AgedNotePathAndSelectors, PoQWalletInputs, PoQWalletInputsData};

use crate::{inputs::PoQVerifierInputJson, proving_key::POQ_PROVING_KEY_PATH};

pub type PoQProof = CompressedGroth16Proof;
pub type ProveError = lbp_error::Error;

const LOG_TARGET: &str = proofs::POQ;

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
    let witness = witness::generate_witness(inputs)?;
    let (proof, verifier_inputs) =
        lb_circuits_prover::prover_from_contents(POQ_PROVING_KEY_PATH.as_path(), witness.as_ref())?;
    let proof: Groth16ProofJsonDeser = serde_json::from_slice(&proof)?;
    let verifier_inputs: PoQVerifierInputJson = serde_json::from_slice(&verifier_inputs)?;
    let proof: Groth16Proof = proof.try_into().map_err(ProveError::Groth16JsonProof)?;
    Ok((
        CompressedGroth16Proof::try_from(&proof).unwrap_or_else(|e| {
            error!(target: LOG_TARGET, "Fatal CompressedGroth16Proof::try_from: {e}");
            // We panic here because this should never happen, and if it does, it's a
            // critical error that we want to be immediately visible during
            // development and testing.
            panic!("Fatal CompressedGroth16Proof::try_from: {e}")
        }),
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
    lb_groth16::groth16_verify(verification_key::POQ_VK.as_ref(), &expanded_proof, &inputs)
        .map_err(|e| VerifyError::ProofVerify(Box::new(e)))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use lb_pol::LotteryConstants;
    use lb_utils::math::NonNegativeRatio;
    use num_bigint::BigUint;

    use super::*;

    #[test]
    #[expect(clippy::too_many_lines, reason = "Test function.")]
    fn test_core_node_full_flow() {
        let blend_data = PoQBlendInputsData {
            core_sk: BigUint::from_str(
                "8905547699320869461831852104817789469875723637735901345741281588946310324235",
            )
            .unwrap()
            .into(),
            core_path_and_selectors: [
                (
                    "16869616652852667189559016717243338331068120739649173884502336155264218010727",
                    false,
                ),
                (
                    "13091718879067163858034522323644176312242009811093736972498389939059085177974",
                    true,
                ),
                (
                    "5615497624396623086866765843762273938268745340944523291991473843820399587958",
                    true,
                ),
                (
                    "12252618726807031528274527895794936410875679656522675436955173925070564886479",
                    true,
                ),
                (
                    "15676171816835539786276219242768254181009067816239948700832551985003974594975",
                    false,
                ),
                (
                    "19704272158800036501582000538191584664181486372889137807570676877252162790252",
                    false,
                ),
                (
                    "17220466132075642970726646606687934619880159579542773841528618335448239077575",
                    false,
                ),
                (
                    "19568444328548325046015987107341253380027495775774622463368229855377989226290",
                    true,
                ),
                (
                    "21061581629256637244739982573784550742652688800467757972406229545047035347638",
                    false,
                ),
                (
                    "21518036264660447465371694976644098419156368727076924435365409888210990153413",
                    true,
                ),
                (
                    "18601741813400480889163627600644403937577783284065467384061180740490941766846",
                    true,
                ),
                (
                    "3276819021875171213616879397115905648698624783190949445325727117619871062684",
                    false,
                ),
                (
                    "16346413070923069365311460658994366491438423790174305844846934643885739701955",
                    false,
                ),
                (
                    "18052187374311288196723181314946958960293262462466262108812751192264820802527",
                    true,
                ),
                (
                    "18798652887540222143344482340074977888006884307412611551550673334473344453748",
                    false,
                ),
                (
                    "6340439308577685187208962346420910386234054724707742363615331612477333766009",
                    true,
                ),
                (
                    "6116045944554536720728524915454748535792176096846806949818056548601205224613",
                    true,
                ),
                (
                    "7495766221993429009053819635630800227825524659472307268648873524676590491716",
                    true,
                ),
                (
                    "4135588845309195152336413092515000771731525447810984287718633658115964724154",
                    true,
                ),
                (
                    "16515196782727402790941217652547735699069943062019512321107139568403168848690",
                    true,
                ),
            ]
            .map(|(value, selector)| (BigUint::from_str(value).unwrap().into(), selector)),
        };
        let (lottery_0, lottery_1) =
            LotteryConstants::new(NonNegativeRatio::new(1, 10.try_into().unwrap()))
                .compute_lottery_values(5000);
        let chain_data = PoQChainInputsData {
            core_root: BigUint::from_str(
                "12497635102173390657108276580981678137198622257613601634363274282193270703654",
            )
            .unwrap()
            .into(),
            pol_ledger_aged: BigUint::from_str(
                "3285301153701124106247898475175231377413013906491501039690975241845353257824",
            )
            .unwrap()
            .into(),
            pol_epoch_nonce: BigUint::from_str(
                "12441952563111122190593710132868692987264877387856913250622883462242318292882",
            )
            .unwrap()
            .into(),
            lottery_0,
            lottery_1,
        };
        let common_data = PoQCommonInputsData {
            core_quota: 15,
            leader_quota: 10,
            message_key: (
                BigUint::from(123_456u32).into(),
                BigUint::from(654_321u32).into(),
            ),
            selector: false,
            index: 2,
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
            lottery_0: chain_data.lottery_0,
            lottery_1: chain_data.lottery_1,
        };
        assert!(verify(&proof, recomputed_verify_inputs.into()).unwrap());
    }

    #[expect(clippy::too_many_lines, reason = "For the sake of the test let it be")]
    #[test]
    fn test_leader_full_flow() {
        let (lottery_0, lottery_1) =
            LotteryConstants::new(NonNegativeRatio::new(1, 10.try_into().unwrap()))
                .compute_lottery_values(5000);
        let chain_data = PoQChainInputsData {
            core_root: BigUint::from_str(
                "12185619490528295825098195451407217973914857946530829707990590609721812577473",
            )
            .unwrap()
            .into(),
            pol_ledger_aged: BigUint::from_str(
                "367970463510572346469727551309607212474618104803583434125220973330505217187",
            )
            .unwrap()
            .into(),
            pol_epoch_nonce: BigUint::from_str(
                "19183275818966466009388966937999847470836267418943165691463680548408801149509",
            )
            .unwrap()
            .into(),
            lottery_0,
            lottery_1,
        };
        let common_data = PoQCommonInputsData {
            core_quota: 15,
            leader_quota: 10,
            message_key: (
                BigUint::from(123_456u32).into(),
                BigUint::from(654_321u32).into(),
            ),
            selector: true,
            index: 2,
        };
        let wallet_data = PoQWalletInputsData {
            slot: 3_934_028_363,
            note_value: 50,
            transaction_hash: BigUint::from_str(
                "2264455058971045832756600439690776883891824153136465835848552732282836700508",
            )
            .unwrap()
            .into(),
            output_number: 1108,
            aged_path_and_selectors: [
                (
                    "11104092157301530528415266289001896179719940037074919849624292254162611405786",
                    true,
                ),
                (
                    "7132185073809921262555691477157103985074819342411989543016526500095228117354",
                    false,
                ),
                (
                    "11907580727147699209958498886101128744892522133797395449242728594286922858086",
                    false,
                ),
                (
                    "13075221440612698710788795973343285593006804290474838812157163926886238383760",
                    true,
                ),
                (
                    "17200451570433990100996559280535385805985882628682996186008504614174096791798",
                    true,
                ),
                (
                    "16750120177808191972292661243802361139298740550633002442851428788856687150475",
                    false,
                ),
                (
                    "12679489866011049826521697310427217461017078670664946750194685078601853495880",
                    false,
                ),
                (
                    "15235439481027011453071738627024275793651222671965241576356428348106464813175",
                    false,
                ),
                (
                    "9961832183928877694020893796261063404160642210645529511233145074735042190217",
                    true,
                ),
                (
                    "4326894103682035305319995256704866698441157935817295295020304417938631996729",
                    true,
                ),
                (
                    "17555384873204675431996467471744635783468598594303097832615135885710898785356",
                    false,
                ),
                (
                    "2067987409200354598518658724620929803515175794312186642703606119551864949264",
                    true,
                ),
                (
                    "18414033735029028807991144814771963824730052421471947864345037264843089845734",
                    false,
                ),
                (
                    "11883340736248057870265402327022279289678280641364875114447762692303561445604",
                    false,
                ),
                (
                    "7157973471780523265294561800756652242511668640260136396798977970116460727342",
                    true,
                ),
                (
                    "10322140357911263063410611191898227332467973200575964702516054623940247053118",
                    false,
                ),
                (
                    "12567824105074446475873393314739330450709906448171155888539789943621309224508",
                    true,
                ),
                (
                    "3778057320817011605792674467466589599030650184482297863404214181285644124757",
                    true,
                ),
                (
                    "15139857989447407851586537192536170660643402207011818138460800689622488846498",
                    true,
                ),
                (
                    "4686260554210282279381725438723974181592826312599841000574494090500374964214",
                    true,
                ),
                (
                    "2017001248552489998639360772220278151302627680149021505074390044394703925937",
                    false,
                ),
                (
                    "15644113980009580949735073673522118459196764019649835645357551831476441064371",
                    true,
                ),
                (
                    "5851335105157805239260310112724712629219062160101168478507890653975790109833",
                    false,
                ),
                (
                    "4506645166310701016292123296822379297660598150676608439399796403413830281952",
                    false,
                ),
                (
                    "19761070472419507198412744968226383034230108135701041508981821408866293379367",
                    false,
                ),
                (
                    "16473696535741964167503054455797711177314106892505373545741412745155986927894",
                    true,
                ),
                (
                    "7024125351521471566160638647544413645870621144999681104961251077959669126366",
                    false,
                ),
                (
                    "4223261943159590385933901257711007435848118520945782486495445973028111080784",
                    true,
                ),
                (
                    "2480120922453299704385093407865595899109107879489738668468480942431779023090",
                    false,
                ),
                (
                    "4872555691447955405629869337126212288862528483324109155414334863586541089650",
                    false,
                ),
                (
                    "2885249961213344608420596395566111778633573487967873292737118191013543595441",
                    false,
                ),
                (
                    "20675954944665714887729603321541315818168820606980234992582325333134286444983",
                    false,
                ),
            ]
            .map(|(value, selector)| (BigUint::from_str(value).unwrap().into(), selector)),
            secret_key: BigUint::from_str(
                "14029017592631272654768426994822986470491530234220356957377918534745805708829",
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
            lottery_0: chain_data.lottery_0,
            lottery_1: chain_data.lottery_1,
        };
        assert!(verify(&proof, recomputed_verify_inputs.into()).unwrap());
    }
}
