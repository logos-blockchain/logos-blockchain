use std::{error::Error, sync::Arc, time::Duration};

use futures::StreamExt as _;
use kzgrs_backend::{
    common::build_blob_id,
    encoder,
    encoder::{DaEncoderParams, EncodedData},
};
use nomos_core::{
    da::{BlobId, DaDispersal, DaEncoder},
    mantle::{SignedMantleTx, tx_builder::MantleTxBuilder},
};
use nomos_da_network_service::backends::ProcessingError;
use nomos_tracing::info_with_id;
use nomos_utils::bounded_duration::{MinimalBoundedDuration, NANO};
use overwatch::DynError;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use tokio::sync::oneshot;
use tracing::instrument;

use crate::{
    adapters::{
        network::DispersalNetworkAdapter,
        wallet::{BlobOpArgs, DaWalletAdapter},
    },
    backend::{DispersalBackend, DispersalTask, InitialBlobOpArgs},
};

#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SampleSubnetworks {
    pub sample_threshold: usize,
    #[serde_as(as = "MinimalBoundedDuration<1, NANO>")]
    pub timeout: Duration,
    #[serde_as(as = "MinimalBoundedDuration<1, NANO>")]
    pub cooldown: Duration,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncoderSettings {
    pub num_columns: usize,
    pub with_cache: bool,
    pub global_params_path: String,
}

#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DispersalKZGRSBackendSettings {
    pub encoder_settings: EncoderSettings,
    #[serde_as(as = "MinimalBoundedDuration<1, NANO>")]
    pub dispersal_timeout: Duration,
    #[serde_as(as = "MinimalBoundedDuration<1, NANO>")]
    pub retry_cooldown: Duration,
    pub retry_limit: usize,
}

#[derive(Clone)]
pub struct DispersalKZGRSBackend<NetworkAdapter, WalletAdapter> {
    settings: DispersalKZGRSBackendSettings,
    network_adapter: Arc<NetworkAdapter>,
    wallet_adapter: Arc<WalletAdapter>,
    encoder: Arc<encoder::DaEncoder>,
}

pub struct DispersalHandler<NetworkAdapter, WalletAdapter> {
    network_adapter: Arc<NetworkAdapter>,
    wallet_adapter: Arc<WalletAdapter>,
    timeout: Duration,
    num_columns: usize,
}

impl<NetworkAdapter, WalletAdapter> Clone for DispersalHandler<NetworkAdapter, WalletAdapter> {
    fn clone(&self) -> Self {
        Self {
            network_adapter: Arc::clone(&self.network_adapter),
            wallet_adapter: Arc::clone(&self.wallet_adapter),
            timeout: self.timeout,
            num_columns: self.num_columns,
        }
    }
}

#[async_trait::async_trait]
impl<NetworkAdapter, WalletAdapter> DaDispersal for DispersalHandler<NetworkAdapter, WalletAdapter>
where
    NetworkAdapter: DispersalNetworkAdapter + Send + Sync,
    NetworkAdapter::SubnetworkId: From<u16> + Send + Sync,
    WalletAdapter: DaWalletAdapter + Send + Sync,
    WalletAdapter::Error: Error + Send + Sync + 'static,
{
    type EncodedData = EncodedData;
    type Tx = SignedMantleTx;
    type Error = DynError;

    async fn disperse_shares(&self, encoded_data: Self::EncodedData) -> Result<(), Self::Error> {
        let adapter = self.network_adapter.as_ref();
        let num_columns = encoded_data.combined_column_proofs.len();
        let blob_id = build_blob_id(&encoded_data.row_commitments);

        let responses_stream = adapter.dispersal_events_stream().await?;
        for (subnetwork_id, share) in encoded_data.into_iter().enumerate() {
            adapter
                .disperse_share((subnetwork_id as u16).into(), share)
                .await?;
        }

        let valid_responses = responses_stream
            .filter_map(async |event| match event {
                Ok((_blob_id, _)) if _blob_id == blob_id => Some(()),
                Err(e) => {
                    tracing::error!("Error dispersing in dispersal stream: {e}");
                    None
                }
                _ => None,
            })
            .take(num_columns)
            .collect::<()>();
        // timeout when collecting positive responses
        tokio::time::timeout(self.timeout, valid_responses)
            .await
            .map_err(|e| Box::new(e) as DynError)?;
        Ok(())
    }

    async fn disperse_tx(&self, blob_id: BlobId, tx: Self::Tx) -> Result<(), Self::Error> {
        let network_adapter = self.network_adapter.as_ref();
        let responses_stream = network_adapter.dispersal_events_stream().await?;

        for subnetwork_id in 0..self.num_columns {
            network_adapter
                .disperse_tx((subnetwork_id as u16).into(), tx.clone())
                .await?;
        }

        let valid_responses = responses_stream
            .filter_map(async |event| match event {
                Ok((_blob_id, _)) if _blob_id == blob_id => Some(()),
                _ => None,
            })
            .take(self.num_columns)
            .collect::<()>();
        // timeout when collecting positive responses
        tokio::time::timeout(self.timeout, valid_responses)
            .await
            .map_err(|e| Box::new(e) as DynError)
    }
}

