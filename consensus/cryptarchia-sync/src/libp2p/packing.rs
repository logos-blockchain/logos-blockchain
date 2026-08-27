use std::io;

use futures::{AsyncReadExt, AsyncWriteExt};
use lb_core::codec::{self, BoundedBytes, BoundedSerializeOp, DeserializeOp as _};
use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;

use super::MAX_MSG_LEN;

type Result<T> = std::result::Result<T, PackingError>;

type LenType = u32;
const MAX_MSG_LEN_BYTES: usize = size_of::<LenType>();

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
    Message: BoundedSerializeOp + DeserializeOwned + Sync,
    Writer: AsyncWriteExt + Send + Unpin,
{
    if <Message::Bytes as BoundedBytes>::MAX > MAX_MSG_LEN {
        return Err(PackingError::MessageTooLarge {
            max: MAX_MSG_LEN,
            actual: <Message::Bytes as BoundedBytes>::MAX,
        });
    }

    let packed_message = message.to_bounded_bytes()?;
    let packed_message = packed_message.as_ref();
    let length_prefix: LenType = packed_message
        .len()
        .try_into()
        .expect("MAX_MSG_LEN should fit in the frame length prefix");

    writer
        .write_all(&length_prefix.to_le_bytes())
        .await
        .map_err(Into::<PackingError>::into)?;

    writer.write_all(packed_message).await.map_err(Into::into)
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
    // Bound the peer-supplied length before allocating, otherwise a malicious
    // peer can send a ~4 GiB length prefix and OOM the node. `MAX_MSG_LEN` is the
    // same cap `pack_to_writer` enforces on the send side.
    if data_length > MAX_MSG_LEN {
        return Err(PackingError::MessageTooLarge {
            max: MAX_MSG_LEN,
            actual: data_length,
        });
    }
    let mut data = vec![0u8; data_length];
    reader.read_exact(&mut data).await?;
    Ok(Message::from_bytes(&data)?)
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::*;
    use crate::{
        libp2p::messages::{DownloadBlocksResponse, RequestMessage},
        messages::GetTipResponse,
    };

    fn assert_valid_message_bound<T>()
    where
        T: BoundedSerializeOp,
    {
        assert!(<T::Bytes as BoundedBytes>::MAX <= MAX_MSG_LEN);
    }

    #[tokio::test]
    async fn sender_rejects_messages_above_frame_limit() {
        let message = DownloadBlocksResponse::Block(Bytes::from(vec![0u8; MAX_MSG_LEN]));
        let mut writer = futures::io::Cursor::new(Vec::new());

        let error = pack_to_writer(&message, &mut writer).await.unwrap_err();

        assert!(matches!(
            error,
            PackingError::Serialization(codec::Error::Serialize(_))
        ));
        assert!(writer.into_inner().is_empty());
    }

    #[test]
    fn production_messages_have_bounded_serialization() {
        assert_valid_message_bound::<RequestMessage>();
        assert_valid_message_bound::<DownloadBlocksResponse>();
        assert_valid_message_bound::<GetTipResponse>();
    }
}
