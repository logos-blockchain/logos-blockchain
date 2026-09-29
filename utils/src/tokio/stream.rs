use core::{
    pin::Pin,
    task::{Context, Poll},
};

use futures::{
    Stream, StreamExt as _,
    stream::{BufferUnordered as BufferUnorderedStream, Buffered as BufferedStream},
    task::noop_waker_ref,
};

mod sealed {
    pub trait Sealed {}
}

pub trait BufferType<InputStream>: sealed::Sealed
where
    InputStream: Stream<Item: Future>,
{
    type OutputStream: Stream<Item = <InputStream::Item as Future>::Output>;

    fn buffer_stream(stream: InputStream, buffer_size: usize) -> Self::OutputStream;
}

/// Outputs are yielded in the order their futures were pulled from the
/// source.
#[derive(Debug, Clone, Copy)]
pub struct Ordered;

impl sealed::Sealed for Ordered {}

impl<InputStream> BufferType<InputStream> for Ordered
where
    InputStream: Stream<Item: Future>,
{
    type OutputStream = BufferedStream<InputStream>;

    fn buffer_stream(stream: InputStream, buffer_size: usize) -> Self::OutputStream {
        stream.buffered(buffer_size)
    }
}

/// Outputs are yielded in the order their futures complete.
#[derive(Debug, Clone, Copy)]
pub struct Unordered;

impl sealed::Sealed for Unordered {}

impl<InputStream> BufferType<InputStream> for Unordered
where
    InputStream: Stream<Item: Future>,
{
    type OutputStream = BufferUnorderedStream<InputStream>;

    fn buffer_stream(stream: InputStream, buffer_size: usize) -> Self::OutputStream {
        stream.buffer_unordered(buffer_size)
    }
}

/// A stream wrapper that eagerly pre-polls the wrapped stream so that
/// buffered futures begin executing before the first consumer poll.
///
/// `BufferLogic` picks the adapter, and with it the order in which outputs are
/// yielded: see [`Buffered`] and [`BufferedUnordered`].
pub struct Buffered<WrappedStream, BufferLogic>
where
    WrappedStream: Stream<Item: Future<Output: Unpin>>,
    BufferLogic: BufferType<WrappedStream>,
{
    stream: Pin<Box<BufferLogic::OutputStream>>,
    peeked: Option<<WrappedStream::Item as Future>::Output>,
}

