use std::fmt::{Debug, Display};

use lb_binary_codec::bincode::{DeserializeOp, SerializeOp};
use lb_blend_service::message::{
    CoreStatus, DataPayload, NetworkInfo, ProxyServiceMessage, ServiceMessage,
};
use lb_chain_service::{
    BlendRewards, Epoch, State,
    api::{CryptarchiaServiceApi, CryptarchiaServiceData},
};
use lb_core::{
    mantle::{NoteId, Value},
    sdp::{DeclarationId, ProviderId},
};
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_network_service::backends::libp2p::PeerId;
use lb_sdp_service::{
    SdpService, api::SdpServiceApi, mempool::SdpMempoolAdapter, state::SdpStateStorage, status,
};
use overwatch::{
    DynError,
    overwatch::OverwatchHandle,
    services::{AsServiceId, ServiceData},
};
use serde::Serialize;
use tokio::sync::oneshot;

pub async fn blend_info<BlendService, RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
) -> Result<Option<NetworkInfo<PeerId>>, DynError>
where
    BlendService: ServiceData<Message = ProxyServiceMessage<ServiceMessage<PeerId>>>,
    RuntimeServiceId: AsServiceId<BlendService> + Debug + Sync + Display + 'static,
{
    let relay = handle.relay::<BlendService>().await?;
    let (sender, receiver) = oneshot::channel();

    relay
        .send(ServiceMessage::GetNetworkInfo { reply: sender }.into())
        .await
        .map_err(|error| DynError::from(error.to_string()))?;

    receiver.await.map_err(|e| Box::new(e) as DynError)
}

pub async fn blend_zk_id<BlendService, RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
) -> Result<ZkPublicKey, DynError>
where
    BlendService: ServiceData<Message = ProxyServiceMessage<ServiceMessage<PeerId>>>,
    RuntimeServiceId: AsServiceId<BlendService> + Debug + Sync + Display + 'static,
{
    let relay = handle.relay::<BlendService>().await?;
    let (sender, receiver) = oneshot::channel();

    relay
        .send(ProxyServiceMessage::GetZkId { reply: sender })
        .await
        .map_err(|error| DynError::from(error.to_string()))?;

    receiver.await.map_err(|e| Box::new(e) as DynError)
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ServiceNoteError {
    #[error("holds no notes")]
    NoNotes,
    #[error("all {locked} notes are already locked for blend")]
    AllLocked { locked: usize },
    #[error(
        "no single note meets min stake {threshold}: largest is {largest}, total {total}; consolidation may be possible"
    )]
    NoSingleNoteMeetsThreshold {
        largest: Value,
        total: Value,
        threshold: Value,
    },
    #[error("insufficient total collateral: {total} < min stake {threshold}")]
    InsufficientTotal { total: Value, threshold: Value },
}

/// Picks the smallest unlocked note that meets `threshold`, so the least
/// collateral is locked and larger notes stay free.
pub fn select_service_note<Id: Copy>(
    notes: &[(Id, Value)],
    is_locked: impl Fn(Id) -> bool,
    threshold: Value,
) -> Result<Id, ServiceNoteError> {
    if notes.is_empty() {
        return Err(ServiceNoteError::NoNotes);
    }

    let unlocked: Vec<_> = notes.iter().filter(|(id, _)| !is_locked(*id)).collect();
    if unlocked.is_empty() {
        return Err(ServiceNoteError::AllLocked {
            locked: notes.len(),
        });
    }

    if let Some((id, _)) = unlocked
        .iter()
        .filter(|(_, value)| *value >= threshold)
        .min_by_key(|(_, value)| *value)
    {
        return Ok(*id);
    }

    let total = unlocked.iter().map(|(_, value)| *value).sum();
    if total >= threshold {
        let largest = unlocked.iter().map(|(_, value)| *value).max().unwrap_or(0);
        return Err(ServiceNoteError::NoSingleNoteMeetsThreshold {
            largest,
            total,
            threshold,
        });
    }

    Err(ServiceNoteError::InsufficientTotal { total, threshold })
}

