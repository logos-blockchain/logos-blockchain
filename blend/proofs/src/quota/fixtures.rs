use lb_pol::LotteryConstants;
use lb_utils::math::NonNegativeRatio;
use num_bigint::BigUint;

use crate::quota::{
    Ed25519PublicKey, Quota,
    inputs::prove::{
        PublicInputs,
        private::{ProofOfCoreQuotaInputs, ProofOfLeadershipQuotaInputs, ProofOfWorkQuotaInputs},
        public::{CoreInputs, LeaderInputs, PowInputs},
    },
};

// Reference values taken from the tests within the `poq` crate.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "Fixture function. Only used for tests."
)]
pub fn valid_proof_of_core_quota_inputs(
    signing_key: Ed25519PublicKey,
    core_quota: Quota,
) -> (PublicInputs, ProofOfCoreQuotaInputs) {
    let (lottery_0, lottery_1) =
        LotteryConstants::new(NonNegativeRatio::new(1, 10.try_into().unwrap()))
            .compute_lottery_values(1);
    let public_inputs = PublicInputs {
        signing_key,
        core: CoreInputs {
            quota: core_quota,
            zk_root: "8648439177578503276872559887462337398415999326614652623958174178821393732006"
                .parse::<BigUint>()
                .unwrap()
                .into(),
        },
        leader: LeaderInputs {
            pol_epoch_nonce: BigUint::from(1u64).into(),
            pol_eligible_aged: BigUint::from(1u64).into(),
            message_quota: Quota::ONE,
            lottery_0,
            lottery_1,
        },
        pow: PowInputs {
            pow_blend_difficulty: BigUint::from(1u64).into(),
            pow_quota: Quota::new::<1>(),
        },
    };

    let private_inputs = ProofOfCoreQuotaInputs {
        // From the leaf level up, the selector of a level being the bit of the leaf
        // index at that level.
        core_path_and_selectors: [
            (
                "7495929195953210541208591724669352166258167386402673863836582332959049741998",
                false,
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
                true,
            ),
            (
                "3127567055231452038159138967497074106547716639376277019791213152208715502108",
                true,
            ),
            (
                "4156278452715681991545944691699148599903237834447534444828105997249209183668",
                false,
            ),
            (
                "18179601246737692255359026481063507429449403349479564041630890140769203876921",
                false,
            ),
            (
                "20437752904871274044354648385284243748143753378271484669957282746297202768728",
                true,
            ),
            (
                "20964128655175295722279612439890819641847965870342369257222152533521036200042",
                true,
            ),
            (
                "8529481264681700999783095712819666338972397694516091342589511693767349600946",
                false,
            ),
            (
                "11390366370077994090069924023908925478862142650386311632070176411828525668517",
                false,
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
                true,
            ),
        ]
        .map(|(value, selector)| (value.parse::<BigUint>().unwrap().into(), selector)),
        core_sk: "12667207762918681737213577433372645554941956657477853383508894204444641435708"
            .parse::<BigUint>()
            .unwrap()
            .into(),
    };

    (public_inputs, private_inputs)
}

// Reference values taken from the leader test vector of the `poq` crate.
#[must_use]
pub fn valid_proof_of_leadership_quota_inputs(
    signing_key: Ed25519PublicKey,
    leader_quota: Quota,
) -> (PublicInputs, ProofOfLeadershipQuotaInputs) {
    let (chain, _, wallet) = lb_poq::test_utils::leader_witness_data();
    let public_inputs = PublicInputs {
        signing_key,
        leader: LeaderInputs {
            pol_epoch_nonce: chain.pol_epoch_nonce,
            pol_eligible_aged: chain.pol_eligible_aged,
            message_quota: leader_quota,
            lottery_0: chain.lottery_0,
            lottery_1: chain.lottery_1,
        },
        core: CoreInputs {
            zk_root: BigUint::from(1u64).into(),
            quota: Quota::ONE,
        },
        pow: PowInputs {
            pow_blend_difficulty: BigUint::from(1u64).into(),
            pow_quota: Quota::new::<1>(),
        },
    };

    let private_inputs = ProofOfLeadershipQuotaInputs {
        slot: wallet.slot,
        note_value: wallet.note_value,
        note_nonce: wallet.note_nonce,
        commitment_aged_path_and_selectors: wallet.commitment_aged_path_and_selectors,
        nullifier_root_aged: wallet.nullifier_root_aged,
        set_aged_path_and_selectors: wallet.set_aged_path_and_selectors,
        secret_key: wallet.pol_secret_key,
    };

    (public_inputs, private_inputs)
}

// Reference values taken from the tests within the `poq` crate.
#[must_use]
pub fn valid_proof_of_work_quota_inputs(
    signing_key: Ed25519PublicKey,
    pow_quota: Quota,
) -> (PublicInputs, ProofOfWorkQuotaInputs) {
    let public_inputs = PublicInputs {
        signing_key,
        pow: PowInputs {
            pow_blend_difficulty:
                "11767310438037502057348804636667730067145920466135580383473019029395854041585"
                    .parse::<BigUint>()
                    .unwrap()
                    .into(),
            pow_quota,
        },
        core: CoreInputs {
            zk_root:
                "17221527552435885266381262988260904945767902157699339824536328890908744829004"
                    .parse::<BigUint>()
                    .unwrap()
                    .into(),
            quota: Quota::new::<10>(),
        },
        leader: LeaderInputs {
            pol_epoch_nonce:
                "9044881160076582754939149961589913743685188188594985299351503064212059195449"
                    .parse::<BigUint>()
                    .unwrap()
                    .into(),
            pol_eligible_aged:
                "15524203674561292134108415479004597997158273575770759806968210936102441082561"
                    .parse::<BigUint>()
                    .unwrap()
                    .into(),
            message_quota: Quota::new::<15>(),
            lottery_0: "148409079361904587837471709956458430342187235603420891378597419711486680"
                .parse::<BigUint>()
                .unwrap()
                .into(),
            lottery_1:
                "21888242871336145414933615591437256729835795974444825699352339602896135814447"
                    .parse::<BigUint>()
                    .unwrap()
                    .into(),
        },
    };

    let private_inputs = ProofOfWorkQuotaInputs {
        pow_nonce: "3208667997606854837691155513582782452110664647780085150997178953548554066249"
            .parse::<BigUint>()
            .unwrap()
            .into(),
    };

    (public_inputs, private_inputs)
}
