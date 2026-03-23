use std::io;

use futures::{AsyncReadExt, AsyncWriteExt};
use lb_core::codec::{self, DeserializeOp as _, SerializeOp as _};
use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;

type Result<T> = std::result::Result<T, PackingError>;

type LenType = u32;
const MAX_MSG_LEN_BYTES: usize = size_of::<LenType>();
const MAX_MSG_LEN: usize = 16 * 1024 * 1024; // 16 MiB;

#[derive(Debug, Error)]
pub enum PackingError {
    #[error("Message too large. Maximum size is {max}, actual size is {actual}")]
    MessageTooLarge { max: usize, actual: usize },

    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("Serialization error")]
    Serialization(#[from] codec::Error),
}

pub async fn pack_to_writer<Message, Writer>(message: &Message, writer: &mut Writer) -> Result<()>
where
    Message: Serialize + DeserializeOwned + Sync,
    Writer: AsyncWriteExt + Send + Unpin,
{
    let packed_message = message.to_bytes()?;

    let length_prefix: LenType =
        packed_message
            .len()
            .try_into()
            .map_err(|_| PackingError::MessageTooLarge {
                max: MAX_MSG_LEN,
                actual: packed_message.len(),
            })?;

    writer
        .write_all(&length_prefix.to_le_bytes())
        .await
        .map_err(Into::<PackingError>::into)?;

    writer.write_all(&packed_message).await.map_err(Into::into)
}

async fn read_data_length<R>(reader: &mut R) -> Result<usize>
where
    R: AsyncReadExt + Unpin,
{
    let mut length_prefix = [0u8; MAX_MSG_LEN_BYTES];
    reader.read_exact(&mut length_prefix).await?;
    Ok(LenType::from_le_bytes(length_prefix) as usize)
}

pub async fn unpack_from_reader<Message, R>(reader: &mut R) -> Result<Message>
where
    Message: DeserializeOwned + Serialize,
    R: AsyncReadExt + Unpin,
{
    let data_length = read_data_length(reader).await?;
    let mut data = vec![0u8; data_length];
    reader.read_exact(&mut data).await?;
    Ok(Message::from_bytes(&data)?)
}