/// This node's core status, or `None` when it is not a core node.
pub async fn blend_core_status<BlendService, RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
) -> Result<Option<CoreStatus>, DynError>
where
    BlendService: ServiceData<Message = ProxyServiceMessage<ServiceMessage<PeerId>>>,
    RuntimeServiceId: AsServiceId<BlendService> + Debug + Sync + Display + 'static,
{
    let relay = handle.relay::<BlendService>().await?;
    let (sender, receiver) = oneshot::channel();

    relay
        .send(ServiceMessage::GetCoreStatus { reply: sender }.into())
        .await
        .map_err(|error| DynError::from(error.to_string()))?;

    receiver.await.map_err(|error| Box::new(error) as DynError)
}

pub async fn blend_join_network<BlendService, RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
    locator: lb_core::sdp::Locator,
    service_note_id: NoteId,
) -> Result<lb_sdp_service::SdpSubmission, DynError>
where
    BlendService: ServiceData<Message = ProxyServiceMessage<ServiceMessage<PeerId>>>,
    RuntimeServiceId: AsServiceId<BlendService> + Debug + Sync + Display + 'static,
{
    let relay = handle.relay::<BlendService>().await?;
    let (sender, receiver) = oneshot::channel();

    relay
        .send(ProxyServiceMessage::JoinAsCore {
            locator,
            service_note_id,
            reply: sender,
        })
        .await
        .map_err(|error| DynError::from(error.to_string()))?;

    let result = receiver.await.map_err(|e| Box::new(e) as DynError)??;

    Ok(result)
}

/// Sends a transaction through the Blend network without adding it to this
/// node's own mempool.
///
/// Returns the transaction's id. Getting one back means the transaction was
/// accepted for blending, not that it was sent.
pub async fn blend_transaction<BlendService, Transaction, Id, RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
    transaction: Transaction,
    id: impl Fn(&Transaction) -> Id,
) -> Result<Id, DynError>
where
    BlendService: ServiceData<Message = ProxyServiceMessage<ServiceMessage<PeerId>>>,
    Transaction: SerializeOp,
    RuntimeServiceId: AsServiceId<BlendService> + Debug + Sync + Display + 'static,
{
    // Encoded the same way the mempool gossips transactions, so that whichever
    // node exits this one decodes what it expects.
    let payload = DataPayload::try_from_transaction(&transaction)?;
    let relay = handle.relay::<BlendService>().await?;

    relay
        .send(ServiceMessage::Blend(payload).into())
        .await
        .map_err(|error| DynError::from(error.to_string()))?;

    Ok(id(&transaction))
}

/// The ids of the transactions this node is still waiting on a `PoW` solution
/// for.
///
/// Only those: once a transaction has been encapsulated it is in the
/// scheduler's hands, waiting for a release round, and no longer reported here.
// TODO: Have Blend hand back transactions rather than bytes, so that this can
// report their ids without decoding them again.
pub async fn blend_pending_transactions<BlendService, Transaction, Id, RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
    id: impl Fn(&Transaction) -> Id,
) -> Result<Vec<Id>, DynError>
where
    BlendService: ServiceData<Message = ProxyServiceMessage<ServiceMessage<PeerId>>>,
    Transaction: DeserializeOp,
    RuntimeServiceId: AsServiceId<BlendService> + Debug + Sync + Display + 'static,
{
    let relay = handle.relay::<BlendService>().await?;
    let (sender, receiver) = oneshot::channel();

    relay
        .send(ServiceMessage::GetPendingTransactions { reply: sender }.into())
        .await
        .map_err(|error| DynError::from(error.to_string()))?;

    receiver
        .await
        .map_err(Box::new)?
        .iter()
        .map(|encoded_transaction| {
            // These are transactions this node encoded itself on the way in, so
            // a decoding failure here is this node disagreeing with itself.
            Ok(id(&Transaction::from_bytes(encoded_transaction)?))
        })
        .collect()
}

/// Gets this node's Blend rewards at the tip.
///
/// # Errors
///
/// Fails right away if the chain is not `Online`, and if the node has no Blend
/// declaration in the ledger.
pub async fn blend_rewards<
    MempoolAdapter,
    WalletAdapter,
    ChainService,
    StateStorage,
    RuntimeServiceId,
