use core::fmt::{Debug, Display};
use std::{collections::HashMap, marker::PhantomData};

use futures::StreamExt as _;
use lb_blend_service::{
    api::{BlendServiceApi, BlendServiceData},
    message::BlendPayload,
};
use lb_chain_service::api::{CryptarchiaServiceApi, CryptarchiaServiceData};
use lb_core::{
    codec::SerializeOp as _,
    mantle::{
        Note, NoteId, Op, OpProof, SignedMantleTx, Utxo, Value,
        gas::MainnetGasProfile,
        ledger::{Inputs, Outputs},
        ops::{NoOpProof, OpId as _, pow::ClaimPowRewardOp, transfer::TransferOp},
        traits::Hashable as _,
        transactions::{
            MantleTxBuilder, MantleTxContext, MantleTxGasContext, OpsProofs, states::Unverified,
        },
    },
};
use lb_key_management_system_keys::keys::{UnsecuredZkKey, ZkPublicKey};
use lb_ledger::LedgerState;
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{AsServiceId, ServiceCore, ServiceData},
};
use serde::Deserialize;
use tracing::error;

use crate::tickets::TicketGenerator;

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
                Some((secret_key, claim)) = winning_tickets.next() => {
                    // Size the reward and fee against the current tip — the state
                    // the tx will actually be applied against — rather than the
                    // (older) block the puzzle anchors to.
                    let tip = cryptarchia_api.info().await?.cryptarchia_info.tip;
                    let ledger_state = cryptarchia_api
                        .get_ledger_state(tip)
                        .await?
                        .expect("Tip ledger state should always be available");
                    match build_reward_claim_tx(
                        settings.claim_address,
                        &ledger_state,
                        claim,
                        secret_key,
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

/// Builds and signs a self-funding reward-claim transaction for a winning
/// ticket.
///
/// The `ClaimPowReward` op mints the reward note, and an appended `Transfer` op
/// spends that very note to pay the gas fee and return the change to
/// `claim_address`. Because the two ops share one transaction and are applied in
/// order, the transfer can reference the UTXO the claim mints — reconstructed
/// here from the same data the ledger uses. The transfer is signed with the
/// ticket's secret key, which owns the reward note.
async fn build_reward_claim_tx(
    claim_address: ZkPublicKey,
    ledger_state: &LedgerState,
    claim: ClaimPowRewardOp,
    sk: UnsecuredZkKey,
) -> Result<SignedMantleTx<Unverified>, DynError> {
    // Value the claim will mint, and the gas prices used to size the fee. Both
    // are read at `ledger_state`; they must match the state the tx applies
    // against, or the reconstructed UTXO / fee will be off.
    let reward_value = ledger_state.mantle_ledger().pow.epoch_reward();
    let context = MantleTxContext {
        gas_context: MantleTxGasContext::new(
            HashMap::new(),
            HashMap::new(),
            ledger_state.get_gas_prices(),
        ),
        leader_reward_amount: 0,
    };

    // Reconstruct the id of the UTXO the claim mints (op_id, output 0, reward
    // note) so the transfer can spend it. Read the claim's fields before move.
    let reward_note_id =
        Utxo::new(claim.op_id(), 0, Note::new(reward_value, claim.public_key)).id();

    // Size the change output to `reward - fee`, measuring the fee against the
    // final transaction shape.
    let fee = estimate_reward_claim_fee(&claim, reward_note_id, claim_address, &context)?;
    let change = reward_value
        .checked_sub(fee)
        .ok_or_else(|| DynError::from("PoW reward does not cover the transaction fee"))?;

    // Op order matters: `ClaimPowReward` mints the reward note, and the
    // `Transfer` spends it — paying the fee and returning the change.
    let transfer = TransferOp::new(
        Inputs::new([reward_note_id]),
        Outputs::new([Note::new(change, claim_address)]),
    );
    let mantle_tx = MantleTxBuilder::new()
        .push_op(Op::ClaimPowReward(claim))?
        .push_op(Op::Transfer(transfer))?
        .build()?;

    // Sign the transfer with the reward note's owning key. Signing is a ZK proof
    // (CPU-heavy), so run it off the async runtime.
    let tx_fr = mantle_tx.hash().to_fr();
    let zk_sig = tokio::task::spawn_blocking(move || sk.sign(&tx_fr)).await??;

    let mut ops_proofs = OpsProofs::empty();
    ops_proofs.try_push(OpProof::None(NoOpProof))?; // ClaimPowReward
    ops_proofs.try_push(OpProof::ZkSig(zk_sig))?; // Transfer
    Ok(SignedMantleTx::new(mantle_tx, ops_proofs))
}

/// Publishes a reward-claim transaction over the blend network.
///
/// The tx is encoded the same way the mempool gossips transactions, so
/// whichever node exits the blend network decodes what it expects.
async fn publish_reward_claim<BlendService, RuntimeServiceId>(
    blend_api: &BlendServiceApi<BlendService, RuntimeServiceId>,
    signed_tx: SignedMantleTx<Unverified>,
) -> Result<(), DynError>
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
/// The fee is the minimum gas cost of the `[ClaimPowReward, Transfer]` shape.
/// The change output's value does not affect gas, so it is measured against a
/// zero-value output.
fn estimate_reward_claim_fee(
    claim: &ClaimPowRewardOp,
    reward_note_id: NoteId,
    claim_address: ZkPublicKey,
    context: &MantleTxContext,
) -> Result<Value, DynError> {
    let probe_transfer = TransferOp::new(
        Inputs::new([reward_note_id]),
        Outputs::new([Note::new(0, claim_address)]),
    );
    let fee = MantleTxBuilder::new()
        .push_op(Op::ClaimPowReward(claim.clone()))?
        .push_op(Op::Transfer(probe_transfer))?
        .minimum_gas_cost::<MainnetGasProfile>(context)?
        .into_inner();
    Ok(fee)
}
