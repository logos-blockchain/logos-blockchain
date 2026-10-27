//! A ledger as a live chain holds it: the ledger note set and the note set of
//! one channel each hold [`fill`] nullifiers and [`fill`] commitments, and one
//! program is registered. The gas prices are `0`, so the measured
//! transactions fund no fee.

use std::{
    collections::HashMap,
    num::NonZero,
    sync::{Arc, LazyLock},
};

use lb_core::{
    mantle::{
        Op, OpProof, SignedOps,
        gas::MainnetGasProfile,
        ledger::verification_mode::StandardMode,
        note_set::{LEDGER_SET, NoteSetIndex},
        notes::{self, NoteCommitment, NoteNullifier, ProgramId},
        ops::{
            ChannelInscribeProof, NoOpProof,
            channel::{
                ChannelId, MsgId,
                inscribe::{ChannelSteps, InscriptionOp},
            },
            program::ProgramRegisterOp,
        },
        programs::{ProgramVerificationKey, derive_program_id},
        traits::Hashable as _,
        transactions::{GasPrices, OpProofs, Ops, states::Preverified},
    },
    sdp::{MinStake, ServiceParameters, ServiceType},
};
use lb_cryptarchia_engine::EpochConfig;
use lb_groth16::Fr;
use lb_key_management_system_keys::keys::{Ed25519Key, ZkPublicKey};
use lb_utils::math::{NonNegativeRatio, PositiveF64};
use logos_blockchain_ledger::{
    Config, LedgerState,
    config::{BlendPoWConfig, ConsensusExcludedKeys, ModulusShift, PoWConfig, RewardPoWConfig},
    mantle::sdp::{ServiceRewardsParameters, rewards},
};
use rand::{Rng as _, SeedableRng as _, rngs::StdRng};

pub type HeaderId = [u8; 32];

/// The number of nullifiers, and of commitments, in each filled note set
/// unless `GAS_BENCH_FILL` sets another, which only shortens the fill while
/// developing a case: the measures are taken at the default.
pub const DEFAULT_FILL: usize = 1_000_000;

pub fn fill() -> usize {
    fill_from(std::env::var("GAS_BENCH_FILL").ok().as_deref())
}

pub fn fill_from(variable: Option<&str>) -> usize {
    variable.map_or(DEFAULT_FILL, |value| {
        value.parse().expect("GAS_BENCH_FILL is a count")
    })
}

pub struct Fixture {
    pub config: Config,
    pub state: LedgerState,
    pub channel: ChannelId,
    pub channel_key: Ed25519Key,
    pub channel_set: NoteSetIndex,
    pub program: ProgramId,
}

static FIXTURE: LazyLock<Fixture> = LazyLock::new(build);

pub fn filled_state() -> &'static Fixture {
    &FIXTURE
}

/// `count` nullifiers absent from every filled set, the same for the same
/// `salt`.
pub fn nullifiers(count: usize, salt: u64) -> Vec<NoteNullifier> {
    fresh(count, salt, 1).map(NoteNullifier).collect()
}

/// `count` commitments absent from every filled set, the same for the same
/// `salt`.
pub fn commitments(count: usize, salt: u64) -> Vec<NoteCommitment> {
    fresh(count, salt, 2).map(NoteCommitment).collect()
}

// The filled values are below 2^128, the fresh ones above 2^192.
fn fresh(count: usize, salt: u64, kind: u128) -> impl Iterator<Item = Fr> {
    (0..count as u128).map(move |index| {
        Fr::from(kind) * Fr::from(1u128 << 96) * Fr::from(1u128 << 96)
            + Fr::from((u128::from(salt) << 32) | index)
    })
}

/// Signs `ops` with `key` for every inscription, and gives every other
/// Operation an empty proof.
pub fn signed(ops: Vec<Op>, key: &Ed25519Key) -> SignedOps<Preverified, StandardMode> {
    let ops = Ops::new_unchecked(ops);
    let tx_hash = ops.hash();
    let proofs = ops
        .iter()
        .map(|op| match op {
            Op::ChannelInscribe(_) => OpProof::ChannelInscribe(
                ChannelInscribeProof::without_steps(key.sign_payload(tx_hash.as_signing_bytes())),
            ),
            _ => OpProof::None(NoOpProof),
        })
        .collect();
    SignedOps::from_parts(ops, OpProofs::new_unchecked(proofs))
        .expect("the transaction builds")
        .preverify()
        .expect("the transaction preverifies")
}

pub fn program_key() -> ProgramVerificationKey {
    ProgramVerificationKey::try_from(&lb_groth16::test_utils::three_input_setup(1).0)
        .expect("the test key is valid")
}

fn apply(
    state: LedgerState,
    config: &Config,
    tx: &SignedOps<Preverified, StandardMode>,
) -> LedgerState {
    state
        .try_apply_transaction::<_, HeaderId, MainnetGasProfile>(config, tx)
        .expect("the setup transaction applies")
        .0
}

