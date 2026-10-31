use std::{iter::empty, num::NonZero, sync::Arc, time::Duration};

use lb_binary_codec::bincode::{DeserializeOp as _, SerializeOp as _};
use lb_blend_service::{
    ServiceComponents,
    api::BlendServiceApi,
    message::{DataPayload, ProxyServiceMessage, ServiceMessage},
};
use lb_chain_service::{
    ChainServiceInfo, ConsensusMsg, CryptarchiaInfo, PhaseTag, Query, Slot, State,
    api::CryptarchiaServiceApi,
};
use lb_core::{
    crypto::ZkHash,
    header::HeaderId,
    mantle::{
        OpRef, SignedOps,
        gas::MainnetGasProfile,
        ledger::verification_mode::StandardMode,
        ops::{leader_claim::VoucherCm, pow::ClaimPowRewardOp},
        transactions::states::{Preverified, Unverified},
    },
    proofs::leader_proof::{Groth16LeaderProof, LeaderProof, LeaderPublic},
};
use lb_groth16::Fr;
use lb_key_management_system_keys::keys::{
    UnsecuredZkKey, UnverifiedEd25519PublicKey, ZkPublicKey,
};
use lb_ledger::{Config, ConsensusExcludedKeys, Ledger, LedgerState};
use lb_time::era::{EraEntriesAfterGenesis, EraSchedule, EraTiming};
use lb_utils::bounded::UpperRuntimeBoundedVec;
use overwatch::services::{ServiceData, relay::OutboundRelay};
use rand::{SeedableRng as _, rngs::StdRng};
use serde_json::{from_value, json};
use time::OffsetDateTime;
use tokio::{
    pin as tokio_pin, select as tokio_select, spawn,
    sync::mpsc::{Receiver, channel, error::TryRecvError},
    task::JoinHandle,
    test as tokio_test,
};

use super::{
    PoWEraSettings, PoWError, PoWServiceState, PublishedClaim, ServiceEraSchedule,
    claim_ready_rewards, max_claims_by_payload_size,
};
use crate::{test_utils::PAYLOAD_BODY_SIZE, tickets::WinningTicket};

const ANCHOR: [u8; 32] = [1; 32];
const OTHER_BLOCK: [u8; 32] = [2; 32];
const PREVIOUS_BLOCK: [u8; 32] = [150; 32];
const TIP: [u8; 32] = [160; 32];
const POOL: u64 = 1_000_000_000_000;

struct TestProof(Groth16LeaderProof);

impl LeaderProof for TestProof {
    fn verify(&self, _public_inputs: &LeaderPublic) -> bool {
        true
    }

    fn verify_genesis(&self) -> bool {
        true
    }

    fn entropy(&self) -> Fr {
        self.0.entropy()
    }

    fn leader_key(&self) -> &UnverifiedEd25519PublicKey {
        self.0.leader_key()
    }

    fn voucher_cm(&self) -> &VoucherCm {
        self.0.voucher_cm()
    }
}

/// Build configs with 100-slot genesis epochs and 20-slot later epochs.
fn ledger_config(short_epochs: bool) -> Config {
    let (denominator, phases, window) = if short_epochs {
        (5, [1, 1, 2], 20)
    } else {
        (10, [3, 3, 4], 100)
    };
    from_value(json!({
        "epoch_config": {
            "epoch_stake_distribution_stabilization": phases[0],
            "epoch_period_nonce_buffer": phases[1],
            "epoch_period_nonce_stabilization": phases[2]
        },
        "consensus_config": {
            "security_param": 1,
            "slot_activation_coeff": {"numerator": 1, "denominator": denominator},
            "stake_inference_learning_rate": 1.0,
            "uncle_reference_window_in_block": 0,
            "max_uncles": 0
        },
        "sdp_config": {
            "service_params": {"BN": {"inactivity_period": 20, "epoch": 0}},
            "service_rewards_params": {"blend": {
                "rounds_per_epoch": if short_epochs { 20 } else { 100 },
                "message_frequency_per_round": 1.0,
                "num_blend_layers": 1,
                "data_replication_factor": 0,
                "minimum_network_size": 1,
                "activity_threshold_sensitivity": 1
            }},
            "min_stake": {"threshold": 1, "timestamp": 0}
        },
        "pow_config": {
            "blend": {
                "base_difficulty": 19,
                "damping_den_offset": 0,
                "damping_num": 1,
                "max_step": 1,
                "target_transactions_per_block": 1
            },
            "reward": {
                "minimum_difficulty": 19,
                "ema_smoothing_factor": 9,
                "ema_smoothing_precision": 10,
                "target_claims_per_block": 1,
                "rate_num": 1,
                "rate_den": 1,
                "target_claim_per_block": 1,
                "pow_share": 0,
                "share_den": 1,
                "slot_window": window
            }
        }
    }))
    .unwrap()
}

