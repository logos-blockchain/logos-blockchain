use std::io;

use ::core::num::NonZeroUsize;
use futures::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _};
use libp2p::Stream;

use crate::message::{IncomingMessage, OutgoingMessage};

pub mod core;
pub mod message;

pub type SendMsgResult = io::Result<Stream>;
/// Write a message to the stream.
pub async fn send_msg(mut stream: Stream, msg: OutgoingMessage) -> SendMsgResult {
    stream.write_all(msg.as_ref()).await?;
    stream.flush().await?;
    Ok(stream)
}

/// End a stream cleanly, once whatever was on it has been written.
pub(crate) async fn flush_and_close_stream(mut stream: Stream) {
    drop(stream.flush().await);
    drop(stream.close().await);
}

pub type RecvMsgResult<Reader> = io::Result<(Reader, IncomingMessage)>;

/// Read one message of `message_size` bytes from the stream.
///
/// Every message is the same size, fixed by the number of encapsulation
/// layers, so there is nothing to frame and nothing to agree on: either the
/// whole message arrives or the stream is over.
///
/// An `Err` is the stream being over, however it came about — the neighbour
/// closing it between messages, the neighbour stopping part way through one,
/// or the connection going away under the read. The reader cannot tell those
/// apart and does not need to: none of them delivered a message, and it is
/// deliveries that keep a neighbour its place.
pub(crate) async fn recv_msg<Reader>(
    mut stream: Reader,
    message_size: NonZeroUsize,
) -> RecvMsgResult<Reader>
where
    Reader: AsyncRead + Unpin,
{
    let mut buf = vec![0; message_size.get()].into_boxed_slice();
    stream.read_exact(&mut buf).await?;
    Ok((stream, buf.into()))
}

#[cfg(test)]
mod tests {
    use core::{
        num::NonZeroUsize,
        pin::Pin,
        task::{Context, Poll},
    };
    use std::io;

    use futures::AsyncRead;

    use crate::recv_msg;

    const MESSAGE_SIZE: NonZeroUsize = NonZeroUsize::new(64).unwrap();

    /// The error kinds a dying connection reaches a blocked reader as. Quinn
    /// reports a lost connection as `NotConnected`; the others are what the
    /// same event looks like through other transports and platforms.
    const TRANSPORT_FAILURES: [io::ErrorKind; 4] = [
        io::ErrorKind::NotConnected,
        io::ErrorKind::ConnectionReset,
        io::ErrorKind::ConnectionAborted,
        io::ErrorKind::BrokenPipe,
    ];

    /// A stream that hands over `delivers` bytes and then ends, or fails with
    /// `fails_with`.
    struct Stops {
        delivers: usize,
        fails_with: Option<io::ErrorKind>,
    }

    impl Stops {
        /// Ends cleanly, which is what a neighbour finishing its half of the
        /// stream looks like from here.
        const fn then_ends(delivers: usize) -> Self {
            Self {
                delivers,
                fails_with: None,
            }
        }

        const fn then_fails(delivers: usize, kind: io::ErrorKind) -> Self {
            Self {
                delivers,
                fails_with: Some(kind),
            }
        }
    }

    impl AsyncRead for Stops {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<io::Result<usize>> {
            if self.delivers == 0 {
                return Poll::Ready(
                    self.fails_with
                        .map_or(Ok(0), |kind| Err(io::Error::from(kind))),
                );
            }
            let handed_over = self.delivers.min(buf.len());
            self.delivers -= handed_over;
            Poll::Ready(Ok(handed_over))
        }
    }

    /// A stream that ends before a message begins is how a connection ends
    /// when the protocol asks for it, at an epoch rotation among other times.
    /// It reads as the stream being over, which is all the reader needs to
    /// know.
    #[tokio::test]
    async fn a_stream_that_ends_before_a_message_begins_is_over() {
        assert!(recv_msg(Stops::then_ends(0), MESSAGE_SIZE).await.is_err());

        for kind in TRANSPORT_FAILURES {
            assert!(
                recv_msg(Stops::then_fails(0, kind), MESSAGE_SIZE)
                    .await
                    .is_err(),
                "a {kind:?} before a message began did not end the stream"
            );
        }
    }

    /// The same, part way through a message. The neighbour may have stopped
    /// there or the connection may have gone away under the read; from here
    /// they are one event, and neither delivered a message.
    #[tokio::test]
    async fn a_message_that_stops_part_way_through_ends_the_stream() {
        assert!(
            recv_msg(Stops::then_ends(MESSAGE_SIZE.get() - 1), MESSAGE_SIZE)
                .await
                .is_err()
        );

        for kind in TRANSPORT_FAILURES {
            assert!(
                recv_msg(
                    Stops::then_fails(MESSAGE_SIZE.get() - 1, kind),
                    MESSAGE_SIZE
                )
                .await
                .is_err(),
                "a {kind:?} part way through a message was read as a whole message"
            );
        }
    }

    #[tokio::test]
    async fn a_whole_message_is_handed_over() {
        let outcome = recv_msg(Stops::then_ends(MESSAGE_SIZE.get()), MESSAGE_SIZE).await;

        assert!(outcome.is_ok());
    }
}