>(
    handle: &OverwatchHandle<RuntimeServiceId>,
) -> Result<BlendRewards, DynError>
where
    MempoolAdapter: SdpMempoolAdapter + Send + Sync + 'static,
    ChainService: CryptarchiaServiceData<Tx: Send> + Send + Sync + 'static,
    StateStorage: SdpStateStorage<RuntimeServiceId>,
    RuntimeServiceId: Send
        + Sync
        + Debug
        + Display
        + 'static
        + AsServiceId<ChainService>
        + AsServiceId<
            SdpService<MempoolAdapter, WalletAdapter, ChainService, StateStorage, RuntimeServiceId>,
        >,
{
    let chain_api = CryptarchiaServiceApi::<ChainService>::from_overwatch_handle(handle).await;
    if chain_api.info().await?.cryptarchia_info.state != State::Online {
        return Err(DynError::from("The node is not online yet."));
    }

    let sdp_api = SdpServiceApi::<
        SdpService<MempoolAdapter, WalletAdapter, ChainService, StateStorage, RuntimeServiceId>,
    >::from_overwatch_handle(handle)
    .await;
    let sdp_status = sdp_api.get_status().await?;
    let Some(participation) = sdp_status.participation else {
        return Err(DynError::from("The node has no Blend declaration."));
    };

    let rewards = chain_api
        .get_blend_rewards(participation.declaration_id())
        .await?
        .ok_or_else(|| DynError::from("The node has no Blend declaration in the ledger."))?;
    Ok(rewards)
}

/// Where the node's Blend declaration is in its lifecycle.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeclarationState {
    /// No declaration posted.
    NotDeclared,
    /// Posted, but not in the ledger yet.
    Pending,
    /// In the ledger, but not in an epoch snapshot yet.
    Activating,
    /// In the current epoch's snapshot.
    Active,
    /// Past `first_epoch_in_snapshot`, but not in the current epoch's snapshot.
    Inactive,
    /// Withdrawal posted. Once it lands, the declaration is withdrawn at
    /// `withdraw_at`, and the locked note unlocks the epoch after.
    Withdrawn,
}

impl From<status::DeclarationState> for DeclarationState {
    fn from(state: status::DeclarationState) -> Self {
        match state {
            status::DeclarationState::NotDeclared => Self::NotDeclared,
            status::DeclarationState::Pending => Self::Pending,
            status::DeclarationState::Activating => Self::Activating,
            status::DeclarationState::Active => Self::Active,
            status::DeclarationState::Inactive => Self::Inactive,
            status::DeclarationState::Withdrawn => Self::Withdrawn,
        }
    }
}

/// The state of the node's activity proof for the activity epoch.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProofState {
    /// Submitted, but not in the ledger yet.
    Pending,
    /// In the ledger.
    Accepted,
    /// Not in the ledger, and not pending.
    Failed,
    /// No proof owed.
    NotRequired,
}

/// Why a [`ProofState`] is `Failed` or `NotRequired`.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProofReason {
    /// `Failed`: no proof submitted. Also seen briefly at the start of an
    /// epoch, before the proof is submitted.
    NoProof,
    /// `Failed`: the node's declaration wasn't found when submitting the
    /// proof.
    DeclarationNotFound,
    /// `Failed`: the proof transaction couldn't be built.
    TransactionBuildFailed,
    /// `Failed`: the mempool rejected the proof transaction.
    MempoolRejected,
    /// `NotRequired`: the chain is in its first epoch, so there's no activity
    /// epoch.
    FirstEpoch,
    /// `NotRequired`: the activity epoch had fewer providers than the minimum
    /// network size, so the ledger accepts no proofs for it.
    NetworkBelowMinimum,
    /// `NotRequired`: the node wasn't a provider in the activity epoch, as in
    /// its first active epoch.
    NotInActivityEpoch,
}

impl From<status::ProofFailureReason> for ProofReason {
    fn from(reason: status::ProofFailureReason) -> Self {
        match reason {
            status::ProofFailureReason::NoProof => Self::NoProof,
            status::ProofFailureReason::DeclarationNotFound => Self::DeclarationNotFound,
            status::ProofFailureReason::TransactionBuildFailed => Self::TransactionBuildFailed,
            status::ProofFailureReason::MempoolRejected => Self::MempoolRejected,
        }
    }
}