struct LedgerFixture {
    schedule: ServiceEraSchedule,
    anchor: LedgerState,
    tip: LedgerState,
    old_nonce: ZkHash,
    current_nonce: ZkHash,
    previous_nonce: ZkHash,
}

fn ledger_fixture() -> LedgerFixture {
    let old_config = ledger_config(false);
    let new_config = ledger_config(true);
    assert_eq!(old_config.epoch_length(), 100);
    assert_eq!(new_config.epoch_length(), 20);

    let timing = |length| EraTiming {
        slot_duration: Duration::from_secs(1),
        epoch_length: NonZero::new(length).unwrap(),
    };
    let after_genesis =
        EraEntriesAfterGenesis::try_from_iter([(NonZero::new(1).unwrap(), timing(20))]).unwrap();
    let timings = EraSchedule::new(OffsetDateTime::UNIX_EPOCH, timing(100), after_genesis).unwrap();
    let schedule = timings.map(|era| PoWEraSettings {
        slot_window: NonZero::new(if era.era.into_inner() == 0 { 100 } else { 20 }).unwrap(),
        rewards_enabled: true,
        max_data_payload_body_size: PAYLOAD_BODY_SIZE,
    });
    let ledger_schedule = timings.map(|era| {
        if era.era.into_inner() == 0 {
            old_config.clone()
        } else {
            new_config.clone()
        }
    });
    let proof = TestProof(Groth16LeaderProof::genesis());
    let mut state = LedgerState::from_utxos([], &old_config, POOL, &ConsensusExcludedKeys::empty());
    for slot in [10, 80] {
        state = state
            .try_apply_header::<_, [u8; 32]>(
                Slot::new(slot),
                &proof,
                &UpperRuntimeBoundedVec::default(),
                &ledger_schedule,
            )
            .unwrap()
            .0;
    }
    let old_nonce = state.epoch_state().nonce;
    // Ledger::new records the anchor in the ledger's canonical block map.
    let mut ledger = Ledger::new(ANCHOR, state, Arc::new(ledger_schedule));
    let anchor = ledger.state(&ANCHOR).unwrap().clone();
    let mut parent = ANCHOR;
    for slot in [100u8, 110, 120, 130, 140, 150, 160] {
        let block_id = [slot; 32];
        let update = ledger
            .prepare_update::<SignedOps<Preverified, StandardMode>, _, MainnetGasProfile>(
                block_id,
                parent,
                Slot::new(u64::from(slot)),
                &proof,
                &UpperRuntimeBoundedVec::default(),
                empty(),
            )
            .unwrap()
            .verify_batch_proofs()
            .unwrap();
        ledger.commit_update(update);
        parent = block_id;
    }
    let previous_nonce = ledger.state(&PREVIOUS_BLOCK).unwrap().epoch_state().nonce;
    let tip = ledger.state(&TIP).unwrap().clone();
    let current_nonce = tip.epoch_state().nonce;
    assert_eq!(
        tip.accepted_pow_epoch_nonces(),
        (current_nonce, previous_nonce)
    );
    assert_ne!(old_nonce, current_nonce);
    assert_ne!(old_nonce, previous_nonce);
    assert_ne!(current_nonce, previous_nonce);
    assert_eq!(
        tip.mantle_ledger().pow.block_slots().get(&ANCHOR),
        Some(&Slot::new(80))
    );
    assert_eq!(
        tip.mantle_ledger().pow.block_slots().get(&PREVIOUS_BLOCK),
        Some(&Slot::new(150))
    );
    assert_eq!(
        tip.mantle_ledger().pow.block_slots().get(&TIP),
        Some(&Slot::new(160))
    );

    LedgerFixture {
        schedule,
        anchor,
        tip,
        old_nonce,
        current_nonce,
        previous_nonce,
    }
}

fn ticket(nonce: ZkHash, block_hash: [u8; 32], block_slot: u64, seed: u64) -> WinningTicket {
    let secret_key = UnsecuredZkKey::from_rng(&mut StdRng::seed_from_u64(seed));
    WinningTicket {
        tip: HeaderId::from([9; 32]),
        block_slot: Slot::new(block_slot),
        claim: ClaimPowRewardOp {
            epoch_nonce: nonce,
            block_hash,
            public_key: secret_key.to_public_key(),
        },
        secret_key,
    }
}

