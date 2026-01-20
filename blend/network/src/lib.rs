use std::io;

use futures::{AsyncReadExt as _, AsyncWriteExt as _};
use libp2p::Stream;

pub mod core;

/// Write a message to the stream
pub async fn send_msg(mut stream: Stream, msg: Vec<u8>) -> io::Result<Stream> {
    let msg_len: u16 = msg.len().try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "Message length is too big. Got {}, expected {}",
                msg.len(),
                size_of::<u16>()
            ),
        )
    })?;
    stream.write_all(msg_len.to_le_bytes().as_ref()).await?;
    stream.write_all(&msg).await?;
    stream.flush().await?;
    Ok(stream)
}

/// Read a message from the stream
pub(crate) async fn recv_msg(mut stream: Stream) -> io::Result<(Stream, Vec<u8>)> {
    let mut msg_len = [0; size_of::<u16>()];
    stream.read_exact(&mut msg_len).await?;
    let msg_len = u16::from_le_bytes(msg_len) as usize;

    let mut buf = vec![0; msg_len];
    stream.read_exact(&mut buf).await?;
    Ok((stream, buf))
}
