use core::{
    fmt::{Debug, Display},
    future::ready,
};

use async_trait::async_trait;
use futures::{Stream, StreamExt as _};
use lb_blend::{
    proofs::quota::inputs::prove::private::ProofOfLeadershipQuotaInputs,
    scheduling::message_blend::provers::{
        core_and_leader::RealCoreAndLeaderProofsGenerator, leader::RealLeaderProofsGenerator,
    },
};
use lb_blend_service::{
    RealProofsVerifier,
    core::kms::PreloadKMSBackendCorePoQGenerator,
    epoch_info::{PolEpochInfo, PolInfoProvider as PolInfoProviderTrait},
    membership::service::Adapter,
};
use lb_chain_broadcast_service::BlockBroadcastService;
use lb_chain_leader_service::LeaderMsg;
use lb_core::crypto::ZkHash;
use lb_libp2p::PeerId;
use lb_pol::{PolChainInputsData, PolWalletInputsData, PolWitnessInputsData};
use lb_poq::AGED_NOTE_MERKLE_TREE_HEIGHT;
use lb_services_utils::wait_until_services_are_ready;
use lb_time_service::backends::NtpTimeBackend;
use overwatch::{overwatch::OverwatchHandle, services::AsServiceId};
use tokio::sync::oneshot::channel;
use tokio_stream::wrappers::WatchStream;

use crate::generic_services::{
    ChainNetworkService, CryptarchiaLeaderService, CryptarchiaService, SdpService, WalletService,
};

pub type BlendMembershipAdapter<RuntimeServiceId> =
    Adapter<BlockBroadcastService<RuntimeServiceId>, PeerId>;
pub type BlendCoreService<RuntimeServiceId> = lb_blend_service::core::BlendService<
    lb_blend_service::core::backends::libp2p::Libp2pBlendBackend,
    PeerId,
    lb_blend_service::core::network::libp2p::Libp2pAdapter<RuntimeServiceId>,
    BlendMembershipAdapter<RuntimeServiceId>,
    SdpService<RuntimeServiceId>,
    RealCoreAndLeaderProofsGenerator<PreloadKMSBackendCorePoQGenerator<RuntimeServiceId>>,
    RealProofsVerifier,
    NtpTimeBackend,
    CryptarchiaService<RuntimeServiceId>,
    PolInfoProvider,
    RuntimeServiceId,
>;
pub type BlendEdgeService<RuntimeServiceId> = lb_blend_service::edge::BlendService<
        lb_blend_service::edge::backends::libp2p::Libp2pBlendBackend,
        PeerId,
        <lb_blend_service::core::network::libp2p::Libp2pAdapter<RuntimeServiceId> as lb_blend_service::core::network::NetworkAdapter<RuntimeServiceId>>::BroadcastSettings,
        BlendMembershipAdapter<RuntimeServiceId>,
        RealLeaderProofsGenerator,
        NtpTimeBackend,
        CryptarchiaService<RuntimeServiceId>,
        PolInfoProvider,
        RuntimeServiceId
    >;
pub type BlendService<RuntimeServiceId> = lb_blend_service::BlendService<
    BlendCoreService<RuntimeServiceId>,
    BlendEdgeService<RuntimeServiceId>,
    RuntimeServiceId,
>;

/// The provider of a stream of winning `PoL` epoch slots for the Blend service,
/// without introducing a cyclic dependency from Blend service to chain service.
pub struct PolInfoProvider;

#[async_trait]
impl<RuntimeServiceId> PolInfoProviderTrait<RuntimeServiceId> for PolInfoProvider
where
    RuntimeServiceId: AsServiceId<
            CryptarchiaLeaderService<
                CryptarchiaService<RuntimeServiceId>,
                ChainNetworkService<RuntimeServiceId>,
                WalletService<CryptarchiaService<RuntimeServiceId>, RuntimeServiceId>,
                RuntimeServiceId,
            >,
        > + Debug
        + Display
        + Send
        + Sync
        + 'static,
{
    type Stream = Box<dyn Stream<Item = PolEpochInfo> + Send + Unpin>;

    async fn subscribe(
        overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    ) -> Option<Self::Stream> {
        use lb_groth16::Field as _;

        wait_until_services_are_ready!(
            overwatch_handle,
            // No timeout since chain-leader service becomes ready
            // only after switching to Online mode.
            None,
            CryptarchiaLeaderService<
                CryptarchiaService<RuntimeServiceId>,
                ChainNetworkService<RuntimeServiceId>,
                WalletService<CryptarchiaService<RuntimeServiceId>, RuntimeServiceId>,
                RuntimeServiceId,
            >
        )
        .await
        .ok()?;
        let cryptarchia_service_relay = overwatch_handle
            .relay::<CryptarchiaLeaderService<
                CryptarchiaService<RuntimeServiceId>,
                ChainNetworkService<RuntimeServiceId>,
                WalletService<CryptarchiaService<RuntimeServiceId>, RuntimeServiceId>,
                RuntimeServiceId,
            >>()
            .await
            .ok()?;
        let (sender, receiver) = channel();
        cryptarchia_service_relay
            .send(LeaderMsg::PotentialWinningPolEpochSlotStreamSubscribe { sender })
            .await
            .ok()?;
        let pol_winning_slot_receiver = receiver.await.ok()?;
        // Return a `WatchStream` that filters out `None`s (i.e., at the very beginning
        // of chain leader start).
        Some(Box::new(
            WatchStream::new(pol_winning_slot_receiver)
                .filter_map(ready)
                .map(|(leader_private, _)| {
                    let PolWitnessInputsData {
                        wallet:
                            PolWalletInputsData {
                                aged_path,
                                aged_selector,
                                note_value,
                                output_number,
                                secret_key,
                                transaction_hash,
                                ..
                            },
                        chain: PolChainInputsData { slot_number, epoch_nonce, .. },
                    } = leader_private.input();

                // TODO: Remove this if `PoL` stuff also migrates to using fixed-size arrays or starts using vecs of the expected length instead of empty ones when generating `LeaderPrivate` values.
                let aged_path_and_selectors = {
                    let mut vec_from_inputs: Vec<_> = aged_path.iter().copied().zip(aged_selector.iter().copied()).collect();
                    let input_len = vec_from_inputs.len();
                    if input_len != AGED_NOTE_MERKLE_TREE_HEIGHT {
                        tracing::warn!("Provided merkle path for aged notes does not match the expected size for PoQ inputs.");
                    }
                    vec_from_inputs.resize(AGED_NOTE_MERKLE_TREE_HEIGHT, (ZkHash::ZERO, false));
                    vec_from_inputs
                };

                PolEpochInfo {
                    nonce: *epoch_nonce,
                    poq_private_inputs: ProofOfLeadershipQuotaInputs {
                        aged_path_and_selectors: aged_path_and_selectors.try_into().expect("List of aged note paths and selectors does not match the expected size for PoQ inputs, although it has already been pre-processed."),
                        note_value: *note_value,
                        output_number: *output_number,
                        secret_key: *secret_key,
                        slot: *slot_number,
                        transaction_hash: *transaction_hash,
                    },
                }
            }),
        ))
    }
}
