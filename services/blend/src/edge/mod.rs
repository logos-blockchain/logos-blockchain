pub mod backends;
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
use lb_blend::{
    message::{
        crypto::proofs::PoQVerificationInputsMinusSigningKey,
        encap::validated::EncapsulatedMessageWithVerifiedPublicHeader,
    },
    proofs::quota::inputs::prove::public::{CoreInputs, LeaderInputs, PowInputs},
    scheduling::{
        epoch::{EpochEvent, UninitializedEpochEventStream},
        message_blend::provers::{
            leader_and_pow::LeaderAndPowProofsGenerator, pow::new_mining_pool,
        },
    },
};
use lb_chain_service::api::CryptarchiaServiceData;
use lb_key_management_system_service::{
    api::KmsServiceApi, keys::KeyOperators,
    operators::ed25519::exfiltrate_secret_key::LeakSecretKeyOperator,
};
use lb_log_targets::blend;
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
    edge::{
        handlers::{Error, MessageHandler},
        settings::RunningBlendConfig,
    },
    epoch_info::{PolEpochInfo, PolInfoProvider as PolInfoProviderTrait},
    kms::PreloadKmsService,
    membership::{self, chain::BlendEpochState, node_id},
    message::{BlendPayload, NetworkInfo, ServiceMessage},
    pending::{LocalEncapsulation, NextLocalMessage, PendingLocalMessages, resolve_encapsulation},
};

const LOG_TARGET: &str = blend::service::EDGE;

type RunningSettings<Backend, NodeId, RuntimeServiceId> =
    RunningBlendConfig<<Backend as BlendBackend<NodeId, RuntimeServiceId>>::Settings>;

pub struct BlendService<
    Backend,
    NodeId,
    ProofsGenerator,
    TimeBackend,
    ChainService,
    PolInfoProvider,
    RuntimeServiceId,
> where
    Backend: BlendBackend<NodeId, RuntimeServiceId>,
    NodeId: Clone,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    _phantom: PhantomData<(ProofsGenerator, TimeBackend, ChainService, PolInfoProvider)>,
}

impl<Backend, NodeId, ProofsGenerator, TimeBackend, ChainService, PolInfoProvider, RuntimeServiceId>
    ServiceData
    for BlendService<
        Backend,
        NodeId,
        ProofsGenerator,
        TimeBackend,
        ChainService,
        PolInfoProvider,
        RuntimeServiceId,
    >
where
    Backend: BlendBackend<NodeId, RuntimeServiceId>,
    NodeId: Clone,
{
    type Settings = StartingBlendConfig<Backend::Settings>;
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = ServiceMessage<NodeId>;
}

#[async_trait::async_trait]
impl<Backend, NodeId, ProofsGenerator, TimeBackend, ChainService, PolInfoProvider, RuntimeServiceId>
    ServiceCore<RuntimeServiceId>
    for BlendService<
        Backend,
        NodeId,
        ProofsGenerator,
        TimeBackend,
        ChainService,
        PolInfoProvider,
        RuntimeServiceId,
    >
