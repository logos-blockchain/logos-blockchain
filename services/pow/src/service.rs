use core::fmt::{Debug, Display};
use std::{collections::HashMap, marker::PhantomData};

use futures::StreamExt as _;
use lb_blend_service::{
    api::{ApiError as BlendApiError, BlendServiceApi, BlendServiceData},
    message::{BlendPayload, TransactionTooLarge},
};
use lb_chain_service::api::{CryptarchiaServiceApi, CryptarchiaServiceData};
use lb_core::{
    codec::{Error as CodecError, SerializeOp as _},
    mantle::{
        Note, NoteId, Op, OpProof, SignedMantleTx, Utxo, Value,
        gas::MainnetGasProfile,
        ledger::{Inputs, InputsError, Outputs},
        ops::{NoOpProof, OpId as _, pow::ClaimPowRewardOp, transfer::TransferOp},
        traits::Hashable as _,
        transactions::{
            GasPrices, MAX_OPS_PER_TX, MantleTxBuilder, MantleTxContext, MantleTxGasContext,
            OpsProofs, TxBuilderError, states::Unverified,
        },
    },
};
use lb_key_management_system_keys::keys::{MAX_ZK_SIGNING_KEYS, UnsecuredZkKey, ZkPublicKey};
use lb_ledger::LedgerState;
use lb_utils::bounded::BoundedError;
use lb_zksign::ZkSignError;
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{AsServiceId, ServiceCore, ServiceData},
};
use serde::Deserialize;
use tokio::task::JoinError;
use tracing::error;

use crate::tickets::{TicketGenerator, WinningTicket};

pub enum PoWServiceMessage {}

#[derive(Clone, Deserialize, Debug)]
pub struct PoWServiceSettings {
    pub claim_address: ZkPublicKey,
}

pub struct PoWServiceState {
    ready_to_claim: Vec<ClaimPowRewardOp>,
    pending_to_claim: Vec<ClaimPowRewardOp>,
}

pub struct PoWService<CryptarchiaService, BlendService, RuntimeServiceId> {
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    state: PoWServiceState,
    settings: PoWServiceSettings,
    _phantom: PhantomData<(CryptarchiaService, BlendService)>,
}

impl<CryptarchiaService, BlendService, RuntimeServiceId> ServiceData
    for PoWService<CryptarchiaService, BlendService, RuntimeServiceId>
{
    type Settings = PoWServiceSettings;
    type State = PoWServiceState;
    type StateOperator = ();
    type Message = PoWServiceMessage;
}

#[async_trait::async_trait]
impl<Tx, CryptarchiaService, BlendService, RuntimeServiceId> ServiceCore<RuntimeServiceId>
    for PoWService<CryptarchiaService, BlendService, RuntimeServiceId>
where
    Tx: Send + Sync + 'static,
    CryptarchiaService: CryptarchiaServiceData<Tx = Tx> + Sync + 'static,
    BlendService: BlendServiceData,
    BlendService::NodeId: Send,
    <BlendService as ServiceData>::Message: Send + 'static,
    RuntimeServiceId: Debug
        + Clone
        + Send
        + Sync
        + Unpin
        + Display
        + 'static
        + AsServiceId<Self>
        + AsServiceId<CryptarchiaService>
        + AsServiceId<BlendService>,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        initial_state: Self::State,
    ) -> Result<Self, DynError> {
        let settings = service_resources_handle
            .settings_handle
            .notifier()
            .get_updated_settings();
        Ok(Self {
            service_resources_handle,
            state: initial_state,
            settings,
            _phantom: PhantomData,
        })
    }

    async fn run(self) -> Result<(), DynError> {
        let Self {
            service_resources_handle,
            settings,
            state: _state,
            _phantom,
        } = self;

        // API wrapper over the chain service relay, used to query chain state.
        let cryptarchia_api = CryptarchiaServiceApi::<CryptarchiaService, RuntimeServiceId>::new(
            service_resources_handle
                .overwatch_handle
                .relay::<CryptarchiaService>()
                .await
                .expect("Relay connection with Cryptarchia chain service should succeed"),
        );

        // API wrapper over the blend service relay, used to publish PoW reward
        // claim transactions to the blend network.
        let blend_api = BlendServiceApi::<BlendService, RuntimeServiceId>::new(
            service_resources_handle
                .overwatch_handle
                .relay::<BlendService>()
                .await
                .expect("Relay connection with BlendService should succeed"),
        );

        // Stream of winning PoW tickets, one per solved puzzle.
        let mut winning_tickets = TicketGenerator::<Tx, CryptarchiaService, RuntimeServiceId>::new(
            cryptarchia_api.clone(),
        )
        .await?;

        let mut inbound_relay = service_resources_handle.inbound_relay;

        service_resources_handle.status_updater.notify_ready();

        loop {
            tokio::select! {
                // No service messages are defined yet, so there is nothing to
                // handle on this branch.
                Some(_message) = inbound_relay.recv() => {}
                // A puzzle was solved: turn the winning claim into a tx and
                // publish it over the blend network.
                Some(WinningTicket { tip, secret_key, claim }) = winning_tickets.next() => {
                    // The ticket carries the chain tip observed when it was found
                    // — the state the tx will be applied against — so we size the
                    // reward and fee against it without an extra round-trip.
                    let ledger_state = cryptarchia_api
                        .get_ledger_state(tip)
                        .await?
                        .expect("Tip ledger state should always be available");
                    // One claim at a time for now; the batch path is exercised
                    // once claim accumulation is added.
                    match build_reward_claim_tx(
                        settings.claim_address,
                        &ledger_state,
                        &[(secret_key, claim)],
                    )
                    .await
                    {
                        Ok(signed_tx) => {
                            if let Err(e) = publish_reward_claim(&blend_api, signed_tx).await {
                                error!("Failed to publish PoW reward claim: {e}");
                            }
                        }
                        Err(e) => error!("Failed to build PoW reward claim: {e}"),
                    }
                }
            }
        }
    }
}

