use std::io;

use futures::{AsyncReadExt, AsyncWriteExt};
use lb_core::codec::{DeserializeOp as _, SerializeOp as _};
use serde::{Serialize, de::DeserializeOwned};

use crate::Result;

type LenType = u16;
const MAX_MSG_LEN_BYTES: usize = size_of::<LenType>();
const MAX_MSG_LEN: usize = 1 << (MAX_MSG_LEN_BYTES * 8);

struct MessageTooLargeError(usize);

impl From<MessageTooLargeError> for io::Error {
    fn from(value: MessageTooLargeError) -> Self {
        Self::new(
            io::ErrorKind::InvalidData,
            format!(
                "Message too large. Maximum size is {}. Actual size is {}",
                MAX_MSG_LEN, value.0
            ),
        )
    }
}

pub fn pack<Message>(message: &Message) -> Result<Vec<u8>>
where
    Message: Serialize + DeserializeOwned,
{
    Ok(message.to_bytes()?.into())
}

fn get_packed_message_size(packed_message: &[u8]) -> Result<usize> {
    let data_length = packed_message.len();
    if data_length > MAX_MSG_LEN {
        return Err(MessageTooLargeError(data_length).into());
    }
    Ok(data_length)
}

fn prepare_message_for_writer(packed_message: &[u8]) -> Result<Vec<u8>> {
    let data_length = get_packed_message_size(packed_message)?;
    let mut buffer = Vec::with_capacity(MAX_MSG_LEN_BYTES + data_length);
    buffer.extend_from_slice(&(data_length as LenType).to_le_bytes());
    buffer.extend_from_slice(packed_message);
    Ok(buffer)
}

pub async fn pack_to_writer<Message, Writer>(message: &Message, writer: &mut Writer) -> Result<()>
where
    Message: Serialize + DeserializeOwned + Sync,
    Writer: AsyncWriteExt + Send + Unpin,
{
    let packed_message = pack(message)?;
    let prepared_packed_message = prepare_message_for_writer(&packed_message)?;
    writer.write_all(&prepared_packed_message).await
}

async fn read_data_length<R>(reader: &mut R) -> Result<usize>
where
    R: AsyncReadExt + Unpin,
{
    let mut length_prefix = [0u8; MAX_MSG_LEN_BYTES];
    reader.read_exact(&mut length_prefix).await?;
    let s = LenType::from_le_bytes(length_prefix) as usize;
    Ok(s)
}

pub fn unpack<M: DeserializeOwned + Serialize>(data: &[u8]) -> Result<M> {
    M::from_bytes(data).map_err(io::Error::from)
}

pub async fn unpack_from_reader<Message, R>(reader: &mut R) -> Result<Message>
where
    Message: DeserializeOwned + Serialize,
    R: AsyncReadExt + Unpin,
{
    let data_length = read_data_length(reader).await?;
    let mut data = vec![0u8; data_length];
    reader.read_exact(&mut data).await?;
    unpack(&data)
}

#[cfg(test)]
mod tests {
    use futures::io::BufReader;
    use lb_core::da::BlobId;
    use lb_kzgrs_backend::testutils::get_da_share;

    use super::*;
    use crate::{
        common::Share,
        dispersal::{DispersalError, DispersalErrorType, DispersalRequest},
    };

    #[tokio::test]
    async fn pack_and_unpack() -> Result<()> {
        let blob_id = BlobId::from([0; 32]);
        let share = get_da_share(None);
        let share = Share::new(blob_id, share);
        let message = DispersalRequest::from(share.data);

        let packed_message = pack(&message)?;
        let unpacked_message: DispersalRequest = unpack(&packed_message)?;

        assert_eq!(message, unpacked_message);
        Ok(())
    }

    #[tokio::test]
    async fn pack_to_writer_and_unpack_from_reader() -> Result<()> {
        let blob_id = BlobId::from([0; 32]);
        let data = get_da_share(None);
        let share = Share::new(blob_id, data);
        let message = DispersalRequest::from(share.data);

        let mut writer = Vec::new();
        pack_to_writer(&message, &mut writer).await?;

        let mut reader = BufReader::new(writer.as_slice());
        let unpacked_message: DispersalRequest = unpack_from_reader(&mut reader).await?;

        assert_eq!(message, unpacked_message);
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::large_stack_arrays,
        reason = "TODO: Address this at some point."
    )]
    async fn pack_to_writer_too_large_message() {
        let blob_id = BlobId::from([0; 32]);
        let error_description = ["."; MAX_MSG_LEN].concat();
        let message =
            DispersalError::new(blob_id, DispersalErrorType::ChunkSize, error_description);

        let mut writer = Vec::new();
        let res = pack_to_writer(&message, &mut writer).await;
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
}
