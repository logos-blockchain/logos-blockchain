use core::time::Duration;

use futures::StreamExt as _;
use tokio::time::timeout;

/// A stream wrapper that expects the underlying stream to yield its first item
/// within the given timeout.
///
/// Instead of implementing [`futures::Stream`], this provides only
/// [`Self::first`] that explicitly tries to read the first item and returns a
/// result.
pub struct UninitializedFirstReadyStream<Stream> {
    stream: Stream,
    timeout: Duration,
}

impl<Stream> UninitializedFirstReadyStream<Stream> {
    pub const fn new(stream: Stream, timeout: Duration) -> Self {
        Self { stream, timeout }
    }
}

impl<Stream> UninitializedFirstReadyStream<Stream>
where
    Stream: futures::Stream + Unpin,
{
    /// Yields the first item if it is yielded within the timeout from the
    /// underlying stream.
    /// The remaining stream is also returned for continued use.
    pub async fn first(mut self) -> Result<(Stream::Item, Stream), FirstReadyStreamError> {
        let item = timeout(self.timeout, self.stream.next())
            .await
            .map_err(|_| FirstReadyStreamError::FirstItemNotReady)?
            .ok_or(FirstReadyStreamError::StreamClosed)?;
        Ok((item, self.stream))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FirstReadyStreamError {
    #[error("The first item was not yielded in time")]
    FirstItemNotReady,
    #[error("The underlying stream was closed before yielding the first item")]
    StreamClosed,
}