/// Errors produced while building or publishing `PoW` reward-claim
/// transactions.
#[derive(thiserror::Error, Debug)]
pub enum PoWError {
    #[error("PoW rewards are disabled (epoch reward is zero)")]
    RewardsDisabled,
    #[error("no claims fit within the reward pool")]
    RewardPoolExhausted,
    #[error("reward value overflow")]
    RewardOverflow,
    #[error("PoW reward does not cover the transaction fee")]
    RewardBelowFee,
    #[error("failed to build transaction: {0}")]
    TxBuilder(#[from] TxBuilderError),
    #[error("invalid transfer inputs: {0}")]
    Inputs(#[from] InputsError),
    #[error("too many operation proofs: {0}")]
    OpsProofs(#[from] BoundedError),
    #[error("failed to sign transfer: {0}")]
    Sign(#[from] ZkSignError),
    #[error("signing task failed: {0}")]
    SignTask(#[from] JoinError),
    #[error("failed to encode transaction: {0}")]
    Encode(#[from] CodecError),
    #[error("transaction too large for a blend payload: {0}")]
    PayloadTooLarge(#[from] TransactionTooLarge),
    #[error("failed to publish to the blend network: {0}")]
    Publish(#[from] BlendApiError),
}

/// Max inputs a single `Transfer` op can carry: its `ZkSig` is a
/// multi-signature over one key per input, capped at [`MAX_ZK_SIGNING_KEYS`].
/// Hence one transfer per 32 claims.
const MAX_TRANSFER_INPUTS: usize = MAX_ZK_SIGNING_KEYS;

/// Largest claim count whose ops — the claims plus the `ceil(c / 32)` transfers
/// that spend their reward notes — fit within [`MAX_OPS_PER_TX`].
const fn max_claims_by_ops() -> usize {
    let mut claims: usize = 0;
    while (claims + 1) + (claims + 1).div_ceil(MAX_TRANSFER_INPUTS) <= MAX_OPS_PER_TX {
        claims += 1;
    }
    claims
}

/// Builds and signs a single self-funding reward-claim transaction from a batch
/// of winning tickets.
///
/// For every group of up to 32 claims (32 being the multi-signature key limit)
/// a `Transfer` op spends the reward notes those claims mint, so the tx
/// interleaves as `[claims 1..32], transfer, [claims 33..64], transfer, ...`.
/// Each group's claims precede its transfer, so the transfer can reference the
/// freshly minted UTXOs, reconstructed here from the same data the ledger uses,
/// and is signed by those notes' owning keys.
///
/// Only as many claims are taken as the reward pool can fund and the per-tx op
/// limit allows.
async fn build_reward_claim_tx(
    claim_address: ZkPublicKey,
    ledger_state: &LedgerState,
    tickets: &[(UnsecuredZkKey, ClaimPowRewardOp)],
) -> Result<SignedMantleTx<Unverified>, PoWError> {
    // Value each claim will mint and the pool that funds them, read at
    // `ledger_state`; they must match the state the tx applies against, or the
    // reconstructed UTXOs / fee will be off.
    let pow = &ledger_state.mantle_ledger().pow;
    build_reward_claim_tx_inner(
        claim_address,
        pow.epoch_reward(),
        pow.reward_pool(),
        ledger_state.get_gas_prices(),
        tickets,
    )
    .await
}

/// Inner builder over plain reward/gas values, so it can be exercised without a
/// full [`LedgerState`]. See [`build_reward_claim_tx`].
async fn build_reward_claim_tx_inner(
    claim_address: ZkPublicKey,
    reward_value: Value,
    reward_pool: Value,
    gas_prices: GasPrices,
    tickets: &[(UnsecuredZkKey, ClaimPowRewardOp)],
) -> Result<SignedMantleTx<Unverified>, PoWError> {
    if reward_value == 0 {
        return Err(PoWError::RewardsDisabled);
    }
    let context = MantleTxContext {
        gas_context: MantleTxGasContext::new(HashMap::new(), HashMap::new(), gas_prices),
        leader_reward_amount: 0,
    };

    // Take as many claims as the pool can fund and the op budget allows.
    let claim_count = tickets
        .len()
        .min((reward_pool / reward_value) as usize)
        .min(max_claims_by_ops());
    if claim_count == 0 {
        return Err(PoWError::RewardPoolExhausted);
    }
    let tickets = &tickets[..claim_count];

    // Reconstruct the id of the UTXO each claim mints (op_id, output 0, reward
    // note), so the transfers can spend them.
    let note_ids: Vec<NoteId> = tickets
        .iter()
        .map(|(_, claim)| {
            Utxo::new(claim.op_id(), 0, Note::new(reward_value, claim.public_key)).id()
        })
        .collect();

    // Size the change against the final tx shape, then charge the whole fee to
    // the first transfer's output.
    let fee = estimate_reward_claim_fee(tickets, &note_ids, claim_address, &context)?;
    let change_outputs = change_outputs(&note_ids, reward_value, fee)?;
    let transfers = transfer_ops(&note_ids, claim_address, &change_outputs)?;

    let mantle_tx = push_reward_claim_ops(MantleTxBuilder::new(), tickets, transfers)?.build()?;

    // Sign each transfer with the keys owning its input notes (a multi-signature
    // over the whole tx hash). Signing is a ZK proof (CPU-heavy), so run it off
    // the async runtime.
    let tx_fr = mantle_tx.hash().to_fr();
    let sk_groups: Vec<Vec<UnsecuredZkKey>> = tickets
        .chunks(MAX_TRANSFER_INPUTS)
        .map(|group| group.iter().map(|(sk, _)| sk.clone()).collect())
        .collect();
    let zk_sigs = tokio::task::spawn_blocking(move || {
        sk_groups
            .iter()
            .map(|sks| UnsecuredZkKey::multi_sign(sks, &tx_fr))
            .collect::<Result<Vec<_>, _>>()
    })
    .await??;

    // Proofs follow the op order: a `None` per claim in the group, then that
    // group's transfer `ZkSig`.
    let mut ops_proofs = OpsProofs::empty();
    for (group, zk_sig) in tickets.chunks(MAX_TRANSFER_INPUTS).zip(zk_sigs) {
        for _ in group {
            ops_proofs.try_push(OpProof::None(NoOpProof))?;
        }
        ops_proofs.try_push(OpProof::ZkSig(zk_sig))?;
    }
    Ok(SignedMantleTx::new(mantle_tx, ops_proofs))
}

/// Builds the `Transfer` ops spending `note_ids`, grouped into batches of up to
/// [`MAX_TRANSFER_INPUTS`] inputs, each returning `change_outputs[group]` to
/// `claim_address`.
fn transfer_ops(
    note_ids: &[NoteId],
    claim_address: ZkPublicKey,
    change_outputs: &[Value],
) -> Result<Vec<TransferOp>, PoWError> {
    note_ids
        .chunks(MAX_TRANSFER_INPUTS)
        .zip(change_outputs)
        .map(|(group, &change)| {
            Ok(TransferOp::new(
                Inputs::try_new(group.to_vec())?,
                Outputs::new([Note::new(change, claim_address)]),
            ))
        })
        .collect()
}

/// Pushes the interleaved reward-claim ops onto `builder`: for every group of
/// up to [`MAX_TRANSFER_INPUTS`] claims, the claim ops followed by that group's
/// transfer. This is where the leaf ops are wrapped into their [`Op`] variants.
fn push_reward_claim_ops(
    mut builder: MantleTxBuilder,
    tickets: &[(UnsecuredZkKey, ClaimPowRewardOp)],
    transfers: Vec<TransferOp>,
) -> Result<MantleTxBuilder, PoWError> {
    for (claim_group, transfer) in tickets.chunks(MAX_TRANSFER_INPUTS).zip(transfers) {
        builder = builder
            .extend_ops(
                claim_group
                    .iter()
                    .map(|(_, claim)| Op::ClaimPowReward(claim.clone())),
            )?
            .push_op(Op::Transfer(transfer))?;
    }
    Ok(builder)
}

/// The change value each transfer group returns: the reward its notes carry,
/// with the whole fee charged to the first group.
fn change_outputs(
    note_ids: &[NoteId],
    reward_value: Value,
    fee: Value,
) -> Result<Vec<Value>, PoWError> {
    note_ids
        .chunks(MAX_TRANSFER_INPUTS)
        .enumerate()
        .map(|(group_index, group)| {
            let group_reward = (group.len() as Value)
                .checked_mul(reward_value)
                .ok_or(PoWError::RewardOverflow)?;
            if group_index == 0 {
                group_reward
                    .checked_sub(fee)
                    .ok_or(PoWError::RewardBelowFee)
            } else {
                Ok(group_reward)
            }
        })
        .collect()
}

/// Publishes a reward-claim transaction over the blend network.
///
/// The tx is encoded the same way the mempool gossips transactions, so
/// whichever node exits the blend network decodes what it expects.
async fn publish_reward_claim<BlendService, RuntimeServiceId>(
    blend_api: &BlendServiceApi<BlendService, RuntimeServiceId>,
    signed_tx: SignedMantleTx<Unverified>,
) -> Result<(), PoWError>
where
    BlendService: BlendServiceData,
    BlendService::NodeId: Send,
    RuntimeServiceId: Sync,
{
    let payload = BlendPayload::transaction(signed_tx.to_bytes()?.to_vec())?;
    blend_api.publish(payload).await?;
    Ok(())
}

/// Estimates the gas fee of a self-funding reward-claim transaction.
///
/// The fee is the minimum gas cost of the whole `[claims.., transfers..]`
/// shape. The change output values do not affect gas, so it is measured against
/// zero-value outputs.
fn estimate_reward_claim_fee(
    tickets: &[(UnsecuredZkKey, ClaimPowRewardOp)],
    note_ids: &[NoteId],
    claim_address: ZkPublicKey,
    context: &MantleTxContext,
) -> Result<Value, PoWError> {
    // Change values don't affect gas, so probe with zero-value outputs.
    let num_groups = note_ids.len().div_ceil(MAX_TRANSFER_INPUTS);
    let transfers = transfer_ops(note_ids, claim_address, &vec![0; num_groups])?;
    let fee = push_reward_claim_ops(MantleTxBuilder::new(), tickets, transfers)?
        .minimum_gas_cost::<MainnetGasProfile>(context)?
        .into_inner();
    Ok(fee)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use lb_core::mantle::{
        Note, NoteId, Op, OpProof, SignedMantleTx, Utxo,
        ops::{OpId as _, pow::ClaimPowRewardOp},
        transactions::{
            GasPrices, MAX_OPS_PER_TX, MantleTxBuilder, MantleTxContext, MantleTxGasContext,
            mantle_tx::MantleTx as _, states::Unverified,
        },
    };
    use lb_key_management_system_keys::keys::{UnsecuredZkKey, ZkPublicKey};

    use super::{
        MAX_TRANSFER_INPUTS, PoWError, build_reward_claim_tx_inner, change_outputs,
        estimate_reward_claim_fee, max_claims_by_ops, push_reward_claim_ops, transfer_ops,
    };

    const REWARD: u64 = 1_000_000;
    const POOL: u64 = 1_000_000_000;

    /// A distinct dummy note id, derived like the ones the builder
    /// reconstructs.
    fn note_id(seed: u8) -> NoteId {
        Utxo::new([seed; 32], 0, Note::new(1, ZkPublicKey::zero())).id()
    }

    /// A ticket whose claim is owned by the ticket's own key.
    fn ticket() -> (UnsecuredZkKey, ClaimPowRewardOp) {
        let secret_key = UnsecuredZkKey::from_rng(&mut rand::thread_rng());
        let claim = ClaimPowRewardOp {
            epoch_nonce: *ZkPublicKey::zero().as_fr(),
            block_hash: [0u8; 32],
            public_key: secret_key.to_public_key(),
        };
        (secret_key, claim)
    }

    fn context() -> MantleTxContext {
        MantleTxContext {
            gas_context: MantleTxGasContext::new(
                HashMap::default(),
                HashMap::default(),
                GasPrices::new(1, 1),
            ),
            leader_reward_amount: 0,
        }
    }

    /// Asserts a built tx respects the transaction's own structural limits: the
    /// op budget, the per-transfer signing-key limit, and a correctly-typed
    /// proof per op (the last via the ledger's stateless `preverify`).
    fn assert_within_tx_limits(tx: &SignedMantleTx<Unverified>) {
        let ops = tx.mantle_tx().ops();
        assert!(ops.len() <= MAX_OPS_PER_TX, "op count exceeds the tx limit");
        for op in ops.iter() {
            if let Op::Transfer(transfer) = op {
                assert!(
                    (&transfer.inputs).into_iter().count() <= MAX_TRANSFER_INPUTS,
                    "transfer inputs exceed the signing-key limit"
                );
            }
        }
        tx.clone()
            .preverify()
            .expect("built tx should pass stateless structural verification");
    }

    #[test]
    fn max_claims_by_ops_saturates_the_op_budget() {
        let claims = max_claims_by_ops();
        assert!(claims + claims.div_ceil(MAX_TRANSFER_INPUTS) <= MAX_OPS_PER_TX);
        assert!((claims + 1) + (claims + 1).div_ceil(MAX_TRANSFER_INPUTS) > MAX_OPS_PER_TX);
    }

    #[test]
    fn change_outputs_returns_reward_minus_fee_for_a_single_group() {
        let notes: Vec<NoteId> = (0u8..5).map(note_id).collect();
        let outputs = change_outputs(&notes, 100, 30).unwrap();
        assert_eq!(outputs, vec![5 * 100 - 30]);
    }

    #[test]
    fn change_outputs_charges_the_whole_fee_to_the_first_group() {
        // 40 notes -> two groups of 32 and 8.
        let notes: Vec<NoteId> = (0u8..40).map(note_id).collect();
        let outputs = change_outputs(&notes, 100, 30).unwrap();
        assert_eq!(outputs, vec![32 * 100 - 30, 8 * 100]);
    }

    #[test]
    fn change_outputs_errors_when_reward_below_fee() {
        let notes = vec![note_id(0)];
        assert!(matches!(
            change_outputs(&notes, 10, 20),
            Err(PoWError::RewardBelowFee)
        ));
    }

    #[test]
    fn change_outputs_errors_on_overflow() {
        let notes: Vec<NoteId> = (0u8..2).map(note_id).collect();
        assert!(matches!(
            change_outputs(&notes, u64::MAX, 0),
            Err(PoWError::RewardOverflow)
        ));
    }

    #[test]
    fn transfer_ops_groups_inputs_by_the_signing_key_limit() {
        // 70 notes -> groups of 32, 32, 6.
        let notes: Vec<NoteId> = (0u8..70).map(note_id).collect();
        let transfers = transfer_ops(&notes, ZkPublicKey::zero(), &[10, 20, 30]).unwrap();
        let input_counts: Vec<usize> = transfers
            .iter()
            .map(|t| (&t.inputs).into_iter().count())
            .collect();
        assert_eq!(input_counts, vec![32, 32, 6]);
    }

    #[test]
    fn push_reward_claim_ops_interleaves_claims_and_transfers() {
        let tickets: Vec<_> = std::iter::repeat_with(ticket).take(40).collect();
        let notes: Vec<NoteId> = (0u8..40).map(note_id).collect();
        let changes = change_outputs(&notes, 100, 0).unwrap();
        let transfers = transfer_ops(&notes, ZkPublicKey::zero(), &changes).unwrap();

        let tx = push_reward_claim_ops(MantleTxBuilder::new(), &tickets, transfers)
            .unwrap()
            .build()
            .unwrap();
        let ops = tx.ops();

        assert_eq!(ops.len(), 42); // 40 claims + 2 transfers
        assert!(
            ops[..32]
                .iter()
                .all(|op| matches!(op, Op::ClaimPowReward(_)))
        );
        assert!(matches!(ops[32], Op::Transfer(_)));
        assert!(
            ops[33..41]
                .iter()
                .all(|op| matches!(op, Op::ClaimPowReward(_)))
        );
        assert!(matches!(ops[41], Op::Transfer(_)));
    }

    #[test]
    fn estimate_reward_claim_fee_is_positive_with_nonzero_gas_prices() {
        let tickets = vec![ticket()];
        let notes = vec![note_id(0)];
        let fee =
            estimate_reward_claim_fee(&tickets, &notes, ZkPublicKey::zero(), &context()).unwrap();
        assert!(fee > 0);
    }

    #[tokio::test]
    async fn build_reward_claim_tx_produces_a_claim_and_signed_transfer() {
        let (secret_key, claim) = ticket();
        let expected_note = Utxo::new(claim.op_id(), 0, Note::new(REWARD, claim.public_key)).id();

        let tx = build_reward_claim_tx_inner(
            ZkPublicKey::zero(),
            REWARD,
            POOL,
            GasPrices::new(1, 1),
            &[(secret_key, claim)],
        )
        .await
        .unwrap();

        assert_within_tx_limits(&tx);
        let ops = tx.mantle_tx().ops();
        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], Op::ClaimPowReward(_)));
        let Op::Transfer(transfer) = &ops[1] else {
            panic!("second op should be a transfer");
        };
        let inputs: Vec<NoteId> = (&transfer.inputs).into_iter().copied().collect();
        assert_eq!(inputs, vec![expected_note]);

        // A `None` proof for the claim, a `ZkSig` for the transfer.
        let proofs = tx.ops_proofs();
        assert_eq!(proofs.len(), 2);
        assert!(matches!(proofs[0], OpProof::None(_)));
        assert!(matches!(proofs[1], OpProof::ZkSig(_)));
    }

    #[tokio::test]
    async fn build_reward_claim_tx_errors_when_rewards_disabled() {
        let result = build_reward_claim_tx_inner(
            ZkPublicKey::zero(),
            0,
            POOL,
            GasPrices::new(1, 1),
            &[ticket()],
        )
        .await;
        assert!(matches!(result, Err(PoWError::RewardsDisabled)));
    }

    #[tokio::test]
    async fn build_reward_claim_tx_errors_when_pool_cannot_fund_a_claim() {
        // Pool below a single reward -> nothing fundable.
        let result = build_reward_claim_tx_inner(
            ZkPublicKey::zero(),
            REWARD,
            REWARD - 1,
            GasPrices::new(1, 1),
            &[ticket()],
        )
        .await;
        assert!(matches!(result, Err(PoWError::RewardPoolExhausted)));
    }

    #[tokio::test]
    async fn build_reward_claim_tx_caps_claims_to_the_reward_pool() {
        // Pool funds only two claims, but five tickets are offered.
        let tickets: Vec<_> = std::iter::repeat_with(ticket).take(5).collect();
        let tx = build_reward_claim_tx_inner(
            ZkPublicKey::zero(),
            REWARD,
            2 * REWARD,
            GasPrices::new(1, 1),
            &tickets,
        )
        .await
        .unwrap();

        assert_within_tx_limits(&tx);
        let ops = tx.mantle_tx().ops();
        let claims = ops
            .iter()
            .filter(|op| matches!(op, Op::ClaimPowReward(_)))
            .count();
        assert_eq!(claims, 2); // 2 claims + 1 transfer
        assert_eq!(ops.len(), 3);
    }

    #[tokio::test]
    async fn build_reward_claim_tx_caps_claims_to_the_op_limit_and_stays_within_it() {
        // More tickets and pool room than the op budget allows: the cap is the
        // op limit, and the resulting tx must still fit within it.
        let cap = max_claims_by_ops();
        let tickets: Vec<_> = std::iter::repeat_with(ticket).take(cap + 50).collect();
        let tx = build_reward_claim_tx_inner(
            ZkPublicKey::zero(),
            REWARD,
            POOL, // POOL / REWARD = 1000 > cap, so the op limit binds
            GasPrices::new(1, 1),
            &tickets,
        )
        .await
        .unwrap();

        assert_within_tx_limits(&tx);
        let ops = tx.mantle_tx().ops();
        let claims = ops
            .iter()
            .filter(|op| matches!(op, Op::ClaimPowReward(_)))
            .count();
        let transfers = ops.len() - claims;
        assert_eq!(claims, cap);
        assert_eq!(transfers, cap.div_ceil(MAX_TRANSFER_INPUTS));
        // The cap is the largest batch that still fits the op budget exactly.
        assert!(ops.len() <= MAX_OPS_PER_TX);
        assert!(claims + 1 + (claims + 1).div_ceil(MAX_TRANSFER_INPUTS) > MAX_OPS_PER_TX);
    }
}