impl From<status::ProofNotRequiredReason> for ProofReason {
    fn from(reason: status::ProofNotRequiredReason) -> Self {
        match reason {
            status::ProofNotRequiredReason::FirstEpoch => Self::FirstEpoch,
            status::ProofNotRequiredReason::NetworkBelowMinimum => Self::NetworkBelowMinimum,
            status::ProofNotRequiredReason::NotInActivityEpoch => Self::NotInActivityEpoch,
        }
    }
}

/// The node's Blend declaration in the ledger.
#[derive(Debug, Clone, Serialize)]
pub struct DeclarationStatus {
    pub id: DeclarationId,
    pub provider_id: ProviderId,
    pub zk_id: ZkPublicKey,
    /// The note locked by the declaration.
    pub locked_note_id: NoteId,
    /// The epoch of the block that contained the declaration.
    pub created_epoch: Epoch,
    /// The first epoch whose snapshot includes the declaration.
    pub first_epoch_in_snapshot: Epoch,
    /// The epoch at which the declaration is scheduled to be withdrawn. `None`
    /// until the withdrawal lands.
    pub withdraw_at: Option<Epoch>,
}

impl From<status::DeclarationStatus> for DeclarationStatus {
    fn from(status: status::DeclarationStatus) -> Self {
        let declaration = status.declaration;
        Self {
            id: status.id,
            provider_id: declaration.provider_id,
            zk_id: declaration.zk_id,
            locked_note_id: declaration.service_note_id,
            created_epoch: declaration.created,
            first_epoch_in_snapshot: status.first_epoch_in_snapshot,
            withdraw_at: declaration.withdraw_at,
        }
    }
}

/// The node's Blend activity.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ActivityStatus {
    /// The epoch being paid out. `None` when there's no activity epoch.
    pub activity_epoch: Option<Epoch>,
    /// The state of the activity proof for `activity_epoch`.
    pub proof_state: ProofState,
    /// Set when `proof_state` is `Failed` or `NotRequired`.
    pub proof_reason: Option<ProofReason>,
    /// The epoch in which the node's latest activity proof landed. Starts at
    /// the declaration's `first_epoch_in_snapshot`.
    pub last_active_epoch: Epoch,
}

impl From<status::ActivityStatus> for ActivityStatus {
    fn from(status: status::ActivityStatus) -> Self {
        let (proof_state, proof_reason) = match status.proof_state {
            status::ProofState::Pending => (ProofState::Pending, None),
            status::ProofState::Accepted => (ProofState::Accepted, None),
            status::ProofState::Failed(reason) => (ProofState::Failed, Some(reason.into())),
            status::ProofState::NotRequired(reason) => {
                (ProofState::NotRequired, Some(reason.into()))
            }
        };
        Self {
            activity_epoch: status.activity_epoch,
            proof_state,
            proof_reason,
            last_active_epoch: status.last_active_epoch,
        }
    }
}

/// The negotiated peers for the current epoch.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct SessionPeers {
    pub total: usize,
    pub healthy: usize,
}

impl From<lb_blend_service::message::SessionPeers> for SessionPeers {
    fn from(peers: lb_blend_service::message::SessionPeers) -> Self {
        Self {
            total: peers.total,
            healthy: peers.healthy,
        }
    }
}

/// The node's Blend status.
#[derive(Debug, Clone, Serialize)]
pub struct BlendStatus {
    /// Whether the node is a core node, as opposed to an edge node.
    pub is_core: bool,
    pub declaration_state: DeclarationState,
    /// The epoch of the chain tip.
    pub current_epoch: Epoch,
    /// `None` while the declaration isn't in the ledger.
    pub declaration: Option<DeclarationStatus>,
    /// `None` while the declaration isn't in the ledger, or while it's
    /// activating.
    pub activity: Option<ActivityStatus>,
    /// Blending tokens collected in the current epoch. `None` when not a core
    /// node.
    pub tokens_collected_in_current_epoch: Option<usize>,
    /// `None` when not a core node.
    pub session_peers: Option<SessionPeers>,
}

