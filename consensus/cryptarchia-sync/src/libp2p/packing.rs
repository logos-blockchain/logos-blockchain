use std::io;

use futures::{AsyncReadExt, AsyncWriteExt};
use lb_binary_codec::bincode::{self, BoundedBytes, BoundedSerializeOp, DeserializeOp as _};
use lb_utils::net::MAX_WIRE_MESSAGE_SIZE;
use serde::de::DeserializeOwned;
use thiserror::Error;

type Result<T> = std::result::Result<T, PackingError>;

type LenType = u32;
const LENGTH_PREFIX_BYTES: usize = size_of::<LenType>();

#[derive(Debug, Error)]
pub enum PackingError {
    #[error("Message too large. Maximum size is {max}, actual size is {actual}")]
    MessageTooLarge { max: usize, actual: usize },

    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("Serialization error")]
    Serialization(#[from] bincode::Error),
}

pub async fn pack_to_writer<Message, Writer>(message: &Message, writer: &mut Writer) -> Result<()>
where
    Message: BoundedSerializeOp + DeserializeOwned + Sync,
    Writer: AsyncWriteExt + Send + Unpin,
{
    const {
        assert!(MAX_WIRE_MESSAGE_SIZE <= LenType::MAX as usize);
        assert!(<Message::Bytes as BoundedBytes>::MAX <= MAX_WIRE_MESSAGE_SIZE);
    }

    let packed_message = message.to_bounded_bytes()?;
    let packed_message = packed_message.as_ref();
    let length_prefix = packed_message.len() as LenType;

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
    let mut length_prefix = [0u8; LENGTH_PREFIX_BYTES];
    reader.read_exact(&mut length_prefix).await?;
    Ok(LenType::from_le_bytes(length_prefix) as usize)
}

pub async fn unpack_from_reader<Message, R>(reader: &mut R) -> Result<Message>
where
    Message: BoundedSerializeOp + DeserializeOwned,
    R: AsyncReadExt + Unpin,
{
    let data_length = read_data_length(reader).await?;
    // Apply the hard transport ceiling before the type-specific bound. Both
    // checks happen before allocating or reading the payload.
    if data_length > MAX_WIRE_MESSAGE_SIZE {
        return Err(PackingError::MessageTooLarge {
            max: MAX_WIRE_MESSAGE_SIZE,
            actual: data_length,
        });
    }
    let message_max = <Message::Bytes as BoundedBytes>::MAX;
    if data_length > message_max {
        return Err(PackingError::MessageTooLarge {
            max: message_max,
            actual: data_length,
        });
    }
    let mut data = vec![0u8; data_length];
    reader.read_exact(&mut data).await?;
    Ok(Message::from_bytes(&data)?)
}

#[cfg(test)]
mod tests {
    use std::{
        pin::Pin,
        task::{Context, Poll},
    };

    use bytes::Bytes;
    use lb_utils::net::MAX_WIRE_MESSAGE_SIZE;

    use super::*;
    use crate::libp2p::{
        messages::{
            DownloadBlocksRequest, DownloadBlocksResponse,
            MAX_DOWNLOAD_BLOCKS_RESPONSE_BINCODE_SIZE, MAX_REQUEST_MESSAGE_BINCODE_SIZE,
            RequestMessage,
        },
        provider::MAX_ADDITIONAL_BLOCKS,
    };

    struct PrefixOnlyReader {
        prefix: [u8; LENGTH_PREFIX_BYTES],
        offset: usize,
        payload_requested: bool,
    }

    impl PrefixOnlyReader {
        fn new(length: usize) -> Self {
            Self {
                prefix: (length as LenType).to_le_bytes(),
                offset: 0,
                payload_requested: false,
            }
        }
    }

    impl futures::AsyncRead for PrefixOnlyReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buffer: &mut [u8],
        ) -> Poll<io::Result<usize>> {
            if self.offset < self.prefix.len() {
                let count = (self.prefix.len() - self.offset).min(buffer.len());
                buffer[..count].copy_from_slice(&self.prefix[self.offset..self.offset + count]);
                self.offset += count;
                Poll::Ready(Ok(count))
            } else {
                self.payload_requested = true;
                Poll::Ready(Err(io::Error::other("payload was requested")))
            }
        }
    }

    #[tokio::test]
    async fn sender_rejects_messages_above_message_limit() {
        let message = DownloadBlocksResponse::Block(Bytes::from(vec![
            0u8;
            MAX_DOWNLOAD_BLOCKS_RESPONSE_BINCODE_SIZE
        ]));
        let mut writer = futures::io::Cursor::new(Vec::new());

        let error = pack_to_writer(&message, &mut writer).await.unwrap_err();

        assert!(matches!(
            error,
            PackingError::Serialization(bincode::Error::Serialize(_))
        ));
        assert!(writer.into_inner().is_empty());
    }

    #[tokio::test]
    async fn receiver_rejects_global_oversize_before_reading_payload() {
        let mut reader = PrefixOnlyReader::new(MAX_WIRE_MESSAGE_SIZE + 1);
        let error = unpack_from_reader::<RequestMessage, _>(&mut reader)
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            PackingError::MessageTooLarge {
                max: MAX_WIRE_MESSAGE_SIZE,
                ..
            }
        ));
        assert!(!reader.payload_requested);
    }

    #[tokio::test]
    async fn receiver_rejects_message_oversize_before_reading_payload() {
        let mut reader = PrefixOnlyReader::new(MAX_REQUEST_MESSAGE_BINCODE_SIZE + 1);
        let error = unpack_from_reader::<RequestMessage, _>(&mut reader)
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            PackingError::MessageTooLarge {
                max: MAX_REQUEST_MESSAGE_BINCODE_SIZE,
                ..
            }
        ));
        assert!(!reader.payload_requested);
    }

    #[tokio::test]
    async fn receiver_accepts_a_valid_frame() {
        let request = RequestMessage::DownloadBlocksRequest(DownloadBlocksRequest::new(
            [0; 32].into(),
            [1; 32].into(),
            [2; 32].into(),
            (3..3 + MAX_ADDITIONAL_BLOCKS)
                .map(|index| [index as u8; 32].into())
                .collect(),
        ));
        let bytes = request.to_bounded_bytes().unwrap();
        let mut frame = Vec::with_capacity(LENGTH_PREFIX_BYTES + bytes.len());
        frame.extend_from_slice(&(bytes.len() as LenType).to_le_bytes());
        frame.extend_from_slice(bytes.as_ref());

        let decoded = unpack_from_reader::<RequestMessage, _>(&mut futures::io::Cursor::new(frame))
            .await
            .unwrap();
        assert!(matches!(decoded, RequestMessage::DownloadBlocksRequest(_)));
    }
}
