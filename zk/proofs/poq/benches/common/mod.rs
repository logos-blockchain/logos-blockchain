//! Proof of Quota witness input fixtures shared by the `prove` and `verify`
//! benchmarks.
//!
//! The values are the same reference vectors used by the `poq` crate tests.

use std::str::FromStr as _;

use lb_pol::LotteryConstants;
use lb_utils::math::NonNegativeRatio;
use logos_blockchain_poq::{
    KeyIndex, PoQBlendInputsData, PoQChainInputsData, PoQCommonInputsData, PoQSelector,
    PoQWitnessInputs, Quota, test_utils,
};
use num_bigint::BigUint;

/// Quota advertised by the fixtures, for the core, the leader and pow branches.
///
/// It has to stay above every key index the benchmarks prove for, since the
/// circuit only accepts indices below the quota of the branch being proven.
const QUOTA: Quota = Quota::new::<64>();

fn lottery() -> (lb_groth16::Fr, lb_groth16::Fr) {
    LotteryConstants::new(NonNegativeRatio::new(1, 10.try_into().unwrap()))
        .compute_lottery_values(5000)
}

fn common_data(selector: PoQSelector, key_index: KeyIndex) -> PoQCommonInputsData {
    PoQCommonInputsData {
        core_quota: QUOTA,
        leader_quota: QUOTA,
        pow_quota: QUOTA,
        message_key: (
            BigUint::from(123_456u32).into(),
            BigUint::from(654_321u32).into(),
        ),
        selector,
        index: key_index,
        pow_difficulty: BigUint::from_str(
            "2334035772366381927456001473031809359552034265892309520954243351196711606017",
        )
        .unwrap()
        .into(),
    }
}

/// Witness inputs for a core node Proof of Quota at the given key index.
///
/// Each index yields a distinct proof and key nullifier, which is what lets the
/// batch verification benchmarks build batches of non-identical proofs.
#[expect(
    clippy::too_many_lines,
    reason = "Benchmark fixture with hardcoded data"
)]
pub fn core_node_inputs(key_index: KeyIndex) -> PoQWitnessInputs {
    let (lottery_0, lottery_1) = lottery();
    let chain_data = PoQChainInputsData {
        core_root: BigUint::from_str(
            "10774149910279330054096178616484626574938100628643657398591620611653283350567",
        )
        .unwrap()
        .into(),
        pol_eligible_aged: BigUint::from_str(
            "5899427723120145663289932094176488517692686385716511263841021495659364229709",
        )
        .unwrap()
        .into(),
        pol_epoch_nonce: BigUint::from_str(
            "14296132653141589082634452040005706132398442975462231668085056116416860738326",
        )
        .unwrap()
        .into(),
        lottery_0,
        lottery_1,
    };
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
        .map(|(v, s)| (BigUint::from_str(v).unwrap().into(), s)),
    };

    PoQWitnessInputs::from_core_node_data(
        chain_data,
        common_data(PoQSelector::Core, key_index),
        blend_data,
    )
}

/// Witness inputs for a leadership Proof of Quota at the given key index.
pub fn leader_inputs(key_index: KeyIndex) -> PoQWitnessInputs {
    let (chain_data, _, wallet_data) = test_utils::leader_witness_data();
    PoQWitnessInputs::from_leader_data(
        chain_data,
        common_data(PoQSelector::Leader, key_index),
        wallet_data,
    )
}
