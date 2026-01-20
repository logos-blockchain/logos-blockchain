macro_rules! adapter_for {
    ($DaNetworkBackend:ident, $DaNetworksEventKind:ident, $DaNetworkEvent:ident) => {
        pub struct Libp2pAdapter<
            Membership,
            MembershipServiceAdapter,
            StorageAdapter,
            ApiAdapter,
            SdpAdapter,
            RuntimeServiceId,
        > where
            Membership: MembershipHandler<NetworkId = SubnetworkId, Id = PeerId>
                + Clone
                + Debug
                + Send
                + Sync
                + 'static,
            MembershipServiceAdapter: MembershipAdapter,
            ApiAdapter: ApiAdapterTrait,
        {
            network_relay: OutboundRelay<
                <NetworkService<
                    $DaNetworkBackend<Membership>,
                    Membership,
                    MembershipServiceAdapter,
                    StorageAdapter,
                    ApiAdapter,
                    SdpAdapter,
                    RuntimeServiceId,
                > as ServiceData>::Message,
            >,
            _membership: PhantomData<Membership>,
        }

        #[async_trait::async_trait]
        impl<
            Membership,
            MembershipServiceAdapter,
            StorageAdapter,
            ApiAdapter,
            SdpAdapter,
            RuntimeServiceId,
        > NetworkAdapter<RuntimeServiceId>
            for Libp2pAdapter<
                Membership,
                MembershipServiceAdapter,
                StorageAdapter,
                ApiAdapter,
                SdpAdapter,
                RuntimeServiceId,
            >
        where
            Membership: MembershipHandler<NetworkId = SubnetworkId, Id = PeerId>
                + Clone
                + Debug
                + Send
                + Sync
                + 'static,
            MembershipServiceAdapter: MembershipAdapter,
            ApiAdapter: ApiAdapterTrait<
                    Share = DaShare,
                    BlobId = BlobId,
                    Commitments = DaSharesCommitments,
                    Membership = DaMembershipHandler<Membership>,
                > + Clone
                + Send
                + Sync
                + 'static,
            SdpAdapter: SdpAdapterTrait<RuntimeServiceId>,
        {
            type Backend = $DaNetworkBackend<Membership>;
            type Settings = ();
            type Share = DaShare;
            type Tx = SignedMantleTx;
            type Membership = Membership;
            type Storage = StorageAdapter;
            type MembershipAdapter = MembershipServiceAdapter;
            type ApiAdapter = ApiAdapter;
            type SdpAdapter = SdpAdapter;

            async fn new(
                _settings: Self::Settings,
                network_relay: OutboundRelay<
                    <NetworkService<
                        Self::Backend,
                        Self::Membership,
                        Self::MembershipAdapter,
                        Self::Storage,
                        Self::ApiAdapter,
                        Self::SdpAdapter,
                        RuntimeServiceId,
                    > as ServiceData>::Message,
                >,
            ) -> Self {
                Self {
                    network_relay,
                    _membership: Default::default(),
                }
            }

            async fn share_stream(
                &self,
            ) -> Box<dyn Stream<Item = ValidationRequest<Self::Share>> + Unpin + Send> {
                let (sender, receiver) = tokio::sync::oneshot::channel();
                self.network_relay
                    .send(lb_da_network_service::DaNetworkMsg::Subscribe {
                        kind: $DaNetworksEventKind::Verifying,
                        sender,
                    })
                    .await
                    .expect("Network backend should be ready");

                let receiver = receiver.await.expect("Blob stream should be received");

                let stream = receiver.filter_map(move |msg| match msg {
                    $DaNetworkEvent::Verifying(verification_event) => match verification_event {
                        VerificationEvent::Share {
                            share,
                            response_sender,
                        } => Some(ValidationRequest {
                            item: *share,
                            sender: response_sender,
                        }),
                        VerificationEvent::Tx { .. } => None,
                    },
                    _ => None,
                });

                Box::new(Box::pin(stream))
            }

            async fn tx_stream(
                &self,
            ) -> Box<dyn Stream<Item = ValidationRequest<(u16, Self::Tx)>> + Unpin + Send> {
                let (sender, receiver) = tokio::sync::oneshot::channel();
                self.network_relay
                    .send(lb_da_network_service::DaNetworkMsg::Subscribe {
                        kind: $DaNetworksEventKind::Verifying,
                        sender,
                    })
                    .await
                    .expect("Network backend should be ready");

                let receiver = receiver.await.expect("Blob stream should be received");

                let stream = receiver.filter_map(move |msg| match msg {
                    $DaNetworkEvent::Verifying(verification_event) => match verification_event {
                        VerificationEvent::Tx {
                            assignations,
                            tx,
                            response_sender,
                        } => Some(ValidationRequest {
                            item: (assignations, *tx),
                            sender: response_sender,
                        }),
                        VerificationEvent::Share { .. } => None,
                    },
                    _ => None,
                });

                Box::new(Box::pin(stream))
            }
        }
    };
}

pub(crate) use adapter_for;