impl BlendStatus {
    fn new(status: status::BlendStatus, core_status: Option<CoreStatus>) -> Self {
        Self {
            is_core: core_status.is_some(),
            declaration_state: status.declaration_state.into(),
            current_epoch: status.current_epoch,
            declaration: status.declaration.map(Into::into),
            activity: status.activity.map(Into::into),
            tokens_collected_in_current_epoch: core_status
                .map(|core_status| core_status.tokens_collected_in_current_epoch),
            session_peers: core_status.map(|core_status| core_status.session_peers.into()),
        }
    }
}

/// Gets this node's Blend status: core or edge, its declaration and its
/// activity proof for the activity epoch.
///
/// # Errors
///
/// Fails right away if the chain is not `Online`.
pub async fn blend_status<
    MempoolAdapter,
    WalletAdapter,
    ChainService,
    StateStorage,
    BlendService,
    RuntimeServiceId,
>(
    handle: &OverwatchHandle<RuntimeServiceId>,
) -> Result<BlendStatus, DynError>
where
    MempoolAdapter: SdpMempoolAdapter + Send + Sync + 'static,
    ChainService: CryptarchiaServiceData<Tx: Send> + Send + Sync + 'static,
    StateStorage: SdpStateStorage<RuntimeServiceId>,
    BlendService: ServiceData<Message = ProxyServiceMessage<ServiceMessage<PeerId>>>,
    RuntimeServiceId: Send
        + Sync
        + Debug
        + Display
        + 'static
        + AsServiceId<ChainService>
        + AsServiceId<BlendService>
        + AsServiceId<
            SdpService<MempoolAdapter, WalletAdapter, ChainService, StateStorage, RuntimeServiceId>,
        >,
{
    let chain_api = CryptarchiaServiceApi::<ChainService>::from_overwatch_handle(handle).await;
    if chain_api.info().await?.cryptarchia_info.state != State::Online {
        return Err(DynError::from("The node is not online yet."));
    }

    let sdp_api = SdpServiceApi::<
        SdpService<MempoolAdapter, WalletAdapter, ChainService, StateStorage, RuntimeServiceId>,
    >::from_overwatch_handle(handle)
    .await;
    let status = sdp_api.get_blend_status().await?;

    let core_status = blend_core_status::<BlendService, RuntimeServiceId>(handle).await?;

    Ok(BlendStatus::new(status, core_status))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never_locked(_: u8) -> bool {
        false
    }

    #[test]
    fn picks_smallest_note_that_meets_threshold() {
        let notes = [(1u8, 1000), (2, 3000), (3, 10000), (4, 5050)];
        assert_eq!(select_service_note(&notes, never_locked, 5000), Ok(4));
    }

    #[test]
    fn exact_threshold_qualifies() {
        let notes = [(1u8, 5000)];
        assert_eq!(select_service_note(&notes, never_locked, 5000), Ok(1));
    }

    #[test]
    fn total_sufficient_but_no_single_note() {
        let notes = [(1u8, 1000), (2, 2000), (3, 3000)];
        assert_eq!(
            select_service_note(&notes, never_locked, 5000),
            Err(ServiceNoteError::NoSingleNoteMeetsThreshold {
                largest: 3000,
                total: 6000,
                threshold: 5000,
            })
        );
    }

    #[test]
    fn total_insufficient() {
        let notes = [(1u8, 1000), (2, 2000)];
        assert_eq!(
            select_service_note(&notes, never_locked, 5000),
            Err(ServiceNoteError::InsufficientTotal {
                total: 3000,
                threshold: 5000,
            })
        );
    }

    #[test]
    fn locked_notes_are_skipped() {
        let notes = [(1u8, 10000), (2, 6000)];
        assert_eq!(select_service_note(&notes, |id| id == 1, 5000), Ok(2));
    }

    #[test]
    fn all_locked() {
        let notes = [(1u8, 10000)];
        assert_eq!(
            select_service_note(&notes, |_| true, 5000),
            Err(ServiceNoteError::AllLocked { locked: 1 })
        );
    }

    #[test]
    fn no_notes() {
        let notes: [(u8, Value); 0] = [];
        assert_eq!(
            select_service_note(&notes, never_locked, 5000),
            Err(ServiceNoteError::NoNotes)
        );
    }
}
