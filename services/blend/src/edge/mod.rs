pub mod backends;
mod current_epoch;
mod handlers;
pub mod service_components;
pub mod settings;
#[cfg(test)]
mod tests;

use core::num::NonZeroU64;
use std::{
    fmt::{Debug, Display},
    hash::Hash,
};

use backends::BlendBackend;
use futures::{Stream, StreamExt as _};
use lb_blend::scheduling::{
    epoch::{EpochEvent, UninitializedEpochEventStream},
    message_blend::provers::{leader_and_pow::LeaderAndPowProofsGenerator, pow::new_mining_pool},
};
use lb_chain_service::api::CryptarchiaServiceData;
use lb_key_management_system_service::{
    api::KmsServiceApi, keys::KeyOperators,
    operators::ed25519::exfiltrate_secret_key::LeakSecretKeyOperator,
};
use lb_log_targets::blend;
use lb_time_service::TimeService;
use overwatch::{overwatch::OverwatchHandle, services::AsServiceId};
use settings::StartingBlendConfig;
use tokio::sync::oneshot;
use tracing::{debug, error, info};

use crate::{
    edge::{
        current_epoch::CurrentEpoch,
        handlers::Error,
        service_components::{Components, EdgeBackendSettingsOf},
        settings::RunningBlendConfig,
    },
    epoch_info::{PolEpochInfo, PolInfoProvider as PolInfoProviderTrait},
    kms::PreloadKmsService,
    membership::{self, chain::BlendEpochState, node_id},
    message::{BlendPayload, NetworkInfo, ServiceMessage},
    pending::{EncapsulationResult, LocalEncapsulation, MessageKind, PendingTransactions},
    service_components::Components as CommonComponents,
};

const LOG_TARGET: &str = blend::service::EDGE;

type RunningSettings<Backend, NodeId, RuntimeServiceId> =
    RunningBlendConfig<<Backend as BlendBackend<NodeId, RuntimeServiceId>>::Settings>;

type CurrentEpochOf<EdgeMode, RuntimeServiceId> = CurrentEpoch<
    <EdgeMode as Components<RuntimeServiceId>>::Backend,
    <EdgeMode as CommonComponents<RuntimeServiceId>>::NodeId,
    <EdgeMode as Components<RuntimeServiceId>>::ProofsGenerator,
    RuntimeServiceId,
>;

/// Runs the node for as long as it is an edge node.
pub(crate) async fn run_edge_mode<EdgeService, RuntimeServiceId>(
    inbound_relay: &mut (impl Stream<Item = ServiceMessage<EdgeService::NodeId>> + Send + Unpin),
    overwatch_handle: OverwatchHandle<RuntimeServiceId>,
    settings: StartingBlendConfig<EdgeBackendSettingsOf<EdgeService, RuntimeServiceId>>,
    notify_ready: impl Fn(),
) -> Result<(), overwatch::DynError>
where
    EdgeService: Components<
            RuntimeServiceId,
            NodeId: Clone + Debug + Eq + Hash + Send + Sync + node_id::TryFrom + 'static,
            Backend: BlendBackend<
                <EdgeService as Components<RuntimeServiceId>>::NodeId,
                RuntimeServiceId,
            > + Send
                         + Sync,
            ProofsGenerator: LeaderAndPowProofsGenerator + Send,
            TimeBackend: lb_time_service::backends::TimeBackend + Send,
            ChainService: CryptarchiaServiceData<Tx: Send + Sync>,
            PolInfoProvider: PolInfoProviderTrait<
                RuntimeServiceId,
                Stream: Send + Unpin + 'static,
            > + Send,
        >,
    RuntimeServiceId: AsServiceId<TimeService<EdgeService::TimeBackend, RuntimeServiceId>>
        + AsServiceId<EdgeService::ChainService>
        + AsServiceId<PreloadKmsService<RuntimeServiceId>>
        + Display
        + Debug
        + Clone
        + Send
        + Sync
        + Unpin
        + 'static,
{
    // No readiness wait: the supervisor did one for every service any mode
    // needs, before the first mode started.
    let kms = KmsServiceApi::<PreloadKmsService<_>, RuntimeServiceId>::new(
        overwatch_handle.relay::<PreloadKmsService<_>>().await?,
    );

    // TODO: This will go once we do not need to pass the secret key anymore, i.e.,
    // when we have libp2p integration with KMS.
    let non_ephemeral_signing_key = {
        let (sender, receiver) = oneshot::channel();
        kms.execute(
            settings.non_ephemeral_signing_key_id,
            KeyOperators::Ed25519(Box::new(LeakSecretKeyOperator::new(sender))),
        )
        .await
        .expect("Failed to interact with KMS to fetch non-ephemeral signing key.");
        receiver
            .await
            .expect("Failed to retrieve non-ephemeral signing key from KMS.")
    };
    let local_node_id = <EdgeService::NodeId as node_id::TryFrom>::try_from_provider_id(
        &non_ephemeral_signing_key.public_key().to_bytes(),
    )
    .expect("non-ephemeral signing key should decode into a valid node id");

    let public_epoch_stream = membership::chain::subscribe::<
        EdgeService::ChainService,
        EdgeService::NodeId,
        EdgeService::TimeBackend,
        RuntimeServiceId,
    >(
        &overwatch_handle,
        non_ephemeral_signing_key.public_key(),
        // No ZK stuff needs to be computed by edge nodes, so no ZK key is specified here.
        None,
    )
    .await;

    run::<EdgeService, _>(
        UninitializedEpochEventStream::new(
            public_epoch_stream,
            settings.time.epoch_transition_period,
        ),
        inbound_relay,
        local_node_id,
        RunningSettings::<<EdgeService as Components<RuntimeServiceId>>::Backend, _, _> {
            backend: settings.backend,
            cover: settings.cover,
            non_ephemeral_signing_key,
            num_blend_layers: settings.num_blend_layers,
            minimum_network_size: settings.minimum_network_size,
            time: settings.time,
            data_replication_factor: settings.data_replication_factor,
            pow_mining_pool: new_mining_pool(),
        },
        &overwatch_handle,
        notify_ready,
    )
    .await
    .map_err(|e| {
        error!(target: LOG_TARGET, "Edge blend service is being terminated with error: {e:?}");
        e.into()
    })
}

