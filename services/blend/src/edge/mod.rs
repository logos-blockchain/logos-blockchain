pub mod backends;
mod current_epoch;
mod handlers;
pub(crate) mod service_components;
pub mod settings;
#[cfg(test)]
mod tests;

use core::num::NonZeroU64;
use std::{
    fmt::{Debug, Display},
    hash::Hash,
    marker::PhantomData,
    time::Duration,
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
use lb_network_service::NetworkService;
use lb_services_utils::wait_until_services_are_ready;
use lb_time_service::TimeService;
use overwatch::{
    OpaqueServiceResourcesHandle,
    overwatch::OverwatchHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        resources::ServiceResourcesHandle,
        state::{NoOperator, NoState},
    },
};
use settings::StartingBlendConfig;
use tokio::sync::oneshot;
use tracing::{debug, error, info};

use crate::{
    core::dispatcher::PayloadDispatcher,
    delivery::{DeliveryLogic, broadcast_undelivered_payloads},
    edge::{current_epoch::CurrentEpoch, handlers::Error, settings::RunningBlendConfig},
    epoch_info::{PolEpochInfo, PolInfoProvider as PolInfoProviderTrait},
    kms::PreloadKmsService,
    membership::{self, chain::BlendEpochState, node_id},
    message::{BlendPayload, NetworkInfo, ServiceMessage},
    pending::{EncapsulationResult, LocalEncapsulation, MessageKind, PendingTransactions},
};

const LOG_TARGET: &str = blend::service::EDGE;

type RunningSettings<Backend, NodeId, BroadcastSettings, RuntimeServiceId> = RunningBlendConfig<
    <Backend as BlendBackend<NodeId, RuntimeServiceId>>::Settings,
    BroadcastSettings,
>;

pub struct BlendService<
    Backend,
    NodeId,
    ProofsGenerator,
    Dispatcher,
    TimeBackend,
    ChainService,
    PolInfoProvider,
    RuntimeServiceId,
> where
    Backend: BlendBackend<NodeId, RuntimeServiceId>,
    NodeId: Clone,
    Dispatcher: PayloadDispatcher<RuntimeServiceId>,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    _phantom: PhantomData<(
        ProofsGenerator,
        Dispatcher,
        TimeBackend,
        ChainService,
        PolInfoProvider,
    )>,
}

impl<
    Backend,
    NodeId,
    ProofsGenerator,
    Dispatcher,
    TimeBackend,
    ChainService,
    PolInfoProvider,
    RuntimeServiceId,
> ServiceData
    for BlendService<
        Backend,
        NodeId,
        ProofsGenerator,
        Dispatcher,
        TimeBackend,
        ChainService,
        PolInfoProvider,
        RuntimeServiceId,
    >
where
    Backend: BlendBackend<NodeId, RuntimeServiceId>,
    NodeId: Clone,
    Dispatcher: PayloadDispatcher<RuntimeServiceId>,
{
    type Settings = StartingBlendConfig<Backend::Settings, Dispatcher::Settings>;
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = ServiceMessage<NodeId>;
}

#[async_trait::async_trait]
impl<
    Backend,
    NodeId,
    ProofsGenerator,
    Dispatcher,
    TimeBackend,
    ChainService,
    PolInfoProvider,
    RuntimeServiceId,
> ServiceCore<RuntimeServiceId>
    for BlendService<
        Backend,
        NodeId,
        ProofsGenerator,
        Dispatcher,
        TimeBackend,
        ChainService,
        PolInfoProvider,
        RuntimeServiceId,
    >