struct TestChain;
impl ServiceData for TestChain {
    type Settings = ();
    type State = ();
    type StateOperator = ();
    type Message = ConsensusMsg<()>;
}

struct TestBlend;
impl ServiceData for TestBlend {
    type Settings = ();
    type State = ();
    type StateOperator = ();
    type Message = ProxyServiceMessage<ServiceMessage<()>>;
}
impl ServiceComponents for TestBlend {
    type NodeId = ();
}

struct TestApis {
    chain: CryptarchiaServiceApi<TestChain>,
    blend: BlendServiceApi<TestBlend, ()>,
    receiver: Option<Receiver<ProxyServiceMessage<ServiceMessage<()>>>>,
    published: Option<DataPayload>,
    chain_task: Option<JoinHandle<()>>,
}

impl TestApis {
    fn new(ledger: LedgerState, tip_id: [u8; 32]) -> Self {
        let (chain_sender, mut chain_receiver) = channel(2);
        let (blend_sender, blend_receiver) = channel(1);
        let chain_task = spawn(async move {
            let ConsensusMsg::Query(Query::Info { reply_channel }) =
                chain_receiver.recv().await.unwrap()
            else {
                panic!("expected chain info query");
            };
            reply_channel
                .send(ChainServiceInfo {
                    cryptarchia_info: CryptarchiaInfo {
                        lib: HeaderId::from(ANCHOR),
                        lib_slot: Slot::new(80),
                        tip: HeaderId::from(tip_id),
                        slot: ledger.slot(),
                        height: 4,
                        state: State::Online,
                    },
                    phase: PhaseTag::Following,
                })
                .unwrap();
            let ConsensusMsg::Query(Query::GetLedgerState {
                block_id,
                reply_channel,
            }) = chain_receiver.recv().await.unwrap()
            else {
                panic!("expected ledger-state query");
            };
            assert_eq!(block_id, HeaderId::from(tip_id));
            reply_channel.send(Some(ledger)).unwrap();
        });
        Self {
            chain: CryptarchiaServiceApi::new(OutboundRelay::new(chain_sender)),
            blend: BlendServiceApi::new(OutboundRelay::new(blend_sender)),
            receiver: Some(blend_receiver),
            published: None,
            chain_task: Some(chain_task),
        }
    }

    async fn claim(
        &mut self,
        state: &mut PoWServiceState,
        schedule: &ServiceEraSchedule,
        pool: u64,
    ) -> Result<Option<PublishedClaim>, PoWError> {
        let claim = claim_ready_rewards(
            &self.chain,
            &self.blend,
            ZkPublicKey::zero(),
            state,
            schedule,
            pool,
            max_claims_by_payload_size(PAYLOAD_BODY_SIZE),
        );
        let result = if let Some(receiver) = &mut self.receiver {
            tokio_pin!(claim);
            tokio_select! {
                result = &mut claim => result,
                message = receiver.recv() => {
                    let Some(ProxyServiceMessage::Blend { payload, reply }) = message else {
                        panic!("expected a Blend publication");
                    };
                    self.published = Some(payload);
                    reply.send(Ok(())).unwrap();
                    claim.await
                }
            }
        } else {
            claim.await
        };
        self.chain_task.take().unwrap().await.unwrap();
        result
    }

    fn published_claims(&mut self) -> Vec<ClaimPowRewardOp> {
        let Some(DataPayload::Transaction(bytes)) = self.published.take() else {
            panic!("expected a published claim transaction");
        };
        let tx = SignedOps::<Unverified, StandardMode>::from_bytes(&bytes).unwrap();
        tx.op_refs()
            .into_iter()
            .filter_map(|op| match op {
                OpRef::ClaimPowReward(claim) => Some(claim.clone()),
                _ => None,
            })
            .collect()
    }
}