/// Run the event loop of the service.
///
/// The event loop keeps track of the latest public epoch info and the latest
/// secret `PoL` info independently and rebuilds the message handler whenever
/// the two line up on the same epoch. It handles three types of events:
/// - **New public epoch info** (chain-derived membership + leader inputs):
///   becomes the current public info; the handler is rebuilt if the latest
///   secret info is for the same epoch, otherwise it stays down.
/// - **New secret `PoL` info**: becomes the current secret info; the handler is
///   rebuilt if it matches the current public info's epoch.
/// - **Incoming messages to blend**: forwarded to the current message handler;
///   dropped with a warning if no handler is active (secret `PoL` info for the
///   current epoch has not yet arrived).
///
/// Returns an [`Error`] if a new membership does not satisfy the edge node
/// condition.
///
/// # Panics
/// - If the initial public epoch info is not yielded immediately from the
///   public epoch stream.
#[expect(
    clippy::cognitive_complexity,
    reason = "TODO: address this in a dedicated refactor"
)]
async fn run<EdgeMode, RuntimeServiceId>(
    public_epoch_stream: UninitializedEpochEventStream<
        impl Stream<Item = BlendEpochState<EdgeMode::NodeId>> + Unpin,
    >,
    inbound_relay: &mut (impl Stream<Item = ServiceMessage<EdgeMode::NodeId>> + Send + Unpin),
    local_node_id: EdgeMode::NodeId,
    settings: RunningSettings<
        <EdgeMode as Components<RuntimeServiceId>>::Backend,
        <EdgeMode as CommonComponents<RuntimeServiceId>>::NodeId,
        RuntimeServiceId,
    >,
    overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    notify_ready: impl Fn(),
) -> Result<(), Error>
where
    EdgeMode: Components<
            RuntimeServiceId,
            NodeId: Clone + Debug + Eq + Hash + Send + Sync + 'static,
            Backend: BlendBackend<
                <EdgeMode as CommonComponents<RuntimeServiceId>>::NodeId,
                RuntimeServiceId,
            > + Sync
                         + Send,
            ProofsGenerator: LeaderAndPowProofsGenerator + Send,
            PolInfoProvider: PolInfoProviderTrait<RuntimeServiceId, Stream: Unpin>,
        >,
    RuntimeServiceId: Clone + Send + Sync,
{
    let (current_epoch_info, mut remaining_public_epoch_stream) = public_epoch_stream
        .await_first_ready()
        .await
        .expect("The current epoch info must be available.");

    info!(
        target: LOG_TARGET,
        members = current_epoch_info.membership_info.membership.size(),
        local_node_index = current_epoch_info.membership_info.membership.local_index(),
        has_zk = current_epoch_info.membership_info.zk.is_some(),
        "current membership is ready"
    );

    notify_ready();

    // No need to wait for the PoL stream to return an element. We just move on and
    // will have a `None` handler until secret info for an epoch is passed to this
    // service.
    let mut secret_pol_info_stream = EdgeMode::PolInfoProvider::subscribe(overwatch_handle)
        .await
        .expect("Should not fail to subscribe to secret PoL info stream.");

    let mut current_secret_epoch_info: Option<PolEpochInfo> = None;
    // The epoch owns its proposals, so a new one takes them with it; a
    // transaction is not slot-bound and outlives every epoch it waits through.
    let mut current_epoch: CurrentEpochOf<EdgeMode, RuntimeServiceId> = match CurrentEpoch::try_new(
        current_epoch_info,
        &settings,
    ) {
        Err(Error::NetworkIsTooSmall(_)) => {
            info!(target: LOG_TARGET, "Initial membership does not satisfy edge node condition, edge service shutting down.");
            return Ok(());
        }
        Err(e) => {
            error!(target: LOG_TARGET, "Error with the initial epoch: {e:?}, edge service shutting down.");
            return Err(e);
        }
        Ok(epoch) => epoch,
    };
    let mut pending_transactions = PendingTransactions::new();

    loop {
        tokio::select! {
            Some(EpochEvent::NewEpoch(new_public_epoch_info)) = remaining_public_epoch_stream.next() => {
                match CurrentEpoch::try_new(new_public_epoch_info, &settings) {
                    Err(Error::NetworkIsTooSmall(_)) => {
                        info!(target: LOG_TARGET, "New membership does not satisfy edge node condition, edge service shutting down.");
                        return Ok(());
                    }
                    Err(e) => {
                        error!(target: LOG_TARGET, "Error when handling new public epoch: {e:?}, edge service shutting down.");
                        return Err(e);
                    }
                    // The epoch this replaces takes its queued proposals with
                    // it: they were built for slots it owned, and blending them
                    // under the new one would spend the quota its own block
                    // needs.
                    Ok(next) => current_epoch = next.with_available_secret_info(&mut current_secret_epoch_info, settings.clone(), overwatch_handle.clone()),
                }
            }
            Some(new_secret_pol_info) = secret_pol_info_stream.next() => {
                current_secret_epoch_info = Some(new_secret_pol_info);
                current_epoch = current_epoch.with_available_secret_info(&mut current_secret_epoch_info, settings.clone(), overwatch_handle.clone());
            }
            Some(message) = inbound_relay.next() => {
                match message {
                    ServiceMessage::Blend(BlendPayload::Transaction(transaction)) => {
                        pending_transactions.queue(transaction);
                    }
                    ServiceMessage::Blend(BlendPayload::BlockProposal(proposal)) => {
                        let proposal_copies = NonZeroU64::new(settings.data_replication_factor.checked_add(1).expect("Data replication factor should not overflow when incremented.")).expect("Number of block proposal copies cannot be zero by definition.");
                        current_epoch.queue_proposal(proposal, proposal_copies);
                    }
                    ServiceMessage::GetNetworkInfo { reply } => {
                        drop(reply.send(Some(NetworkInfo {
                            node_id: local_node_id.clone(),
                            core_info: None,
                        })));
                    }
                    ServiceMessage::GetPendingTransactions { reply } => {
                        drop(reply.send(pending_transactions.iter().cloned().collect()));
                    }
                }
            }
            // A queued transaction leaves as soon as a `PoW` solution backs it, awaited
            // here as one branch among the others so the loop keeps turning meanwhile.
            // Both kinds share one branch because both need the handler mutably,
            // and `select!` builds every branch future before any of them wins.
            Some(encapsulation_result) = current_epoch.encapsulate_next_local_message(&pending_transactions) => {
                match encapsulation_result {
                    EncapsulationResult::Complete(encapsulation) => {
                        let LocalEncapsulation { message, kind } = *encapsulation;
                        current_epoch.send(message).await;
                        match kind {
                            MessageKind::Proposal => current_epoch.proposals_mut().mark_copy_as_sent(),
                            MessageKind::Transaction => drop(pending_transactions.mark_as_sent()),
                        }
                    }
                    // The head of whichever queue it came from can never be
                    // encapsulated, so it goes rather than blocking the rest.
                    EncapsulationResult::Discard(MessageKind::Proposal) => current_epoch.proposals_mut().discard_head(),
                    EncapsulationResult::Discard(MessageKind::Transaction) => drop(pending_transactions.discard_head()),
                    EncapsulationResult::Retry => unreachable!("`Retry` is never returned by `encapsulate_next_local_message`"),
                }
            }
            else => {
                // All input streams have terminated (e.g. disorderly shutdown).
                // Exit cleanly instead of letting `select!` panic.
                debug!(target: LOG_TARGET, "All input streams terminated, edge service shutting down.");
                return Ok(());
            }
        }
    }
}