where
    Backend: BlendBackend<NodeId, RuntimeServiceId> + Send + Sync,
    NodeId: Clone + Debug + Eq + Hash + Send + Sync + node_id::TryFrom + 'static,
    ProofsGenerator: LeaderAndPowProofsGenerator + Send,
    Dispatcher: PayloadDispatcher<RuntimeServiceId> + Send + Sync,
    TimeBackend: lb_time_service::backends::TimeBackend + Send,
    ChainService: CryptarchiaServiceData<Tx: Send + Sync>,
    PolInfoProvider: PolInfoProviderTrait<RuntimeServiceId, Stream: Send + Unpin + 'static> + Send,
    RuntimeServiceId: AsServiceId<Self>
        + AsServiceId<TimeService<TimeBackend, RuntimeServiceId>>
        + AsServiceId<ChainService>
        + AsServiceId<PreloadKmsService<RuntimeServiceId>>
        + AsServiceId<NetworkService<Dispatcher::Backend, RuntimeServiceId>>
        + AsServiceId<Dispatcher::MempoolService>
        + Display
        + Debug
        + Clone
        + Send
        + Sync
        + Unpin
        + 'static,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        _initial_state: Self::State,
    ) -> Result<Self, overwatch::DynError> {
        Ok(Self {
            service_resources_handle,
            _phantom: PhantomData,
        })
    }

    async fn run(mut self) -> Result<(), overwatch::DynError> {
        let Self {
            service_resources_handle:
                ServiceResourcesHandle {
                    inbound_relay,
                    overwatch_handle,
                    settings_handle,
                    status_updater,
                    ..
                },
            ..
        } = self;

        let settings = settings_handle.notifier().get_updated_settings();

        wait_until_services_are_ready!(
            &overwatch_handle,
            Some(Duration::from_mins(1)),
            TimeService<_, _>,
            PreloadKmsService<_>,
            NetworkService<Dispatcher::Backend, _>
        )
        .await?;

        // An edge node holds no connections into the Blend network and sees none of
        // its traffic, so the broadcasting channel is the only thing it can watch —
        // and the only way it has of putting a payload the network dropped where the
        // chain will still see it.
        let payload_dispatcher = {
            let network_relay = overwatch_handle
                .relay::<NetworkService<Dispatcher::Backend, _>>()
                .await
                .expect("Relay with network service should be available.");
            let mempool_relay = overwatch_handle
                .relay::<Dispatcher::MempoolService>()
                .await
                .expect("Relay with mempool service should be available.");
            Dispatcher::new(network_relay, mempool_relay, settings.broadcast.clone())
        };

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
        let local_node_id =
            NodeId::try_from_provider_id(&non_ephemeral_signing_key.public_key().to_bytes())
                .expect("non-ephemeral signing key should decode into a valid node id");

        let public_epoch_stream =
            membership::chain::subscribe::<ChainService, NodeId, TimeBackend, RuntimeServiceId>(
                &overwatch_handle,
                non_ephemeral_signing_key.public_key(),
                // No ZK stuff needs to be computed by edge nodes, so no ZK key is specified here.
                None,
                "blend_edge_service",
            )
            .await;

        run::<Backend, _, ProofsGenerator, _, PolInfoProvider, _>(
            UninitializedEpochEventStream::new(
                public_epoch_stream,
                settings.time.epoch_transition_period,
            ),
            Box::pin(inbound_relay),
            local_node_id,
            RunningSettings::<Backend, _, Dispatcher::Settings, _> {
                backend: settings.backend,
                cover: settings.cover,
                non_ephemeral_signing_key,
                num_blend_layers: settings.num_blend_layers,
                minimum_network_size: settings.minimum_network_size,
                time: settings.time,
                data_replication_factor: settings.data_replication_factor,
                pow_mining_pool: new_mining_pool(),
                broadcast: settings.broadcast,
                blend_failure_fallback: settings.blend_failure_fallback,
                max_blend_delay_in_rounds: settings.max_blend_delay_in_rounds,
            },
            payload_dispatcher,
            &overwatch_handle,
            || {
                status_updater.notify_ready();
                info!(
                    target: LOG_TARGET,
                    "Service '{}' is ready.",
                    <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
                );
            },
        )
        .await
        .map_err(|e| {
            error!(target: LOG_TARGET, "Edge blend service is being terminated with error: {e:?}");
            e.into()
        })
    }
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
    clippy::too_many_lines,
    reason = "TODO: address this in a dedicated refactor"
)]
async fn run<Backend, NodeId, ProofsGenerator, Dispatcher, PolInfoProvider, RuntimeServiceId>(
    public_epoch_stream: UninitializedEpochEventStream<
        impl Stream<Item = BlendEpochState<NodeId>> + Unpin,
    >,
    mut inbound_relay: impl Stream<Item = ServiceMessage<NodeId>> + Send + Unpin,
    local_node_id: NodeId,
    settings: RunningSettings<Backend, NodeId, Dispatcher::Settings, RuntimeServiceId>,
    payload_dispatcher: Dispatcher,
    overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    notify_ready: impl Fn(),
) -> Result<(), Error>
where
    Backend: BlendBackend<NodeId, RuntimeServiceId> + Sync + Send,
    NodeId: Clone + Debug + Eq + Hash + Send + Sync + 'static,
    ProofsGenerator: LeaderAndPowProofsGenerator + Send,
    Dispatcher: PayloadDispatcher<RuntimeServiceId> + Sync,
    PolInfoProvider: PolInfoProviderTrait<RuntimeServiceId, Stream: Unpin>,
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
    let mut secret_pol_info_stream = PolInfoProvider::subscribe(overwatch_handle)
        .await
        .expect("Should not fail to subscribe to secret PoL info stream.");

    let mut current_secret_epoch_info: Option<PolEpochInfo> = None;
    // The epoch owns its proposals, so a new one takes them with it; a
    // transaction is not slot-bound and outlives every epoch it waits through.
    let mut current_epoch: CurrentEpoch<Backend, NodeId, ProofsGenerator, RuntimeServiceId> =
        match CurrentEpoch::try_new(current_epoch_info, &settings) {
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

    // What this node has handed to the Blend network and not seen come out of it.
    // An edge node reaches the same verdict as a core node and reacts the same way,
    // only later: it sees none of the network's traffic, so the deadline is all it
    // has.
    let mut deliveries = if settings.blend_failure_fallback {
        DeliveryLogic::watching(
            settings.max_data_message_delay_in_rounds(),
            settings.time.round_duration,
            payload_dispatcher.observe_broadcasts().await,
        )
    } else {
        DeliveryLogic::blended()
    };

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
            Some(undelivered) = deliveries.next() => {
                broadcast_undelivered_payloads(undelivered.into_iter(), &payload_dispatcher).await;
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
                        // An edge node releases what it encapsulates, there and then, so
                        // the payload it carries is claimed from the queue that still
                        // holds it and the deadline starts on the spot.
                        let payload = match kind {
                            MessageKind::Proposal => current_epoch.proposals_mut().head().map(|proposal| BlendPayload::BlockProposal(proposal.to_vec())),
                            MessageKind::Transaction => pending_transactions.head().map(|transaction| BlendPayload::Transaction(transaction.to_vec())),
                        }
                        .expect("A message was encapsulated, so the payload it carries is queued.");
                        current_epoch.send(message).await;
                        deliveries.mark_payload_as_released(payload);
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
