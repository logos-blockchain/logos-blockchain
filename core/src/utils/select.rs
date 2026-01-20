use futures::{Stream, StreamExt as _};

pub fn select_from_till_fill_size<'i, const SIZE: usize, T>(
    mut measure: impl FnMut(&T) -> usize + 'i,
    items: impl Iterator<Item = T> + 'i,
) -> impl Iterator<Item = T> + 'i {
    let mut current_size = 0usize;
    items.take_while(move |item: &T| {
        current_size += measure(item);
        current_size <= SIZE
    })
}

pub fn select_from_till_fill_size_stream<'i, const SIZE: usize, T>(
    mut measure: impl FnMut(&T) -> usize + 'i + Send,
    items: impl Stream<Item = T> + Send + 'i,
) -> impl Stream<Item = T> + Send + 'i
where
    T: Send,
{
    let mut current_size = 0usize;
    items.take_while(move |item: &T| {
        current_size += measure(item);
        futures::future::ready(current_size <= SIZE)
    })
}
