//! Checks every [`KvStore`] implementation must pass. Each check runs on a
//! fresh store from `make`.

use crate::{Bytes, KvEntry, KvStore, WriteBatch};

/// Runs every check against stores built by `make`, panicking on the first
/// failure.
pub fn run<S: KvStore>(make: impl Fn() -> S) {
    get_returns_the_last_write(&make());
    batch_applies_in_order(&make());
    get_prev_stays_within_prefix(&make());
    get_prev_handles_bounds(&make());
    scan_prefix_is_ordered_and_bounded(&make());
    delete_prefix_removes_only_the_range(&make());
    delete_prefix_handles_max_bytes(&make());
}

fn entry(key: &[u8], value: &[u8]) -> KvEntry {
    KvEntry {
        key: Bytes::copy_from_slice(key),
        value: Bytes::copy_from_slice(value),
    }
}

fn put_all<S: KvStore>(store: &S, entries: &[(&[u8], &[u8])]) {
    let mut batch = WriteBatch::new();
    for (key, value) in entries {
        batch.put(Bytes::copy_from_slice(key), Bytes::copy_from_slice(value));
    }
    store.write_batch(batch).expect("batch is written");
}

fn get_returns_the_last_write<S: KvStore>(store: &S) {
    assert_eq!(store.get(b"missing").unwrap(), None);
    put_all(store, &[(b"k", b"1")]);
    put_all(store, &[(b"k", b"2")]);
    assert_eq!(store.get(b"k").unwrap(), Some(Bytes::from_static(b"2")));
    let mut batch = WriteBatch::new();
    batch.delete(&b"k"[..]).delete(&b"never-written"[..]);
    store.write_batch(batch).unwrap();
    assert_eq!(store.get(b"k").unwrap(), None);
}

fn batch_applies_in_order<S: KvStore>(store: &S) {
    let mut batch = WriteBatch::new();
    batch
        .put(&b"a"[..], &b"1"[..])
        .delete(&b"a"[..])
        .put(&b"b"[..], &b"1"[..])
        .put(&b"b"[..], &b"2"[..])
        .put(&b"p/x"[..], &b"1"[..])
        .delete_prefix(&b"p/"[..])
        .put(&b"p/y"[..], &b"1"[..]);
    store.write_batch(batch).unwrap();
    assert_eq!(store.get(b"a").unwrap(), None);
    assert_eq!(store.get(b"b").unwrap(), Some(Bytes::from_static(b"2")));
    assert_eq!(store.get(b"p/x").unwrap(), None);
    assert_eq!(store.get(b"p/y").unwrap(), Some(Bytes::from_static(b"1")));
    store.write_batch(WriteBatch::new()).unwrap();
}

fn get_prev_stays_within_prefix<S: KvStore>(store: &S) {
    put_all(
        store,
        &[
            (b"a/1", b"x"),
            (b"b/1", b"y"),
            (b"b/3", b"z"),
            (b"c/0", b"w"),
        ],
    );
    assert_eq!(
        store.get_prev(b"b/", b"b/2").unwrap(),
        Some(entry(b"b/1", b"y"))
    );
    assert_eq!(
        store.get_prev(b"b/", b"b/9").unwrap(),
        Some(entry(b"b/3", b"z"))
    );
    // strictly lower
    assert_eq!(
        store.get_prev(b"b/", b"b/3").unwrap(),
        Some(entry(b"b/1", b"y"))
    );
    // nothing below within the prefix, even though `a/1` is lower
    assert_eq!(store.get_prev(b"b/", b"b/1").unwrap(), None);
    assert_eq!(store.get_prev(b"b/", b"b/0").unwrap(), None);
    // a bound past the prefix still finds its greatest key, not `c/0`
    assert_eq!(
        store.get_prev(b"b/", b"c/9").unwrap(),
        Some(entry(b"b/3", b"z"))
    );
    assert_eq!(store.get_prev(b"d/", b"d/9").unwrap(), None);
    // a bound below the prefix
    assert_eq!(store.get_prev(b"b/", b"a").unwrap(), None);
}

fn get_prev_handles_bounds<S: KvStore>(store: &S) {
    put_all(store, &[(&[0x00], b"lo"), (&[0xff, 0xff], b"hi")]);
    assert_eq!(store.get_prev(b"", &[0x00]).unwrap(), None);
    assert_eq!(
        store.get_prev(b"", &[0x01]).unwrap(),
        Some(entry(&[0x00], b"lo"))
    );
    assert_eq!(
        store.get_prev(&[0xff], &[0xff, 0xff, 0x00]).unwrap(),
        Some(entry(&[0xff, 0xff], b"hi"))
    );
    assert_eq!(store.get_prev(b"", b"").unwrap(), None);
}

fn scan_prefix_is_ordered_and_bounded<S: KvStore>(store: &S) {
    put_all(
        store,
        &[
            (b"s/2", b"b"),
            (b"s/1", b"a"),
            (b"s", b"root"),
            (b"t/1", b"c"),
            (b"r/9", b"d"),
        ],
    );
    assert_eq!(
        store.scan_prefix(b"s/").unwrap(),
        vec![entry(b"s/1", b"a"), entry(b"s/2", b"b")]
    );
    assert_eq!(store.scan_prefix(b"u/").unwrap(), vec![]);
    assert_eq!(store.scan_prefix(b"").unwrap().len(), 5);
}

fn delete_prefix_removes_only_the_range<S: KvStore>(store: &S) {
    put_all(
        store,
        &[(b"d/1", b"a"), (b"d/2", b"b"), (b"d", b"c"), (b"e/1", b"d")],
    );
    let mut batch = WriteBatch::new();
    batch.delete_prefix(&b"d/"[..]);
    store.write_batch(batch).unwrap();
    assert_eq!(store.scan_prefix(b"d/").unwrap(), vec![]);
    assert_eq!(store.get(b"d").unwrap(), Some(Bytes::from_static(b"c")));
    assert_eq!(store.get(b"e/1").unwrap(), Some(Bytes::from_static(b"d")));
}

fn delete_prefix_handles_max_bytes<S: KvStore>(store: &S) {
    put_all(
        store,
        &[
            (&[0xff, 0x01], b"a"),
            (&[0xff, 0xff, 0x02], b"b"),
            (&[0xfe], b"c"),
        ],
    );
    let mut batch = WriteBatch::new();
    batch.delete_prefix(&[0xff][..]);
    store.write_batch(batch).unwrap();
    assert_eq!(store.scan_prefix(&[0xff]).unwrap(), vec![]);
    assert_eq!(store.get(&[0xfe]).unwrap(), Some(Bytes::from_static(b"c")));
}
