use core::{
    pin::Pin,
    task::{Context, Poll},
};

use futures::{Stream, StreamExt as _, stream::Buffered as BufferedStream, task::noop_waker_ref};

/// A stream wrapper that eagerly pre-polls the wrapped stream so that
/// buffered futures begin executing before the first consumer poll.
pub struct Buffered<WrappedStream>
where
    WrappedStream: Stream<Item: Future<Output: Unpin>>,
{
    stream: Pin<Box<BufferedStream<WrappedStream>>>,
    peeked: Option<<WrappedStream::Item as Future>::Output>,
}

impl<WrappedStream> Buffered<WrappedStream>
where
    WrappedStream: Stream<Item: Future<Output: Send + Unpin> + Send> + Send,
{
    /// Creates a new `Buffered` stream that wraps the given `wrapped_stream`
    /// and buffers up to `buffer_size` futures, kicking off their computation
    /// eagerly before the first consumer poll.
    pub fn new(wrapped_stream: WrappedStream, buffer_size: usize) -> Self {
        let mut stream = Box::pin(wrapped_stream.buffered(buffer_size));
        // Pre-poll once to kick off eager computation. `Poll::Pending` means
        // futures are now in-flight with their wakers registered; `Poll::Ready`
        // means an item arrived immediately and is saved so it isn't lost.
        // A no-op waker is sufficient here: we only need to drive the internal
        // buffer forward once; real wakers are registered on subsequent consumer polls.
        let mut cx = Context::from_waker(noop_waker_ref());
        let peeked = match stream.as_mut().poll_next(&mut cx) {
            Poll::Ready(item) => item,
            Poll::Pending => None,
        };

        Self { stream, peeked }
    }
}

impl<WrappedStream> Stream for Buffered<WrappedStream>
where
    WrappedStream: Stream<Item: Future<Output: Unpin> + Send> + Send,
{
    type Item = <WrappedStream::Item as Future>::Output;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if let Some(item) = this.peeked.take() {
            return Poll::Ready(Some(item));
        }
        this.stream.as_mut().poll_next(cx)
    }
}

#[cfg(test)]
mod tests {

    use core::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };
    use std::sync::Arc;

    use futures::{StreamExt as _, stream};
    use tokio::time::sleep;

    use crate::tokio::stream::Buffered;

    async fn async_id(n: usize) -> usize {
        sleep(Duration::from_millis(n.try_into().unwrap())).await;
        n
    }

    #[tokio::test]
    async fn none_when_inner_stream_ends() {
        let base = stream::iter(vec![async_id(1), async_id(2), async_id(3)]);
        let mut buffered = Buffered::new(base, 10);

        assert_eq!(buffered.next().await, Some(1));
        assert_eq!(buffered.next().await, Some(2));
        assert_eq!(buffered.next().await, Some(3));

        // After exhaustion, should return `None`
        assert_eq!(buffered.next().await, None);
    }

    /// Test that buffering happens without polling (i.e., prefetch works).
    #[tokio::test]
    async fn prefetches_up_to_buffer_capacity() {
        let produced = Arc::new(AtomicUsize::new(0));
        let produced_clone = Arc::clone(&produced);

        // A stream that increments a counter every time it's polled
        let base = stream::unfold(0, move |state| {
            let produced = Arc::clone(&produced_clone);
            async move {
                produced.fetch_add(1, Ordering::SeqCst);
                Some((state, state + 1))
            }
        })
        .map(async_id);

        {
            let buffer_size = 5;
            let mut buffered = Buffered::new(base, buffer_size);

            // Wait that the stream pre-buffers the elements without being polled.
            sleep(Duration::from_millis(100)).await;

            let count = produced.load(Ordering::SeqCst);

            // The background task should have filled the buffer
            assert!(
                count >= buffer_size,
                "Expected at least {buffer_size} prefetched items, got {count}",
            );
            // Now consume them and ensure they are immediately available
            for expected in 0..buffer_size {
                let item = buffered.next().await;
                assert_eq!(item, Some(expected));
            }
        }
    }
}
