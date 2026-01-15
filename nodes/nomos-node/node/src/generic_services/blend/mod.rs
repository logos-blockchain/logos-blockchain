use core::{
    fmt::{Debug, Display},
    future::ready,
    marker::PhantomData,
    time::Duration,
};

use async_trait::async_trait;
use broadcast_service::BlockBroadcastService;
use chain_leader::LeaderMsg;
use futures::{Stream, StreamExt as _};
use nomos_blend::proofs::quota::inputs::prove::private::ProofOfLeadershipQuotaInputs;
use nomos_blend_service::{
    core::kms::PreloadKMSBackendCorePoQGenerator,
    epoch_info::{PolEpochInfo, PolInfoProvider as PolInfoProviderTrait},
    membership::service::Adapter,
};
use nomos_core::crypto::ZkHash;
use nomos_da_sampling::network::NetworkAdapter;
use nomos_libp2p::PeerId;
use nomos_time::backends::NtpTimeBackend;
use overwatch::{overwatch::OverwatchHandle, services::AsServiceId};
use pol::{PolChainInputsData, PolWalletInputsData, PolWitnessInputsData};
use poq::AGED_NOTE_MERKLE_TREE_HEIGHT;
use services_utils::wait_until_services_are_ready;
use tokio::sync::oneshot::channel;
use tokio_stream::wrappers::WatchStream;

use crate::generic_services::{
    CryptarchiaLeaderService, CryptarchiaService, SdpService, WalletService,
    blend::proofs::{BlendProofsVerifier, CoreProofsGenerator, EdgeProofsGenerator},
};

mod proofs;

pub type BlendMembershipAdapter<RuntimeServiceId> =
    Adapter<BlockBroadcastService<RuntimeServiceId>, PeerId>;
pub type BlendCoreService<SamplingAdapter, RuntimeServiceId> =
    nomos_blend_service::core::BlendService<
        nomos_blend_service::core::backends::libp2p::Libp2pBlendBackend,
        PeerId,
        nomos_blend_service::core::network::libp2p::Libp2pAdapter<RuntimeServiceId>,
        BlendMembershipAdapter<RuntimeServiceId>,
        SdpService<RuntimeServiceId>,
        CoreProofsGenerator<PreloadKMSBackendCorePoQGenerator<RuntimeServiceId>>,
        BlendProofsVerifier,
        NtpTimeBackend,
        CryptarchiaService<RuntimeServiceId>,
        PolInfoProvider<SamplingAdapter>,
        RuntimeServiceId,
    >;
pub type BlendEdgeService<SamplingAdapter, RuntimeServiceId> = nomos_blend_service::edge::BlendService<
        nomos_blend_service::edge::backends::libp2p::Libp2pBlendBackend,
        PeerId,
        <nomos_blend_service::core::network::libp2p::Libp2pAdapter<RuntimeServiceId> as nomos_blend_service::core::network::NetworkAdapter<RuntimeServiceId>>::BroadcastSettings,
        BlendMembershipAdapter<RuntimeServiceId>,
        EdgeProofsGenerator,
        NtpTimeBackend,
        CryptarchiaService<RuntimeServiceId>,
        PolInfoProvider<SamplingAdapter>,
        RuntimeServiceId
    >;
pub type BlendService<SamplingAdapter, RuntimeServiceId> = nomos_blend_service::BlendService<
    BlendCoreService<SamplingAdapter, RuntimeServiceId>,
    BlendEdgeService<SamplingAdapter, RuntimeServiceId>,
    RuntimeServiceId,
>;

/// The provider of a stream of winning `PoL` epoch slots for the Blend service,
/// without introducing a cyclic dependency from Blend service to chain service.
pub struct PolInfoProvider<SamplingAdapter>(PhantomData<SamplingAdapter>);

#[async_trait]
impl<SamplingAdapter, RuntimeServiceId> PolInfoProviderTrait<RuntimeServiceId>
    for PolInfoProvider<SamplingAdapter>
where
    SamplingAdapter: NetworkAdapter<RuntimeServiceId> + 'static,
    RuntimeServiceId: AsServiceId<
            CryptarchiaLeaderService<
                CryptarchiaService<RuntimeServiceId>,
                WalletService<CryptarchiaService<RuntimeServiceId>, RuntimeServiceId>,
                SamplingAdapter,
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
        use groth16::Field as _;

        wait_until_services_are_ready!(
            overwatch_handle,
            Some(Duration::from_secs(60)),
            CryptarchiaLeaderService<
                CryptarchiaService<RuntimeServiceId>,
                WalletService<CryptarchiaService<RuntimeServiceId>, RuntimeServiceId>,
                SamplingAdapter,
                RuntimeServiceId,
            >
        )
        .await
        .ok()?;
        let cryptarchia_service_relay = overwatch_handle
            .relay::<CryptarchiaLeaderService<
                CryptarchiaService<RuntimeServiceId>,
                WalletService<CryptarchiaService<RuntimeServiceId>, RuntimeServiceId>,
                SamplingAdapter,
                RuntimeServiceId,
            >>()
            .await
            .ok()?;
        let (sender, receiver) = channel();
        cryptarchia_service_relay
            .send(LeaderMsg::WinningPolEpochSlotStreamSubscribe { sender })
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
