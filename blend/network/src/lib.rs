use std::io;

use futures::{AsyncReadExt as _, AsyncWriteExt as _};
use libp2p::Stream;

use crate::message::{FRAME_LENGTH_WIRE_SIZE, FrameLength, IncomingMessage, OutgoingMessage};

pub mod core;
pub mod message;

/// Write a message to the stream
pub async fn send_msg(mut stream: Stream, msg: OutgoingMessage) -> io::Result<Stream> {
    stream
        .write_all(msg.wire_length().to_le_bytes().as_ref())
        .await?;
    stream.write_all(msg.as_ref()).await?;
    stream.flush().await?;
    Ok(stream)
}

/// Read a message from the stream
pub(crate) async fn recv_msg(mut stream: Stream) -> io::Result<(Stream, IncomingMessage)> {
    let mut msg_len = [0; FRAME_LENGTH_WIRE_SIZE];
    stream.read_exact(&mut msg_len).await?;
    let msg_len = FrameLength::from_le_bytes(msg_len) as usize;
    let mut buf = vec![0; msg_len].into_boxed_slice();
    stream.read_exact(&mut buf).await?;
    Ok((stream, buf.into()))
}
