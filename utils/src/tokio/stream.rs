use core::{
    pin::Pin,
    task::{Context, Poll},
};

use futures::{Stream, StreamExt as _};
use tokio::{sync::mpsc::channel, task::JoinHandle};
use tokio_stream::wrappers::ReceiverStream;

/// A stream wrapper that buffers items from the wrapped stream in a background
/// task before being polled.
pub struct Buffered<WrappedStream>
where
    WrappedStream: Stream,
{
    task_handle: JoinHandle<()>,
    stream: ReceiverStream<WrappedStream::Item>,
}

impl<WrappedStream> Drop for Buffered<WrappedStream>
where
    WrappedStream: Stream,
{
    fn drop(&mut self) {
        self.task_handle.abort();
    }
}

impl<WrappedStream> Buffered<WrappedStream>
where
    WrappedStream: Stream<Item: Send> + Send + 'static,
{
    /// Creates a new `Buffered` stream that wraps the given `wrapped_stream`
    /// and buffers up to `buffer_size` items in a background task and keep them
    /// ready for consumption.
    pub fn new(wrapped_stream: WrappedStream, buffer_size: usize) -> Self {
        let (item_sender, item_receiver) = channel(buffer_size);

        let task_handle = tokio::spawn(async move {
            futures::pin_mut!(wrapped_stream);

            while let Some(item) = wrapped_stream.next().await {
                if item_sender.send(item).await.is_err() {
                    break;
                }
            }
        });

        Self {
            task_handle,
            stream: ReceiverStream::new(item_receiver),
        }
    }
}

impl<WrappedStream> Stream for Buffered<WrappedStream>
where
    WrappedStream: Stream<Item: Send> + Send + 'static,
{
    type Item = WrappedStream::Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.stream.poll_next_unpin(cx)
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

    #[tokio::test]
    async fn none_when_inner_stream_ends() {
        let base = stream::iter(vec![1u8, 2, 3]);
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
        });

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