#[tokio_test]
async fn claim_uses_tip_ledger_nonces_and_preserves_window_and_anchor_checks() {
    let fixture = ledger_fixture();
    let old = ticket(fixture.old_nonce, ANCHOR, 80, 1);
    let current = ticket(fixture.current_nonce, TIP, 160, 2);
    let previous = ticket(fixture.previous_nonce, PREVIOUS_BLOCK, 150, 3);
    let orphan = ticket(fixture.current_nonce, OTHER_BLOCK, 160, 4);
    let mut state = PoWServiceState {
        ready_to_claim: vec![
            old.clone(),
            current.clone(),
            previous.clone(),
            orphan.clone(),
        ],
        pending_to_claim: vec![],
    };
    let reward = fixture.tip.mantle_ledger().pow.epoch_reward();
    let mut apis = TestApis::new(fixture.tip, TIP);

    assert!(
        apis.claim(&mut state, &fixture.schedule, 2 * reward)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        apis.published_claims(),
        vec![current.claim.clone(), previous.claim.clone()]
    );
    assert_eq!(
        state
            .pending_to_claim
            .iter()
            .map(|t| t.claim.clone())
            .collect::<Vec<_>>(),
        vec![current.claim, previous.claim]
    );
    assert_eq!(
        state
            .ready_to_claim
            .iter()
            .map(|t| t.claim.clone())
            .collect::<Vec<_>>(),
        vec![old.claim, orphan.claim]
    );
}

#[tokio_test]
async fn a_reorg_can_restore_a_nonce_ineligible_ticket() {
    let fixture = ledger_fixture();
    let old = ticket(fixture.old_nonce, ANCHOR, 80, 1);
    let orphan = ticket(fixture.current_nonce, OTHER_BLOCK, 80, 2);
    let mut state = PoWServiceState {
        ready_to_claim: vec![old.clone(), orphan.clone()],
        pending_to_claim: vec![],
    };
    let before = state.to_bytes().unwrap();
    let tip_pool = fixture.tip.mantle_ledger().pow.reward_pool();
    let mut apis = TestApis::new(fixture.tip, TIP);

    assert!(
        apis.claim(&mut state, &fixture.schedule, tip_pool)
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        apis.receiver.as_mut().unwrap().try_recv(),
        Err(TryRecvError::Empty)
    ));
    assert_eq!(state.to_bytes().unwrap(), before);

    let anchor_pool = fixture.anchor.mantle_ledger().pow.reward_pool();
    let mut apis = TestApis::new(fixture.anchor, ANCHOR);
    assert!(
        apis.claim(&mut state, &fixture.schedule, anchor_pool)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(apis.published_claims(), vec![old.claim.clone()]);
    assert_eq!(state.pending_to_claim[0].claim, old.claim);
    assert_eq!(state.ready_to_claim[0].claim, orphan.claim);
}

#[tokio_test]
async fn pool_cap_moves_only_the_published_eligible_prefix() {
    let fixture = ledger_fixture();
    let old = ticket(fixture.old_nonce, ANCHOR, 80, 1);
    let current = ticket(fixture.current_nonce, TIP, 160, 2);
    let orphan = ticket(fixture.current_nonce, OTHER_BLOCK, 160, 3);
    let previous = ticket(fixture.previous_nonce, PREVIOUS_BLOCK, 150, 4);
    let mut state = PoWServiceState {
        ready_to_claim: vec![
            old.clone(),
            current.clone(),
            orphan.clone(),
            previous.clone(),
        ],
        pending_to_claim: vec![],
    };
    let reward = fixture.tip.mantle_ledger().pow.epoch_reward();
    let mut apis = TestApis::new(fixture.tip, TIP);

    assert!(
        apis.claim(&mut state, &fixture.schedule, reward)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(apis.published_claims(), vec![current.claim.clone()]);
    assert_eq!(state.pending_to_claim.len(), 1);
    assert_eq!(state.pending_to_claim[0].claim, current.claim);
    assert_eq!(
        state
            .ready_to_claim
            .iter()
            .map(|t| t.claim.clone())
            .collect::<Vec<_>>(),
        vec![old.claim, orphan.claim, previous.claim]
    );
}

#[tokio_test]
async fn failed_publication_keeps_the_entire_ticket_state() {
    let fixture = ledger_fixture();
    let mut state = PoWServiceState {
        ready_to_claim: vec![
            ticket(fixture.old_nonce, ANCHOR, 80, 1),
            ticket(fixture.current_nonce, TIP, 160, 2),
        ],
        pending_to_claim: vec![ticket(fixture.previous_nonce, PREVIOUS_BLOCK, 150, 3)],
    };
    let before = state.to_bytes().unwrap();
    let pool = fixture.tip.mantle_ledger().pow.reward_pool();
    let mut apis = TestApis::new(fixture.tip, TIP);
    drop(apis.receiver.take());

    assert!(matches!(
        apis.claim(&mut state, &fixture.schedule, pool).await,
        Err(PoWError::Publish(_))
    ));
    assert_eq!(state.to_bytes().unwrap(), before);
}
