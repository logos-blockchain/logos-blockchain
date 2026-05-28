pub mod backends;
mod handlers;
pub(crate) mod service_components;
pub mod settings;
#[cfg(test)]
mod tests;

use std::{
    fmt::{Debug, Display},
    hash::Hash,
    marker::PhantomData,
    time::Duration,
};

use backends::BlendBackend;
use futures::{Stream, StreamExt as _};
use lb_blend::{
    message::crypto::proofs::PoQVerificationInputsMinusSigningKey,
    proofs::quota::inputs::prove::public::{CoreInputs, LeaderInputs},
    scheduling::{
        epoch::{EpochEvent, UninitializedEpochEventStream},
        message_blend::provers::leader::LeaderProofsGenerator,
    },
};
use lb_chain_service::api::CryptarchiaServiceData;
use lb_core::codec::SerializeOp as _;
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
use serde::{Serialize, de::DeserializeOwned};
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
    message::{NetworkInfo, NetworkMessage, ServiceMessage},
};

const LOG_TARGET: &str = blend::service::EDGE;

type RunningSettings<Backend, NodeId, RuntimeServiceId> =
    RunningBlendConfig<<Backend as BlendBackend<NodeId, RuntimeServiceId>>::Settings>;

pub struct BlendService<
    Backend,
    NodeId,
    BroadcastSettings,
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

impl<
    Backend,
    NodeId,
    BroadcastSettings,
    ProofsGenerator,
    TimeBackend,
    ChainService,
    PolInfoProvider,
    RuntimeServiceId,
> ServiceData
    for BlendService<
        Backend,
        NodeId,
        BroadcastSettings,
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
    type Message = ServiceMessage<BroadcastSettings, NodeId>;
}

#[async_trait::async_trait]
impl<
    Backend,
    NodeId,
    BroadcastSettings,
    ProofsGenerator,
    TimeBackend,
    ChainService,
    PolInfoProvider,
    RuntimeServiceId,
> ServiceCore<RuntimeServiceId>
    for BlendService<
        Backend,
        NodeId,
        BroadcastSettings,
        ProofsGenerator,
        TimeBackend,
        ChainService,
        PolInfoProvider,
        RuntimeServiceId,
    >
where
    Backend: BlendBackend<NodeId, RuntimeServiceId> + Send + Sync,
    NodeId: Clone + Debug + Eq + Hash + Send + Sync + node_id::TryFrom + 'static,
    BroadcastSettings: Serialize + DeserializeOwned + Send,
    ProofsGenerator: LeaderProofsGenerator + Send,
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

        let messages_to_blend_stream = Box::pin(inbound_relay.filter_map(async |msg| {
            match msg {
                ServiceMessage::Blend(message) => Some(
                    NetworkMessage::<BroadcastSettings>::to_bytes(&message)
                        .expect("NetworkMessage should be able to be serialized")
                        .to_vec(),
                ),
                ServiceMessage::GetNetworkInfo { reply } => {
                    drop(reply.send(Some(NetworkInfo {
                        node_id: local_node_id.clone(),
                        core_info: None,
                    })));
                    None
                }
            }
        }));

        run::<Backend, _, ProofsGenerator, PolInfoProvider, _>(
            UninitializedEpochEventStream::new(
                public_epoch_stream,
                settings.time.epoch_transition_period,
            ),
            messages_to_blend_stream,
            RunningSettings::<Backend, _, _> {
                backend: settings.backend,
                cover: settings.cover,
                non_ephemeral_signing_key,
                num_blend_layers: settings.num_blend_layers,
                minimum_network_size: settings.minimum_network_size,
                time: settings.time,
                data_replication_factor: settings.data_replication_factor,
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
    mut incoming_message_stream: impl Stream<Item = Vec<u8>> + Send + Unpin,
    settings: RunningSettings<Backend, NodeId, RuntimeServiceId>,
    overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    notify_ready: impl Fn(),
) -> Result<(), Error>
where
    Backend: BlendBackend<NodeId, RuntimeServiceId> + Sync + Send,
    NodeId: Clone + Debug + Eq + Hash + Send + Sync + 'static,
    ProofsGenerator: LeaderProofsGenerator + Send,
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
    let mut current_epoch_message_handler: Option<
        MessageHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId>,
    > = None;

    loop {
        tokio::select! {
            Some(EpochEvent::NewEpoch(new_public_epoch_info)) = remaining_public_epoch_stream.next() => {
                current_epoch_info = new_public_epoch_info;
                match handle_new_epoch_event(&current_epoch_info, current_secret_epoch_info.as_ref(), &mut current_epoch_message_handler, settings.clone(), overwatch_handle.clone()) {
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
                match handle_new_epoch_event(&current_epoch_info, current_secret_epoch_info.as_ref(), &mut current_epoch_message_handler, settings.clone(), overwatch_handle.clone()) {
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
            Some(message) = incoming_message_stream.next() => {
                // TODO: Investigate why secret PoL info at times arrives after the block proposal.
                let Some(handler) = current_epoch_message_handler.as_mut() else {
                    tracing::warn!(target: LOG_TARGET, "Received a message to blend, but no active message handler is available to process it because the secret PoL info for the current epoch is not yet available. Ignoring the message.");
                    continue;
                };
                let message_copies = settings.data_replication_factor.checked_add(1).unwrap();
                for _ in 0..message_copies {
                    handler.handle_message_to_blend(message.clone()).await;
                }
            }
        }
    }
}

fn handle_new_epoch_event<Backend, NodeId, ProofsGenerator, RuntimeServiceId>(
    current_public_epoch_info: &BlendEpochState<NodeId>,
    current_secret_epoch_info: Option<&PolEpochInfo>,
    current_epoch_message_handler: &mut Option<
        MessageHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId>,
    >,
    settings: RunningSettings<Backend, NodeId, RuntimeServiceId>,
    overwatch_handle: OverwatchHandle<RuntimeServiceId>,
) -> Result<(), Error>
where
    Backend: BlendBackend<NodeId, RuntimeServiceId>,
    NodeId: Clone + Send + Eq + Hash + 'static,
    ProofsGenerator: LeaderProofsGenerator,
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

    let Some(current_secret_epoch_info) = current_secret_epoch_info else {
        assert!(
            current_epoch_message_handler.is_none(),
            "If there is no secret PoL info, there should not be an active message handler."
        );
        debug!(target: LOG_TARGET, "No secret PoL info available for the new epoch, cannot create message handler until it arrives.");
        return Ok(());
    };

    if current_public_epoch_info.epoch != current_secret_epoch_info.epoch {
        debug!(target: LOG_TARGET, "Public and secret epoch info on different epochs (public: {:?}, secret: {:?}). Cannot create new handler.", current_public_epoch_info.epoch, current_secret_epoch_info.epoch);
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
    };

    debug!(target: LOG_TARGET, "Creating new handler for epoch {:?}", current_public_epoch_info.epoch);
    let new_handler = MessageHandler::try_new_with_edge_condition_check(
        settings,
        current_public_epoch_info.membership_info.membership.clone(),
        new_public_inputs,
        current_secret_epoch_info.poq_private_inputs.clone(),
        overwatch_handle,
        current_public_epoch_info.epoch,
    )?;

    *current_epoch_message_handler = Some(new_handler);
    Ok(())
}
