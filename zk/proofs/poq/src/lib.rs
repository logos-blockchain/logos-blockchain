mod blend_inputs;
mod chain_inputs;
mod common_inputs;
mod inputs;
mod pow_inputs;
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
use lb_circuits_prover::Prover as _;
use lb_groth16::{
    CompressedGroth16Proof, Groth16Proof, Groth16ProofJsonDeser, groth16_batch_verify,
};
use lb_log_targets::proofs;
pub use lb_pol::AGED_NOTE_MERKLE_TREE_HEIGHT;
pub use pow_inputs::{PoQPowInputs, PoQPowInputsData};
use tracing::error;
pub use wallet_inputs::{AgedNotePathAndSelectors, PoQWalletInputs, PoQWalletInputsData};

use crate::inputs::PoQVerifierInputJson;

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
    let result = lb_circuits_prover::Rapidsnark::prove(
        lbc_poq_sys::artifacts::PROVING_KEY,
        witness.as_ref(),
    )?;
    let proof: Groth16ProofJsonDeser = serde_json::from_str(&result.proof)?;
    let verifier_inputs: PoQVerifierInputJson = serde_json::from_str(&result.public_signals)?;
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

pub fn batch_verify(
    proofs_and_inputs: &[(PoQProof, PoQVerifierInput)],
) -> Result<bool, VerifyError> {
    let inputs: Vec<Vec<_>> = proofs_and_inputs
        .iter()
        .cloned()
        .map(|(_, pi)| pi.to_inputs().to_vec())
        .collect();

    let expanded_proofs: Vec<Groth16Proof> = proofs_and_inputs
        .iter()
        .map(|(p, _)| Groth16Proof::try_from(p).map_err(|_| VerifyError::Expansion))
        .collect::<Result<Vec<_>, _>>()?; // short-circuits on first failure

    Ok(groth16_batch_verify(
        verification_key::POQ_VK.as_ref(),
        &expanded_proofs,
        &inputs,
    ))
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
                "12667207762918681737213577433372645554941956657477853383508894204444641435708",
            )
            .unwrap()
            .into(),
            core_path_and_selectors: [
                (
                    "7495929195953210541208591724669352166258167386402673863836582332959049741998",
                    true,
                ),
                (
                    "8777307683015870709408351814333112152804794621246499110711643849444602886454",
                    false,
                ),
                (
                    "2036778247346771710692375429289939913839125337381038976370669720942761418159",
                    false,
                ),
                (
                    "5150238042344629386129384874056943525919076545561275756461664527118718260316",
                    false,
                ),
                (
                    "4936482837140898160990313117063047116523911033722639295600657041261330950251",
                    true,
                ),
                (
                    "8937835534203757508211957478554582398600683099364069427807070320868647534222",
                    false,
                ),
                (
                    "20952980496079374121937279088975994236259798048934859728621453910938508315429",
                    false,
                ),
                (
                    "3127567055231452038159138967497074106547716639376277019791213152208715502108",
                    false,
                ),
                (
                    "4156278452715681991545944691699148599903237834447534444828105997249209183668",
                    true,
                ),
                (
                    "18179601246737692255359026481063507429449403349479564041630890140769203876921",
                    true,
                ),
                (
                    "20437752904871274044354648385284243748143753378271484669957282746297202768728",
                    false,
                ),
                (
                    "20964128655175295722279612439890819641847965870342369257222152533521036200042",
                    false,
                ),
                (
                    "8529481264681700999783095712819666338972397694516091342589511693767349600946",
                    true,
                ),
                (
                    "11390366370077994090069924023908925478862142650386311632070176411828525668517",
                    true,
                ),
                (
                    "5917548759338686071972714539493044537194763096388397090814246211223906547270",
                    false,
                ),
                (
                    "20167409213677086350276694709897905285253180362287004012973310966314090006733",
                    true,
                ),
                (
                    "2263236988262202014904251664760262847563737954962247776121488194923407644186",
                    false,
                ),
                (
                    "19725021492526941122134445491655907123723766494432771594886452496452660956448",
                    false,
                ),
                (
                    "5142311427791207713153244085802613121123367252788982353300919738927631606801",
                    false,
                ),
                (
                    "9162839441909107733910143825064550229009975662836406280871655871464192283628",
                    false,
                ),
            ]
            .map(|(value, selector)| (BigUint::from_str(value).unwrap().into(), selector)),
        };
        let chain_data = PoQChainInputsData {
            core_root: BigUint::from_str(
                "8648439177578503276872559887462337398415999326614652623958174178821393732006",
            )
            .unwrap()
            .into(),
            pol_ledger_aged: BigUint::from_str(
                "17215820411670358459256686882934231030146782155124511326026608377144127196093",
            )
            .unwrap()
            .into(),
            pol_epoch_nonce: BigUint::from_str(
                "16164570534004466859325445480705051126415612717114918921927075731584584730308",
            )
            .unwrap()
            .into(),
            lottery_0: BigUint::from_str(
                "148409079361904587837471709956458430342187235603420891378597419711486680",
            )
            .unwrap()
            .into(),
            lottery_1: BigUint::from_str(
                "21888242871336145414933615591437256729835795974444825699352339602896135814447",
            )
            .unwrap()
            .into(),
            block_hash: BigUint::from_str(
                "8120861784052316792639248700404452951342238269077440156691809582275873301855",
            )
            .unwrap()
            .into(),
        };
        let common_data = PoQCommonInputsData {
            core_quota: 10,
            leader_quota: 15,
            pow_quota: 20,
            message_key: (
                BigUint::from(123_456u32).into(),
                BigUint::from(654_321u32).into(),
            ),
            selector: 0,
            index: 9,
            pow_difficulty: BigUint::from_str(
                "2334035772366381927456001473031809359552034265892309520954243351196711606017",
            )
            .unwrap()
            .into(),
        };

        let witness_inputs =
            PoQWitnessInputs::from_core_node_data(chain_data, common_data, blend_data).unwrap();
        let (proof, inputs) = prove(witness_inputs).unwrap();
        let key_nullifier = inputs.key_nullifier.into_inner();
        // `pow_pk` is a circuit output derived from the randomly generated `pow_sk`, so
        // it can only be recovered from the proof's public signals.
        let pow_pk = inputs.pow_pk.into_inner();
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
            pow_pk,
            leader_quota: common_data.leader_quota,
            pow_quota: common_data.pow_quota,
            pow_block_hash: chain_data.block_hash,
            pow_blend_difficulty: common_data.pow_difficulty,
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
            block_hash: BigUint::from_str(
                "8120861784052316792639248700404452951342238269077440156691809582275873301855",
            )
            .unwrap()
            .into(),
        };
        let common_data = PoQCommonInputsData {
            core_quota: 15,
            leader_quota: 10,
            pow_quota: 5,
            message_key: (
                BigUint::from(123_456u32).into(),
                BigUint::from(654_321u32).into(),
            ),
            selector: 1,
            index: 2,
            pow_difficulty: BigUint::from_str(
                "2334035772366381927456001473031809359552034265892309520954243351196711606017",
            )
            .unwrap()
            .into(),
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
            pol_secret_key: BigUint::from_str(
                "14029017592631272654768426994822986470491530234220356957377918534745805708829",
            )
            .unwrap()
            .into(),
        };

        let witness_inputs =
            PoQWitnessInputs::from_leader_data(chain_data, common_data, wallet_data).unwrap();
        let (proof, inputs) = prove(witness_inputs).unwrap();
        let key_nullifier = inputs.key_nullifier.into_inner();
        // `pow_pk` is a circuit output derived from the randomly generated `pow_sk`, so
        // it can only be recovered from the proof's public signals.
        let pow_pk = inputs.pow_pk.into_inner();
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
            pow_pk,
            leader_quota: common_data.leader_quota,
            pow_quota: common_data.pow_quota,
            pow_block_hash: chain_data.block_hash,
            pow_blend_difficulty: common_data.pow_difficulty,
            pol_epoch_nonce: chain_data.pol_epoch_nonce,
            pol_ledger_aged: chain_data.pol_ledger_aged,
            lottery_0: chain_data.lottery_0,
            lottery_1: chain_data.lottery_1,
        };
        assert!(verify(&proof, recomputed_verify_inputs.into()).unwrap());
    }

    #[test]
    fn test_pow_full_flow() {
        let chain_data = PoQChainInputsData {
            core_root: BigUint::from_str(
                "7196120816867742764244305920813414287385070139349175831448761050095304066684",
            )
            .unwrap()
            .into(),
            pol_ledger_aged: BigUint::from_str(
                "12901234158445930888471913831760642108627105440164948073754288938400615804116",
            )
            .unwrap()
            .into(),
            pol_epoch_nonce: BigUint::from_str(
                "10612486568558363932403470969435464905237449260307875593547304337083779862075",
            )
            .unwrap()
            .into(),
            lottery_0: BigUint::from_str(
                "148409079361904587837471709956458430342187235603420891378597419711486680",
            )
            .unwrap()
            .into(),
            lottery_1: BigUint::from_str(
                "21888242871336145414933615591437256729835795974444825699352339602896135814447",
            )
            .unwrap()
            .into(),
            block_hash: BigUint::from_str(
                "17412116459874055221726429396167151400213699852365025340176158516975240665302",
            )
            .unwrap()
            .into(),
        };
        let common_data = PoQCommonInputsData {
            core_quota: 10,
            leader_quota: 15,
            pow_quota: 20,
            message_key: (
                BigUint::from(123_456u32).into(),
                BigUint::from(654_321u32).into(),
            ),
            selector: 2,
            index: 8,
            pow_difficulty: BigUint::from_str(
                "10631870504716456348838861774188160492563879712126054449569633827216160699117",
            )
            .unwrap()
            .into(),
        };
        let pow_data = PoQPowInputsData {
            pow_secret_key: BigUint::from_str(
                "10044758699144566038746293679996441958807939592793641056821682251877616662024",
            )
            .unwrap()
            .into(),
        };

        let witness_inputs =
            PoQWitnessInputs::from_pow_data(chain_data, common_data, pow_data).unwrap();
        let (proof, inputs) = prove(witness_inputs).unwrap();
        let key_nullifier = inputs.key_nullifier.into_inner();
        // On this path `pow_pk` is derived from the real `pow_sk`, but it is still a
        // circuit output and so is read back from the proof's public signals.
        let pow_pk = inputs.pow_pk.into_inner();
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
            pow_pk,
            leader_quota: common_data.leader_quota,
            pow_quota: common_data.pow_quota,
            pow_block_hash: chain_data.block_hash,
            pow_blend_difficulty: common_data.pow_difficulty,
            pol_epoch_nonce: chain_data.pol_epoch_nonce,
            pol_ledger_aged: chain_data.pol_ledger_aged,
            lottery_0: chain_data.lottery_0,
            lottery_1: chain_data.lottery_1,
        };
        assert!(verify(&proof, recomputed_verify_inputs.into()).unwrap());
    }
}
