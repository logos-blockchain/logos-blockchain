use std::{io, num::NonZeroUsize};

use ::core::{
    error,
    fmt::{self, Display, Formatter},
};
use futures::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _};
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

/// A message that stopped part way through, or a stream that failed while one
/// was being read.
///
/// This is the spec's "failure of the authenticated stream", which it defines
/// as a violation of the framing of the stream. The transport authenticates
/// every byte it carries, so bytes cannot be truncated or mangled on the way:
/// a message that stops part way through stopped because the neighbour stopped
/// it there.
#[derive(Debug)]
pub struct FramingViolationError(io::Error);

impl Display for FramingViolationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "The stream broke the connection's framing: {}", self.0)
    }
}

impl error::Error for FramingViolationError {}

impl From<FramingViolationError> for io::Error {
    fn from(error: FramingViolationError) -> Self {
        error.0
    }
}

/// End a stream cleanly, so that the neighbour reading it sees the end of the
/// stream rather than a reset.
///
/// That distinction is the whole of what makes a framing violation
/// attributable.
pub(crate) async fn flush_and_close_stream(mut stream: Stream) {
    drop(stream.flush().await);
    drop(stream.close().await);
}

/// Read one message of `message_size` bytes from the stream.
///
/// Returns `Ok(None)` when the stream ends before any byte of the to-be-read
/// message has arrived, which signals a graceful shutdown by the remote node,
/// for e.g., peering degree enforcement or epoch rotations.
pub(crate) async fn recv_msg<Reader>(
    mut stream: Reader,
    message_size: NonZeroUsize,
) -> Result<Option<(Reader, IncomingMessage)>, FramingViolationError>
where
    Reader: AsyncRead + Unpin,
{
    let mut buf = vec![0; message_size.get()].into_boxed_slice();
    let (first_byte, rest) = buf.split_at_mut(1);

    // If the stream is dropped before any bytes is sent, then it is considered a
    // graceful shutdown and no action is taken.
    let Ok(1..) = stream.read(first_byte).await else {
        return Ok(None);
    };

    // Past the first byte a message is under way. This node finishes one it has
    // started before it closes, so a message that stops here stopped because
    // the neighbour decided to, and is considered an attributable framing
    // violation.
    stream
        .read_exact(rest)
        .await
        .map_err(FramingViolationError)?;
    Ok(Some((stream, buf.into())))
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

    /// A stream that hands over `delivers` bytes and then fails or ends.
    struct Stops {
        delivers: usize,
        with_failure: bool,
    }

    impl AsyncRead for Stops {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<io::Result<usize>> {
            if self.delivers == 0 {
                return Poll::Ready(if self.with_failure {
                    // What a stream reset looks like from the reading end, and
                    // so what an ungraceful close looks like.
                    Err(io::Error::from(io::ErrorKind::ConnectionReset))
                } else {
                    Ok(0)
                });
            }
            let handed_over = self.delivers.min(buf.len());
            self.delivers -= handed_over;
            Poll::Ready(Ok(handed_over))
        }
    }

    /// Before any byte of a message has arrived there is nothing framed to
    /// violate, so neither the end of the stream nor a failure on it is a
    /// fault. Closing is something the protocol asks nodes to do — at every
    /// epoch rotation among other times — and a node that read its neighbour's
    /// close as a fault would exclude it for `W`, both ways, across the network
    /// at once.
    #[tokio::test]
    async fn a_stream_that_ends_before_a_message_begins_is_not_a_fault() {
        for with_failure in [false, true] {
            let outcome = recv_msg(
                Stops {
                    delivers: 0,
                    with_failure,
                },
                MESSAGE_SIZE,
            )
            .await;

            assert!(
                matches!(outcome, Ok(None)),
                "a stream ending before a message began was read as a fault \
                 (with_failure: {with_failure})"
            );
        }
    }

    /// Past the first byte a message is under way, and a node finishes one it
    /// has started before closing — so a message that stops here stopped
    /// because the neighbour stopped it there.
    #[tokio::test]
    async fn a_message_that_stops_part_way_through_is_a_fault() {
        for with_failure in [false, true] {
            let outcome = recv_msg(
                Stops {
                    delivers: MESSAGE_SIZE.get() - 1,
                    with_failure,
                },
                MESSAGE_SIZE,
            )
            .await;

            assert!(
                outcome.is_err(),
                "a message that stopped part way through was not attributed to its sender \
                 (with_failure: {with_failure})"
            );
        }
    }

    #[tokio::test]
    async fn a_whole_message_is_handed_over() {
        let outcome = recv_msg(
            Stops {
                delivers: MESSAGE_SIZE.get(),
                with_failure: false,
            },
            MESSAGE_SIZE,
        )
        .await;

        assert!(matches!(outcome, Ok(Some(_))));
    }
}