impl<WrappedStream, BufferLogic> Buffered<WrappedStream, BufferLogic>
where
    WrappedStream: Stream<Item: Future<Output: Unpin>>,
    BufferLogic: BufferType<WrappedStream>,
{
    /// Creates a new `Buffered` stream that wraps the given `wrapped_stream`
    /// and buffers up to `buffer_size` futures, kicking off their computation
    /// eagerly before the first consumer poll.
    pub fn new(wrapped_stream: WrappedStream, buffer_size: usize) -> Self {
        let mut stream = Box::pin(BufferLogic::buffer_stream(wrapped_stream, buffer_size));
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

impl<WrappedStream, BufferLogic> Stream for Buffered<WrappedStream, BufferLogic>
where
    WrappedStream: Stream<Item: Future<Output: Unpin>>,
    BufferLogic: BufferType<WrappedStream>,
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

/// An [`Buffered`] stream yielding outputs in source order: see
/// [`Ordered`].
pub type BufferedOrdered<WrappedStream> = Buffered<WrappedStream, Ordered>;

/// An [`Buffered`] stream yielding outputs in completion order: see
/// [`Unordered`].
pub type BufferedUnordered<WrappedStream> = Buffered<WrappedStream, Unordered>;

#[cfg(test)]
mod tests {
    use core::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };
    use std::sync::Arc;

    use futures::{FutureExt as _, Stream, StreamExt as _, stream};
    use tokio::{
        sync::oneshot,
        time::{sleep, timeout},
    };

    use crate::tokio::stream::{BufferedOrdered, BufferedUnordered};

    const BUFFER_SIZE: usize = 5;

    async fn async_id(n: usize) -> usize {
        sleep(Duration::from_millis(n.try_into().unwrap())).await;
        n
    }

    /// Polls the next item, failing rather than hanging the test if the
    /// stream holds it back.
    async fn next_or_fail<S>(stream: &mut S) -> Option<S::Item>
    where
        S: Stream + Unpin + Send,
    {
        timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("Stream should yield its next item rather than hold it back.")
    }

    fn ending_source() -> impl Stream<Item = impl Future<Output = usize>> {
        stream::iter([async_id(1), async_id(2), async_id(3)])
    }

    async fn assert_none_when_source_ends<S>(buffered: &mut S)
    where
        S: Stream<Item = usize> + Unpin + Send,
    {
        let mut items = Vec::new();
        for _ in 0..3 {
            items.push(next_or_fail(buffered).await.unwrap());
        }
        items.sort_unstable();
        assert_eq!(items, [1, 2, 3]);

        // After exhaustion, should return `None`
        assert_eq!(next_or_fail(buffered).await, None);
    }

    #[tokio::test]
    async fn ordered_none_when_source_ends() {
        let mut buffered = BufferedOrdered::new(ending_source(), 10);
        assert_none_when_source_ends(&mut buffered).await;
    }

    #[tokio::test]
    async fn unordered_none_when_source_ends() {
        let mut buffered = BufferedUnordered::new(ending_source(), 10);
        assert_none_when_source_ends(&mut buffered).await;
    }

    /// An endless source that counts, in `produced`, how many futures have
    /// been pulled from it.
    fn counting_source(
        produced: Arc<AtomicUsize>,
    ) -> impl Stream<Item = impl Future<Output = usize>> {
        stream::unfold(0, move |state| {
            let produced = Arc::clone(&produced);
            async move {
                produced.fetch_add(1, Ordering::SeqCst);
                Some((state, state + 1))
            }
        })
        .map(async_id)
    }

    /// Checks that buffering happens without polling (i.e., prefetch works).
    async fn assert_prefetched_up_to_buffer_capacity<S>(buffered: &mut S, produced: &AtomicUsize)
    where
        S: Stream<Item = usize> + Unpin + Send,
    {
        // Wait that the stream pre-buffers the elements without being polled.
        sleep(Duration::from_millis(100)).await;

        let count = produced.load(Ordering::SeqCst);

        // The pre-poll should have filled the buffer
        assert!(
            count >= BUFFER_SIZE,
            "Expected at least {BUFFER_SIZE} prefetched items, got {count}",
        );
        // Now consume them and ensure they are immediately available
        let mut items = Vec::new();
        for _ in 0..BUFFER_SIZE {
            items.push(next_or_fail(buffered).await.unwrap());
        }
        items.sort_unstable();
        assert_eq!(items, (0..BUFFER_SIZE).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn ordered_prefetches_up_to_buffer_capacity() {
        let produced = Arc::new(AtomicUsize::new(0));
        let mut buffered =
            BufferedOrdered::new(counting_source(Arc::clone(&produced)), BUFFER_SIZE);
        assert_prefetched_up_to_buffer_capacity(&mut buffered, &produced).await;
    }

    #[tokio::test]
    async fn unordered_prefetches_up_to_buffer_capacity() {
        let produced = Arc::new(AtomicUsize::new(0));
        let mut buffered =
            BufferedUnordered::new(counting_source(Arc::clone(&produced)), BUFFER_SIZE);
        assert_prefetched_up_to_buffer_capacity(&mut buffered, &produced).await;
    }

    async fn awaited(receiver: oneshot::Receiver<char>) -> char {
        receiver.await.unwrap()
    }

    /// Three futures whose completion the test controls, all in flight at
    /// once.
    fn controlled_source() -> (
        [oneshot::Sender<char>; 3],
        impl Stream<Item = impl Future<Output = char>>,
    ) {
        let (sender_a, receiver_a) = oneshot::channel();
        let (sender_b, receiver_b) = oneshot::channel();
        let (sender_c, receiver_c) = oneshot::channel();
        let source = stream::iter([receiver_a, receiver_b, receiver_c]).map(awaited);
        ([sender_a, sender_b, sender_c], source)
    }

    /// The ordered buffer holds a completed future's output back until every
    /// future pulled before it has completed too.
    #[tokio::test]
    async fn ordered_yields_in_source_order() {
        let ([sender_a, sender_b, sender_c], source) = controlled_source();
        let mut buffered = BufferedOrdered::new(source, 3);

        // `c` is done, but `a` and `b` before it are not, so nothing is yielded.
        sender_c.send('c').unwrap();
        assert_eq!(buffered.next().now_or_never(), None);

        sender_a.send('a').unwrap();
        assert_eq!(next_or_fail(&mut buffered).await, Some('a'));
        // `c` is still held back behind `b`.
        assert_eq!(buffered.next().now_or_never(), None);

        sender_b.send('b').unwrap();
        assert_eq!(next_or_fail(&mut buffered).await, Some('b'));
        assert_eq!(next_or_fail(&mut buffered).await, Some('c'));
        assert_eq!(next_or_fail(&mut buffered).await, None);
    }

    /// The unordered buffer yields each output as soon as its future
    /// completes, whatever position that future had in the source.
    #[tokio::test]
    async fn unordered_yields_in_completion_order() {
        let ([sender_a, sender_b, sender_c], source) = controlled_source();
        let mut buffered = BufferedUnordered::new(source, 3);

        // `c` is done while `a` and `b` before it are not: it is not held back.
        sender_c.send('c').unwrap();
        assert_eq!(next_or_fail(&mut buffered).await, Some('c'));
        // Nothing else has completed yet.
        assert_eq!(buffered.next().now_or_never(), None);

        sender_a.send('a').unwrap();
        assert_eq!(next_or_fail(&mut buffered).await, Some('a'));

        sender_b.send('b').unwrap();
        assert_eq!(next_or_fail(&mut buffered).await, Some('b'));
        assert_eq!(next_or_fail(&mut buffered).await, None);
    }
}
