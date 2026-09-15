use std::{io, num::NonZeroUsize};

use futures::{AsyncReadExt as _, AsyncWriteExt as _};
use libp2p::Stream;

use crate::message::{IncomingMessage, OutgoingMessage};

pub mod core;
pub mod message;

/// Write a message to the stream.
pub async fn send_msg(mut stream: Stream, msg: OutgoingMessage) -> io::Result<Stream> {
    stream.write_all(msg.as_ref()).await?;
    stream.flush().await?;
    Ok(stream)
}

/// Read one message of `message_size` bytes from the stream.
pub(crate) async fn recv_msg(
    mut stream: Stream,
    message_size: NonZeroUsize,
) -> io::Result<(Stream, IncomingMessage)> {
    let mut buf = vec![0; message_size.get()].into_boxed_slice();
    stream.read_exact(&mut buf).await?;
    Ok((stream, buf.into()))
}