fn build() -> Fixture {
    let config = config();
    let channel = ChannelId::from([1u8; 32]);
    let channel_key = Ed25519Key::from_bytes(&[7u8; 32]);
    let genesis_note = notes::Note::new(1, Fr::from(1u64), ZkPublicKey::new(Fr::from(2u64)));
    let state =
        LedgerState::from_notes([genesis_note], &config).with_gas_prices(&GasPrices::new(0, 0));

    let create_channel = Op::ChannelInscribe(InscriptionOp {
        channel_id: channel,
        inscription: [0].into(),
        parent: MsgId::root(),
        signer: channel_key.public_key().into_unverified(),
        steps: ChannelSteps::default(),
    });
    let key = program_key();
    let program = derive_program_id(&key);
    let register = Op::ProgramRegister(ProgramRegisterOp {
        verification_key: key,
    });
    let state = apply(
        state,
        &config,
        &signed(vec![create_channel, register], &channel_key),
    );

    let channel_set = state
        .mantle_ledger()
        .channels()
        .channel_state(&channel)
        .and_then(|channel| channel.note_set)
        .expect("the channel has a note set");

    let mut rng = StdRng::seed_from_u64(0x6a5);
    let mut note_sets = state.latest_note_sets().clone();
    for index in [LEDGER_SET, channel_set] {
        let filled_nullifiers: Vec<NoteNullifier> =
            std::iter::repeat_with(|| NoteNullifier(Fr::from(rng.gen_range(1..u128::MAX))))
                .take(fill())
                .collect();
        let filled_commitments: Vec<NoteCommitment> =
            std::iter::repeat_with(|| NoteCommitment(Fr::from(rng.r#gen::<u128>())))
                .take(fill())
                .collect();
        let set = note_sets
            .get(index)
            .expect("the set exists")
            .clone()
            .execute_spending(&filled_nullifiers)
            .expect("the filled nullifiers are distinct")
            .execute_adding(&filled_commitments)
            .expect("the commitment MMR has room");
        note_sets = note_sets.update(index, set);
    }
    let state = state.with_latest_note_sets(note_sets.end_block(0));

    Fixture {
        config,
        state,
        channel,
        channel_key,
        channel_set,
        program,
    }
}

pub fn config() -> Config {
    let epoch_config = EpochConfig {
        epoch_stake_distribution_stabilization: NonZero::new(3).unwrap(),
        epoch_period_nonce_buffer: NonZero::new(3).unwrap(),
        epoch_period_nonce_stabilization: NonZero::new(4).unwrap(),
    };
    let consensus_config = lb_cryptarchia_engine::Config::new(
        NonZero::new(1).unwrap(),
        NonNegativeRatio::new(1, 10.try_into().unwrap()),
        1f64.try_into().expect("1 > 0"),
        NonZero::new(12).unwrap(),
    );
    let epoch_length = epoch_config.epoch_length(consensus_config.base_period_length());

    Config {
        epoch_config,
        consensus_config,
        sdp_config: logos_blockchain_ledger::mantle::sdp::Config {
            service_params: Arc::new(HashMap::from([(
                ServiceType::BlendNetwork,
                ServiceParameters {
                    inactivity_period: 2.try_into().unwrap(),
                    epoch: 0.into(),
                },
            )])),
            service_rewards_params: ServiceRewardsParameters {
                blend: rewards::blend::RewardsParameters {
                    rounds_per_epoch: epoch_length.try_into().unwrap(),
                    message_frequency_per_round: PositiveF64::try_from(1.0).unwrap(),
                    num_blend_layers: NonZero::new(3).unwrap(),
                    minimum_network_size: NonZero::new(1).unwrap(),
                    data_replication_factor: 0,
                    activity_threshold_sensitivity: 1,
                },
            },
            min_stake: MinStake {
                threshold: 1,
                timestamp: 0,
            },
        },
        consensus_excluded_keys: ConsensusExcludedKeys::empty(),
        pow_config: PoWConfig {
            blend: BlendPoWConfig {
                base_difficulty: ModulusShift::new::<234>(),
                target_transactions_per_block: NonZero::new(10).unwrap(),
                max_step: NonZero::new(4).unwrap(),
                damping_num: NonZero::new(1).unwrap(),
                damping_den_offset: 1,
            },
            reward: RewardPoWConfig {
                reward_pool_genesis: 1_000_000_000,
                epoch_reward_genesis: 1_000_000,
                minimum_difficulty: ModulusShift::new::<26>(),
                ema_smoothing_factor: 9,
                ema_smoothing_precision: NonZero::new(10).unwrap(),
                target_claims_per_block: 100,
                rate_num: 0,
                rate_den: NonZero::<u64>::MIN,
                target_claim_per_block: NonZero::<u64>::MIN,
                pow_share: 0,
                share_den: NonZero::<u64>::MIN,
                slot_window: NonZero::new(100).unwrap(),
            },
        },
    }
}
