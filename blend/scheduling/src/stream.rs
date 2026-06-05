use futures::StreamExt as _;

/// A stream wrapper that consumes the first item from the underlying stream.
///
/// Instead of implementing [`futures::Stream`], this provides only
/// [`Self::first`] that explicitly reads the first item and returns it together
/// with the remaining stream. It waits for the first item for as long as
/// necessary: blend membership and the slot clock only become available at
/// genesis, which may be arbitrarily far in the future, so there is no timeout.
pub struct UninitializedFirstReadyStream<Stream> {
    stream: Stream,
}

impl<Stream> UninitializedFirstReadyStream<Stream> {
    pub const fn new(stream: Stream) -> Self {
        Self { stream }
    }
}

impl<Stream> UninitializedFirstReadyStream<Stream>
where
    Stream: futures::Stream + Unpin,
{
    /// Yields the first item from the underlying stream, awaiting it for as
    /// long as necessary. The remaining stream is also returned for
    /// continued use.
    ///
    /// Returns [`FirstReadyStreamError::StreamClosed`] if the underlying stream
    /// ends before yielding any item.
    pub async fn first(mut self) -> Result<(Stream::Item, Stream), FirstReadyStreamError> {
        let item = self
            .stream
            .next()
            .await
            .ok_or(FirstReadyStreamError::StreamClosed)?;
        Ok((item, self.stream))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FirstReadyStreamError {
    #[error("The underlying stream was closed before yielding the first item")]
    StreamClosed,
}
