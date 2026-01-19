use std::{path::PathBuf, sync::mpsc::Sender};

use clap::Args;
use lb_core::{
    da::BlobId,
    mantle::ops::channel::{ChannelId, Ed25519PublicKey, MsgId},
};
use lb_executor_http_client::{BasicAuthCredentials, ExecutorHttpClient};
use lb_kzgrs_backend::encoder::DaEncoderParams;
use reqwest::Url;

#[derive(Args, Debug)]
pub struct Disseminate {
    #[clap(short, long)]
    pub channel_id: String,
    #[clap(short, long)]
    pub parent_msg_id: String,
    #[clap(short, long)]
    pub signer: String,
    /// Text to disseminate.
    #[clap(short, long, required_unless_present("file"))]
    pub data: Option<String>,
    /// File to disseminate.
    #[clap(short, long)]
    pub file: Option<PathBuf>,
    /// Executor address which is responsible for dissemination.
    #[clap(long)]
    pub addr: Url,
    /// Optional username for authentication.
    #[clap(long)]
    pub username: Option<String>,
    /// Optional password for authentication.
    #[clap(long)]
    pub password: Option<String>,
}

impl Disseminate {
    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: Address this at some point."
    )]
    pub fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        let basic_auth = self
            .username
            .map(|u| BasicAuthCredentials::new(u, self.password.clone()));

        let client = ExecutorHttpClient::new(basic_auth);

        let mut bytes: Vec<u8> = if let Some(data) = &self.data {
            data.clone().into_bytes()
        } else {
            let file_path = self.file.as_ref().unwrap();
            std::fs::read(file_path)?
        };

        let remainder = bytes.len() % DaEncoderParams::MAX_BLS12_381_ENCODING_CHUNK_SIZE;
        if remainder != 0 {
            bytes.resize(
                bytes.len() + (DaEncoderParams::MAX_BLS12_381_ENCODING_CHUNK_SIZE - remainder),
                0,
            );
        }

        let channel_id: [u8; 32] = hex::decode(&self.channel_id)?
            .try_into()
            .map_err(|_| "Invalid channel_id")?;
        let parent_msg_id: [u8; 32] = hex::decode(&self.parent_msg_id)?
            .try_into()
            .map_err(|_| "Invalid parent_msg_id")?;
        let signer_bytes: [u8; 32] = hex::decode(&self.signer)?
            .try_into()
            .map_err(|_| "Invalid signer hex: must be 32 bytes")?;
        let signer: Ed25519PublicKey = Ed25519PublicKey::from_bytes(&signer_bytes)
            .map_err(|e| format!("Invalid signer public key: {e}"))?;

        let (res_sender, res_receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            disperse_data(
                &res_sender,
                &client,
                self.addr.clone(),
                channel_id.into(),
                parent_msg_id.into(),
                signer,
                bytes,
            );
        });

        match res_receiver.recv() {
            Ok(update) => match update {
                Ok(_) => tracing::info!("Data successfully disseminated."),
                Err(e) => {
                    tracing::error!("Error disseminating data: {e}");
                    return Err(e.into());
                }
            },
            Err(e) => {
                tracing::error!("Failed to receive from client thread: {e}");
                return Err(e.into());
            }
        }

        tracing::info!("Done");
        Ok(())
    }
}

#[tokio::main]
async fn disperse_data(
    res_sender: &Sender<Result<BlobId, String>>,
    client: &ExecutorHttpClient,
    base_url: Url,
    channel_id: ChannelId,
    parent_msg_id: MsgId,
    signer: Ed25519PublicKey,
    bytes: Vec<u8>,
) {
    let res = client
        .publish_blob(base_url, channel_id, parent_msg_id, signer, bytes)
        .await
        .map_err(|err| format!("Failed to publish blob: {err:?}"));
    res_sender.send(res).unwrap();
}