where
    Backend: BlendBackend<NodeId, RuntimeServiceId> + Send + Sync,
    NodeId: Clone + Debug + Eq + Hash + Send + Sync + node_id::TryFrom + 'static,
    ProofsGenerator: LeaderAndPowProofsGenerator + Send,
    TimeBackend: lb_time_service::backends::TimeBackend + Send,
    ChainService: CryptarchiaServiceData<Tx: Send + Sync>,
    PolInfoProvider: PolInfoProviderTrait<RuntimeServiceId, Stream: Send + Unpin + 'static> + Send,
    RuntimeServiceId: AsServiceId<Self>
        + AsServiceId<TimeService<TimeBackend, RuntimeServiceId>>
        + AsServiceId<ChainService>
        + AsServiceId<PreloadKmsService<RuntimeServiceId>>
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
            PreloadKmsService<_>
        )
        .await?;

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
            )
            .await;

        run::<Backend, _, ProofsGenerator, PolInfoProvider, _>(
            UninitializedEpochEventStream::new(
                public_epoch_stream,
                settings.time.epoch_transition_period,
            ),
            Box::pin(inbound_relay),
            local_node_id,
            RunningSettings::<Backend, _, _> {
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
    reason = "TODO: address this in a dedicated refactor"
)]
async fn run<Backend, NodeId, ProofsGenerator, PolInfoProvider, RuntimeServiceId>(
    public_epoch_stream: UninitializedEpochEventStream<
        impl Stream<Item = BlendEpochState<NodeId>> + Unpin,
    >,
    mut inbound_relay: impl Stream<Item = ServiceMessage<NodeId>> + Send + Unpin,
    local_node_id: NodeId,
    settings: RunningSettings<Backend, NodeId, RuntimeServiceId>,
    overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    notify_ready: impl Fn(),
) -> Result<(), Error>
where
    Backend: BlendBackend<NodeId, RuntimeServiceId> + Sync + Send,
    NodeId: Clone + Debug + Eq + Hash + Send + Sync + 'static,
    ProofsGenerator: LeaderAndPowProofsGenerator + Send,
    PolInfoProvider: PolInfoProviderTrait<RuntimeServiceId, Stream: Unpin>,
    RuntimeServiceId: Clone + Send + Sync,
{
    let (mut current_epoch_info, mut remaining_public_epoch_stream) = public_epoch_stream
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
    // Transactions waiting for a `PoW` solution to back their layer proofs.
    let mut pending_messages = PendingLocalMessages::new();
    let mut current_epoch_message_handler: Option<
        MessageHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId>,
    > = None;

    loop {
        tokio::select! {
            Some(EpochEvent::NewEpoch(new_public_epoch_info)) = remaining_public_epoch_stream.next() => {
                current_epoch_info = new_public_epoch_info;
                match handle_new_epoch_event(&current_epoch_info, &mut current_secret_epoch_info, &mut current_epoch_message_handler, settings.clone(), overwatch_handle.clone()) {
                    Err(Error::NetworkIsTooSmall(_)) => {
                        info!(target: LOG_TARGET, "New membership does not satisfy edge node condition, edge service shutting down.");
                        return Ok(());
                    }
                    Err(e) => {
                        error!(target: LOG_TARGET, "Error when handling new public epoch: {e:?}, edge service shutting down.");
                        return Err(e);
                    }
                    Ok(()) => {}
                }
            }
            Some(new_secret_pol_info) = secret_pol_info_stream.next() => {
                current_secret_epoch_info = Some(new_secret_pol_info);
                match handle_new_epoch_event(&current_epoch_info, &mut current_secret_epoch_info, &mut current_epoch_message_handler, settings.clone(), overwatch_handle.clone()) {
                    Err(Error::NetworkIsTooSmall(_)) => {
                        info!(target: LOG_TARGET, "New membership does not satisfy edge node condition, edge service shutting down.");
                        return Ok(());
                    }
                    Err(e) => {
                        error!(target: LOG_TARGET, "Error when handling new secret epoch: {e:?}, edge service shutting down.");
                        return Err(e);
                    }
                    Ok(()) => {}
                }
            }
            Some(message) = inbound_relay.next() => {
                match message {
                    ServiceMessage::Blend(BlendPayload::Transaction(transaction)) => {
                        pending_messages.queue_transaction(transaction);
                    }
                    ServiceMessage::Blend(BlendPayload::BlockProposal(proposal)) => {
                        let proposal_copies = NonZeroU64::new(settings.data_replication_factor.checked_add(1).expect("Data replication factor should not overflow when incremented.")).expect("Number of block proposal copies cannot be zero by definition.");
                        pending_messages.queue_proposal(proposal, proposal_copies);
                    }
                    ServiceMessage::GetNetworkInfo { reply } => {
                        drop(reply.send(Some(NetworkInfo {
                            node_id: local_node_id.clone(),
                            core_info: None,
                        })));
                    }
                    ServiceMessage::GetPendingTransactions { reply } => {
                        drop(reply.send(pending_messages.transactions().cloned().collect()));
                    }
                }
            }
            // A queued transaction leaves as soon as a `PoW` solution backs it, awaited
            // here as one branch among the others so the loop keeps turning meanwhile.
            // Both kinds share one branch because both need the handler mutably,
            // and `select!` builds every branch future before any of them wins.
            Some(encapsulation_result) = encapsulate_next_local_message(&pending_messages, &mut current_epoch_message_handler) => {
                let current_epoch_message_handler = current_epoch_message_handler.as_mut().expect("Message handler must exist for a message that was just encapsulated.");
                match encapsulation_result {
                    Ok(LocalEncapsulation::ProposalCopy(message)) => {
                        current_epoch_message_handler.send(message).await;
                        pending_messages.mark_proposal_copy_as_sent();
                    }
                    Ok(LocalEncapsulation::Transaction(message)) => {
                        current_epoch_message_handler.send(message).await;
                        drop(pending_messages.mark_transaction_as_sent());
                    }
                    Err(()) => drop(pending_messages.discard_head()),
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

/// Encapsulates one locally-originated message, once a handler exists to make
/// it and proofs back it.
///
/// Proposals go first: one is tied to the slot it was built for and goes stale,
/// whereas a transaction keeps.
///
/// Neither queue is popped here either, for the same reason: one that popped
/// before awaiting would take the message down with it every time this future
/// was dropped. The caller updates the queues once the race is settled, which
/// is also why only one copy of a proposal is wrapped per call.
///
/// Returns `None` when there is nothing to hand back — nothing queued, no
/// handler for this epoch yet, or no proofs — which is what leaves the branch
/// free to wait on the others.
async fn encapsulate_next_local_message<Backend, NodeId, ProofsGenerator, RuntimeServiceId>(
    pending_messages: &PendingLocalMessages,
    current_epoch_message_handler: &mut Option<
        MessageHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId>,
    >,
) -> Option<Result<LocalEncapsulation, ()>>
where
    Backend: BlendBackend<NodeId, RuntimeServiceId> + Sync,
    NodeId: Clone + Debug + Eq + Hash + Send + Sync + 'static,
    ProofsGenerator: LeaderAndPowProofsGenerator + Send,
{
    type Wrap = fn(EncapsulatedMessageWithVerifiedPublicHeader) -> LocalEncapsulation;

    let handler = current_epoch_message_handler.as_mut()?;
    let (encapsulated, wrap_fn): (_, Wrap) = match pending_messages.next()? {
        NextLocalMessage::ProposalCopy(proposal) => (
            handler.encapsulate_block_proposal(proposal).await,
            LocalEncapsulation::ProposalCopy,
        ),
        NextLocalMessage::Transaction(transaction) => (
            handler.encapsulate_transaction(transaction).await,
            LocalEncapsulation::Transaction,
        ),
    };

    resolve_encapsulation(encapsulated, wrap_fn)
}

fn handle_new_epoch_event<Backend, NodeId, ProofsGenerator, RuntimeServiceId>(
    current_public_epoch_info: &BlendEpochState<NodeId>,
    maybe_current_secret_epoch_info: &mut Option<PolEpochInfo>,
    current_epoch_message_handler: &mut Option<
        MessageHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId>,
    >,
    settings: RunningSettings<Backend, NodeId, RuntimeServiceId>,
    overwatch_handle: OverwatchHandle<RuntimeServiceId>,
) -> Result<(), Error>
where
    Backend: BlendBackend<NodeId, RuntimeServiceId>,
    NodeId: Clone + Send + Eq + Hash + 'static,
    ProofsGenerator: LeaderAndPowProofsGenerator,
{
    // Whatever happens on a new epoch, we shut down the previous handler.
    // It will be rebuilt below if the current public and secret info line up
    // on the same epoch.
    drop(current_epoch_message_handler.take());

    let Some(zk_info) = &current_public_epoch_info.membership_info.zk else {
        return Err(Error::NetworkIsTooSmall(0));
    };

    // Validate the edge node condition up front so the service shuts down on
    // an invalid membership regardless of whether secret PoL info is available
    // yet for the current epoch.
    let membership_size = current_public_epoch_info.membership_info.membership.size();
    if membership_size < settings.minimum_network_size.get() as usize {
        return Err(Error::NetworkIsTooSmall(membership_size));
    }
    if current_public_epoch_info
        .membership_info
        .membership
        .contains_local()
    {
        return Err(Error::LocalIsCoreNode);
    }

    let Some(current_secret_epoch_info) = maybe_current_secret_epoch_info.take() else {
        assert!(
            current_epoch_message_handler.is_none(),
            "If there is no secret PoL info, there should not be an active message handler."
        );
        debug!(target: LOG_TARGET, "No secret PoL info available for the new epoch, cannot create message handler until it arrives.");
        return Ok(());
    };

    if current_secret_epoch_info.epoch != current_public_epoch_info.epoch {
        debug!(target: LOG_TARGET, "Secret PoL info is for epoch {:?} which does not match the current public epoch {:?}, cannot create message handler until they line up.", current_secret_epoch_info.epoch, current_public_epoch_info.epoch);
        // Re-instate the stream since we need it for when the new public epoch info
        // will arrive. We chose this over not calling `.take()` here and use
        // `.take().unwrap()` below instead.
        *maybe_current_secret_epoch_info = Some(current_secret_epoch_info);
        return Ok(());
    }

    let new_public_inputs = PoQVerificationInputsMinusSigningKey {
        core: CoreInputs {
            quota: settings.cover.epoch_core_quota(
                settings.num_blend_layers,
                &settings.time,
                current_public_epoch_info.membership_info.membership.size(),
            ),
            zk_root: zk_info.root,
        },
        leader: LeaderInputs {
            lottery_0: current_public_epoch_info.lottery_0,
            lottery_1: current_public_epoch_info.lottery_1,
            pol_epoch_nonce: current_public_epoch_info.nonce,
            pol_ledger_aged: current_public_epoch_info.aged,
            message_quota: settings.epoch_leadership_quota(),
        },
        pow: PowInputs {
            pow_blend_difficulty: current_public_epoch_info.pow_difficulty,
            pow_quota: settings.epoch_pow_quota(),
        },
    };

    debug!(target: LOG_TARGET, "Creating new handler for epoch {:?}", current_public_epoch_info.epoch);
    let new_handler = MessageHandler::try_new_with_edge_condition_check(
        settings,
        current_public_epoch_info.membership_info.membership.clone(),
        new_public_inputs,
        current_secret_epoch_info.winning_pol_info_stream,
        overwatch_handle,
        current_public_epoch_info.epoch,
    )?;

    *current_epoch_message_handler = Some(new_handler);
    Ok(())
}
