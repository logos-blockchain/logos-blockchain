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
            MAX_OPS_PER_TX, MantleTxBuilder, MantleTxContext, MantleTxGasContext, OpsProofs,
            TxBuilderError, states::Unverified,
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

pub struct PoWServiceState<Tx> {
    claims: Vec<ClaimPowRewardOp>,
    transactions: Vec<Tx>,
}

pub struct PoWService<Tx, CryptarchiaService, BlendService, RuntimeServiceId> {
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    state: PoWServiceState<Tx>,
    settings: PoWServiceSettings,
    _phantom: PhantomData<(CryptarchiaService, BlendService)>,
}

impl<Tx, CryptarchiaService, BlendService, RuntimeServiceId> ServiceData
    for PoWService<Tx, CryptarchiaService, BlendService, RuntimeServiceId>
{
    type Settings = PoWServiceSettings;
    type State = PoWServiceState<Tx>;
    type StateOperator = ();
    type Message = PoWServiceMessage;
}

#[async_trait::async_trait]
impl<Tx, CryptarchiaService, BlendService, RuntimeServiceId> ServiceCore<RuntimeServiceId>
    for PoWService<Tx, CryptarchiaService, BlendService, RuntimeServiceId>
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

/// Errors produced while building or publishing PoW reward-claim transactions.
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
    let reward_value = pow.epoch_reward();
    let reward_pool = pow.reward_pool();
    if reward_value == 0 {
        return Err(PoWError::RewardsDisabled);
    }
    let context = MantleTxContext {
        gas_context: MantleTxGasContext::new(
            HashMap::new(),
            HashMap::new(),
            ledger_state.get_gas_prices(),
        ),
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
