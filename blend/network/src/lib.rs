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
    const MAX_MESSAGE_SIZE: usize = u16::MAX as usize; // 65535 bytes
    
    let mut msg_len = [0; size_of::<u16>()];
    stream.read_exact(&mut msg_len).await?;
    let msg_len = u16::from_le_bytes(msg_len) as usize;

    // Defense-in-depth: validate message length even though u16 bounds it
    // This protects against potential future changes to the length type
    if msg_len > MAX_MESSAGE_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Message length {msg_len} exceeds maximum {MAX_MESSAGE_SIZE}"),
        ));
    }

    let mut buf = vec![0; msg_len];
    stream.read_exact(&mut buf).await?;
    Ok((stream, buf))
}
