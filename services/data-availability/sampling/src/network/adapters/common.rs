macro_rules! adapter_for {
    ($DaNetworkBackend:ident, $DaNetworkMessage:ident, $DaEventKind:ident, $DaNetworkEvent:ident) => {
        pub struct Libp2pAdapter<Membership,MembershipServiceAdapter, StorageAdapter, ApiAdapter, SdpAdapter, RuntimeServiceId>
        where
            Membership: MembershipHandler<NetworkId = SubnetworkId, Id = PeerId>
                + Debug
                + Clone
                + Send
                + Sync
                + 'static,
            MembershipServiceAdapter: MembershipAdapter,
            ApiAdapter: ApiAdapterTrait,

        {
            network_relay: OutboundRelay<
                <NetworkService<$DaNetworkBackend<Membership>, Membership, MembershipServiceAdapter, StorageAdapter,ApiAdapter, SdpAdapter, RuntimeServiceId> as ServiceData>::Message,
            >,
        }

        #[async_trait::async_trait]
        impl<Membership, MembershipServiceAdapter, StorageAdapter, ApiAdapter,SdpAdapter,RuntimeServiceId> NetworkAdapter<RuntimeServiceId> for Libp2pAdapter<Membership, MembershipServiceAdapter, StorageAdapter,ApiAdapter, SdpAdapter,RuntimeServiceId>
        where
            Membership: MembershipHandler<NetworkId = SubnetworkId, Id = PeerId>
                + Debug
                + Clone
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
            type Membership = Membership;
            type Storage = StorageAdapter;
            type MembershipAdapter = MembershipServiceAdapter;
            type ApiAdapter = ApiAdapter;
            type SdpAdapter = SdpAdapter;

            async fn new(
                network_relay: OutboundRelay<<NetworkService<Self::Backend, Self::Membership, Self::MembershipAdapter, Self::Storage, Self::ApiAdapter, Self::SdpAdapter, RuntimeServiceId> as ServiceData>::Message>,
            ) -> Self {
                Self { network_relay }
            }

            async fn start_sampling(
                &mut self,
                blob_id: BlobId,
                session: SessionNumber,
            ) -> Result<(), DynError> {
                self.network_relay
                    .send(DaNetworkMsg::Process($DaNetworkMessage::RequestSample {
                        blob_id,
                        session,
                    }))
                    .await
                    .expect("RequestSample message should have been sent");
                Ok(())
            }

            async fn request_historic_sampling(
                &self,
                block_id: HeaderId,
                blob_ids: HashMap<BlobId, SessionNumber>,
            ) -> Result<(), DynError> {
                self.network_relay
                    .send(DaNetworkMsg::RequestHistoricSampling{
                        block_id,
                        blob_ids,
                    })
                    .await
                    .expect("RequestHistoricSampling message should have been sent");
                Ok(())
            }

            async fn request_historic_commitments(
                &self,
                block_id: HeaderId,
                blob_id: BlobId,
                session: SessionNumber,
            ) -> Result<(), DynError> {
                self.network_relay
                    .send(DaNetworkMsg::RequestHistoricCommitments{
                        block_id,
                        blob_id,
                        session,
                    })
                    .await
                    .expect("RequestHistoricCommitments message should have been sent");
                Ok(())
            }

            async fn listen_to_sampling_messages(
                &self,
            ) -> Result<Pin<Box<dyn Stream<Item = SamplingEvent> + Send>>, DynError> {
                let (stream_sender, stream_receiver) = oneshot::channel();
                self.network_relay
                    .send(DaNetworkMsg::Subscribe {
                        kind: $DaEventKind::Sampling,
                        sender: stream_sender,
                    })
                    .await
                    .map_err(|(error, _)| error)?;
                stream_receiver
                    .await
                    .map(|stream| {
                        tokio_stream::StreamExt::filter_map(stream, |event| match event {
                            $DaNetworkEvent::Sampling(event) => {
                                Some(event)
                            }
                            _ => {
                                unreachable!("Subscribing to sampling events should return a sampling only event stream");
                            }
                        }).boxed()
                    })
                    .map_err(|error| Box::new(error) as DynError)
            }

            async fn listen_to_commitments_messages(
                &self,
            ) -> Result<Pin<Box<dyn Stream<Item = CommitmentsEvent> + Send>>, DynError> {
                let (stream_sender, stream_receiver) = oneshot::channel();
                self.network_relay
                    .send(DaNetworkMsg::Subscribe {
                        kind: $DaEventKind::Commitments,
                        sender: stream_sender,
                    })
                    .await
                    .map_err(|(error, _)| error)?;
                stream_receiver
                    .await
                    .map(|stream| {
                        tokio_stream::StreamExt::filter_map(stream, |event| match event {
                            $DaNetworkEvent::Commitments(event) => {
                                Some(event)
                            }
                            _ => {
                                unreachable!("Subscribing to commitments events should return a commitments only event stream");
                            }
                        }).boxed()
                    })
                    .map_err(|error| Box::new(error) as DynError)
            }

            async fn listen_to_historic_sampling_messages(
                &self,
            ) -> Result<Pin<Box<dyn Stream<Item = HistoricSamplingEvent> + Send>>, DynError> {
                let (stream_sender, stream_receiver) = oneshot::channel();
                self.network_relay
                    .send(DaNetworkMsg::Subscribe {
                        kind: $DaEventKind::HistoricSampling,
                        sender: stream_sender,
                    })
                    .await
                    .map_err(|(error, _)| error)?;
                stream_receiver
                    .await
                    .map(|stream| {
                        tokio_stream::StreamExt::filter_map(stream, |event| match event {
                            $DaNetworkEvent::HistoricSampling(event) => {
                                Some(event)
                            }
                            _ => {
                                unreachable!("Subscribing to historic sampling events should return a historic sampling only event stream");
                            }
                        }).boxed()
                    })
                    .map_err(|error| Box::new(error) as DynError)
            }

            async fn request_commitments(
                &self,
                blob_id: BlobId,
                session: SessionNumber,
            ) -> Result<(), DynError> {
                self.network_relay
                    .send(DaNetworkMsg::Process($DaNetworkMessage::RequestCommitments {
                        blob_id,
                        session,
                    }))
                    .await
                    .expect("RequestCommitments message should have been sent");
                Ok(())
            }
        }
    }
}

pub(crate) use adapter_for;