impl<NetworkAdapter, WalletAdapter> DispersalKZGRSBackend<NetworkAdapter, WalletAdapter>
where
    NetworkAdapter: DispersalNetworkAdapter + Send + Sync + 'static,
    NetworkAdapter::SubnetworkId: From<u16> + Send + Sync,
    WalletAdapter: DaWalletAdapter + Send + Sync + 'static,
    WalletAdapter::Error: Error + Send + Sync + 'static,
{
    async fn encode(
        &self,
        data: Vec<u8>,
    ) -> Result<(BlobId, <encoder::DaEncoder as DaEncoder>::EncodedData), DynError> {
        let encoder = Arc::clone(&self.encoder);
        // this is a REALLY heavy task, so we should try not to block the thread here
        let heavy_task = tokio::task::spawn_blocking(move || encoder.encode(&data));
        let encoded_data = heavy_task.await??;
        let blob_id = build_blob_id(&encoded_data.row_commitments);
        Ok((blob_id, encoded_data))
    }

    async fn disperse(
        handler: DispersalHandler<NetworkAdapter, WalletAdapter>,
        tx_builder: MantleTxBuilder,
        blob_op_args: BlobOpArgs,
        encoded_data: <encoder::DaEncoder as DaEncoder>::EncodedData,
    ) -> Result<SignedMantleTx, DynError> {
        let blob_id = blob_op_args.blob_id;

        tracing::debug!("Dispersing {blob_id:?} transaction");
        let wallet_adapter = handler.wallet_adapter.as_ref();
        let tx = wallet_adapter
            .blob_tx(tx_builder, blob_op_args)
            .map_err(DynError::from)?;

        handler.disperse_tx(blob_id, tx.clone()).await?;
        tracing::debug!("Dispersing {blob_id:?} shares");
        handler.disperse_shares(encoded_data).await?;
        tracing::debug!("Dispersal of {blob_id:?} successful");
        Ok(tx)
    }

    fn create_dispersal_task(
        handler: DispersalHandler<NetworkAdapter, WalletAdapter>,
        tx_builder: MantleTxBuilder,
        blob_op_args: BlobOpArgs,
        encoded_data: EncodedData,
        sender: oneshot::Sender<Result<BlobId, DynError>>,
        retry_limit: usize,
        retry_cooldown: Duration,
    ) -> DispersalTask {
        Box::pin(async move {
            let blob_id = blob_op_args.blob_id;
            let channel_id = blob_op_args.channel_id;
            for attempt in 0..=retry_limit {
                match Self::disperse(
                    handler.clone(),
                    tx_builder.clone(),
                    blob_op_args.clone(),
                    encoded_data.clone(),
                )
                .await
                {
                    Ok(tx) => {
                        drop(sender.send(Ok(blob_id)));
                        return (channel_id, Some(tx));
                    }
                    Err(retry_err) => {
                        if !matches!(
                            retry_err.downcast_ref::<ProcessingError>(),
                            Some(ProcessingError::InsufficientSubnetworkConnections)
                        ) {
                            drop(sender.send(Err(retry_err)));
                            return (channel_id, None);
                        }
                    }
                }

                tokio::time::sleep(retry_cooldown).await;
                tracing::warn!(
                    "Retrying dispersal, attempt {}/{}...",
                    attempt + 1,
                    retry_limit
                );
            }
            drop(sender.send(Err("Retry limit reached".into())));
            (channel_id, None)
        })
    }
}

#[async_trait::async_trait]
impl<NetworkAdapter, WalletAdapter> DispersalBackend
    for DispersalKZGRSBackend<NetworkAdapter, WalletAdapter>
where
    NetworkAdapter: DispersalNetworkAdapter + Send + Sync + 'static,
    NetworkAdapter::SubnetworkId: From<u16> + Send + Sync,
    WalletAdapter: DaWalletAdapter + Send + Sync + 'static,
    WalletAdapter::Error: Error + Send + Sync + 'static,
{
    type Settings = DispersalKZGRSBackendSettings;
    type Encoder = encoder::DaEncoder;
    type Dispersal = DispersalHandler<NetworkAdapter, WalletAdapter>;
    type NetworkAdapter = NetworkAdapter;
    type WalletAdapter = WalletAdapter;
    type BlobId = BlobId;

    fn init(
        settings: Self::Settings,
        network_adapter: Self::NetworkAdapter,
        wallet_adapter: Self::WalletAdapter,
    ) -> Self {
        let encoder_settings = &settings.encoder_settings;
        let global_params =
            kzgrs_backend::kzg_keys::proving_key_from_file(&encoder_settings.global_params_path)
                .expect("Global encoder params should be available");
        let encoder = Self::Encoder::new(DaEncoderParams::new(
            encoder_settings.num_columns,
            encoder_settings.with_cache,
            global_params,
        ));
        Self {
            settings,
            network_adapter: Arc::new(network_adapter),
            wallet_adapter: Arc::new(wallet_adapter),
            encoder: Arc::new(encoder),
        }
    }

    #[instrument(skip_all)]
    async fn process_dispersal(
        &self,
        tx_builder: MantleTxBuilder,
        blob_op_args: InitialBlobOpArgs,
        data: Vec<u8>,
        sender: oneshot::Sender<Result<Self::BlobId, DynError>>,
    ) -> Result<DispersalTask, DynError> {
        let original_size = data.len();
        let (blob_id, encoded_data) = self.encode(data).await?;
        info_with_id!(blob_id.as_ref(), "ProcessDispersal");

        let handler = DispersalHandler {
            network_adapter: Arc::clone(&self.network_adapter),
            wallet_adapter: Arc::clone(&self.wallet_adapter),
            timeout: self.settings.dispersal_timeout,
            num_columns: self.settings.encoder_settings.num_columns,
        };

        let dispersal_task = Self::create_dispersal_task(
            handler,
            tx_builder,
            BlobOpArgs::from_initial(blob_op_args, blob_id, original_size),
            encoded_data,
            sender,
            self.settings.retry_limit,
            self.settings.retry_cooldown,
        );

        Ok(dispersal_task)
    }
}
