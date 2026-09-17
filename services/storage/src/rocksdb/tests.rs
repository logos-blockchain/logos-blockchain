use std::iter;

use tempfile::TempDir;

use super::{
    handlers::{streamed_immutable_block_ids_reverse_vec, streamed_immutable_block_ids_vec},
    *,
};

#[test]
fn loads_prefix_entries() {
    let directory = TempDir::new().unwrap();
    let settings = RocksBackendSettings {
        db_path: directory.path().into(),
        read_only: false,
        column_family: None,
    };

    let writer = RocksBackend::new(settings.clone()).unwrap();
    writer
        .txn(|database| {
            database.put(b"recovery/one", b"one")?;
            database.put(b"recovery/two", b"two")?;
            database.put(b"unrelated", b"value")?;
            Ok(None)
        })
        .execute()
        .unwrap();
    drop(writer);

    let backend = RocksBackend::new(settings).unwrap();
    let database = Arc::downgrade(&backend.rocks);
    let entries = backend.load_prefix_entries(b"recovery/").unwrap();
    drop(backend);

    assert_eq!(
        entries.get(b"recovery/one".as_slice()),
        Some(&Bytes::from_static(b"one"))
    );
    assert!(database.upgrade().is_none());
    assert_eq!(
        entries.get(b"recovery/two".as_slice()),
        Some(&Bytes::from_static(b"two"))
    );
    assert_eq!(entries.get(b"unrelated".as_slice()), None);
}

#[tokio::test]
async fn test_store_load_remove() -> Result<(), <RocksBackend as StorageBackend>::Error> {
    let temp_path = TempDir::new().unwrap();
    let sled_settings = RocksBackendSettings {
        db_path: temp_path.path().to_path_buf(),
        read_only: false,
        column_family: None,
    };
    let key = "foo";
    let value = "bar";

    let mut db: RocksBackend = RocksBackend::new(sled_settings)?;
    db.store(key.as_bytes().into(), value.as_bytes().into())
        .await?;
    let load_value = db.load(key.as_bytes()).await?;
    assert_eq!(load_value, Some(value.as_bytes().into()));
    let removed_value = db.remove(key.as_bytes()).await?;
    assert_eq!(removed_value, Some(value.as_bytes().into()));

    Ok(())
}

#[tokio::test]
async fn test_load_prefix() {
    let mut backend = RocksBackend::new(RocksBackendSettings {
        db_path: TempDir::new().unwrap().path().to_path_buf(),
        read_only: false,
        column_family: None,
    })
    .unwrap();

    let prefix = b"foo/";

    // No data yet in the backend
    assert!(
        backend
            .load_prefix(prefix, None, None, None)
            .await
            .unwrap()
            .is_empty()
    );

    // No data with the prefix
    backend.store("boo/0".into(), "boo0".into()).await.unwrap();
    backend.store("zoo/0".into(), "zoo0".into()).await.unwrap();
    assert!(
        backend
            .load_prefix(prefix, None, None, None)
            .await
            .unwrap()
            .is_empty()
    );

    // Two data with the prefix
    // (Inserting in mixed order to test the sorted scan).
    backend.store("foo/7".into(), "foo7".into()).await.unwrap();
    backend.store("foo/0".into(), "foo0".into()).await.unwrap();
    assert_eq!(
        backend.load_prefix(prefix, None, None, None).await.unwrap(),
        vec![Bytes::from("foo0"), Bytes::from("foo7")]
    );
}

#[tokio::test]
async fn test_load_prefix_range_limit() {
    let mut backend = RocksBackend::new(RocksBackendSettings {
        db_path: TempDir::new().unwrap().path().to_path_buf(),
        read_only: false,
        column_family: None,
    })
    .unwrap();
    backend.store("boo/0".into(), "boo0".into()).await.unwrap();
    backend.store("foo/0".into(), "foo0".into()).await.unwrap();
    backend.store("foo/1".into(), "foo1".into()).await.unwrap();
    backend.store("foo/3".into(), "foo3".into()).await.unwrap();
    backend.store("foo/4".into(), "foo4".into()).await.unwrap();
    backend.store("foo/9".into(), "foo9".into()).await.unwrap();
    backend.store("zoo/4".into(), "zoo4".into()).await.unwrap();

    // with start_key and end_key that exist
    assert_eq!(
        backend
            .load_prefix(b"foo/", Some(b"1"), Some(b"9"), None)
            .await
            .unwrap(),
        vec![
            Bytes::from("foo1"),
            Bytes::from("foo3"),
            Bytes::from("foo4"),
            Bytes::from("foo9")
        ]
    );

    // with start_key that doesn't exist
    assert_eq!(
        backend
            .load_prefix(b"foo/", Some(b"2"), Some(b"9"), None)
            .await
            .unwrap(),
        vec![
            Bytes::from("foo3"),
            Bytes::from("foo4"),
            Bytes::from("foo9")
        ]
    );

    // with end_key that doesn't exist
    assert_eq!(
        backend
            .load_prefix(b"foo/", Some(b"1"), Some(b"8"), None)
            .await
            .unwrap(),
        vec![
            Bytes::from("foo1"),
            Bytes::from("foo3"),
            Bytes::from("foo4"),
        ]
    );

    // with limit
    assert_eq!(
        backend
            .load_prefix(
                b"foo/",
                Some(b"1"),
                Some(b"9"),
                Some(NonZeroUsize::new(3).unwrap())
            )
            .await
            .unwrap(),
        vec![
            Bytes::from("foo1"),
            Bytes::from("foo3"),
            Bytes::from("foo4"),
        ]
    );
}

#[tokio::test]
async fn test_load_prefix_reverse_range_limit() {
    let mut backend = RocksBackend::new(RocksBackendSettings {
        db_path: TempDir::new().unwrap().path().to_path_buf(),
        read_only: false,
        column_family: None,
    })
    .unwrap();
    backend.store("foo/0".into(), "foo0".into()).await.unwrap();
    backend.store("foo/1".into(), "foo1".into()).await.unwrap();
    backend.store("foo/3".into(), "foo3".into()).await.unwrap();
    backend.store("foo/4".into(), "foo4".into()).await.unwrap();
    backend.store("foo/9".into(), "foo9".into()).await.unwrap();

    assert_eq!(
        backend
            .load_prefix_reverse(b"foo/", Some(b"1"), Some(b"9"), None)
            .await
            .unwrap(),
        vec![
            Bytes::from("foo9"),
            Bytes::from("foo4"),
            Bytes::from("foo3"),
            Bytes::from("foo1")
        ]
    );

    assert_eq!(
        backend
            .load_prefix_reverse(
                b"foo/",
                Some(b"1"),
                Some(b"9"),
                Some(NonZeroUsize::new(2).unwrap())
            )
            .await
            .unwrap(),
        vec![Bytes::from("foo9"), Bytes::from("foo4")]
    );
}

#[tokio::test]
async fn test_transaction() -> Result<(), <RocksBackend as StorageBackend>::Error> {
    let temp_path = TempDir::new().unwrap();

    let sled_settings = RocksBackendSettings {
        db_path: temp_path.path().to_path_buf(),
        read_only: false,
        column_family: None,
    };

    let mut db: RocksBackend = RocksBackend::new(sled_settings)?;
    let txn = db.txn(|db| {
        let key = "foo";
        let value = "bar";
        db.put(key, value)?;
        let result = db.get(key)?;
        db.delete(key)?;
        Ok(result.map(Into::into))
    });
    let result = db.execute(txn).await??;
    assert_eq!(result, Some(b"bar".as_ref().into()));

    Ok(())
}

#[tokio::test]
async fn test_multi_readers_single_writer() -> Result<(), <RocksBackend as StorageBackend>::Error> {
    use tokio::sync::mpsc::channel;

    let temp_path = TempDir::new().unwrap();
    let path = temp_path.path().to_path_buf();
    let sled_settings = RocksBackendSettings {
        db_path: temp_path.path().to_path_buf(),
        read_only: false,
        column_family: None,
    };
    let key = "foo";
    let value = "bar";

    let mut db: RocksBackend = RocksBackend::new(sled_settings)?;

    let (tx, mut rx) = channel(5);
    // now let us spawn a few readers
    for _ in 0..5 {
        let p = path.clone();
        let tx = tx.clone();
        std::thread::spawn(move || {
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(async move {
                    let sled_settings = RocksBackendSettings {
                        db_path: p,
                        read_only: true,
                        column_family: None,
                    };
                    let key = "foo";

                    let mut db: RocksBackend = RocksBackend::new(sled_settings).unwrap();

                    while db.load(key.as_bytes()).await.unwrap().is_none() {
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    }

                    tx.send(()).await.unwrap();
                });
        });
    }

    db.store(key.as_bytes().into(), value.as_bytes().into())
        .await?;

    let mut recvs = 0;
    loop {
        if rx.recv().await.is_some() {
            recvs += 1;
            if recvs == 5 {
                break;
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn store_block_data_stores_block_and_indexes() {
    let temp_dir = TempDir::new().unwrap();
    let mut backend = RocksBackend::new(RocksBackendSettings {
        db_path: temp_dir.path().to_path_buf(),
        read_only: false,
        column_family: None,
    })
    .unwrap();

    let header_id = HeaderId::from([1u8; 32]);
    let parent_id = HeaderId::from([0u8; 32]);
    let block = Bytes::from_static(b"block");
    let events = Bytes::from_static(b"events");
    let immutable_id = HeaderId::from([2u8; 32]);

    backend
        .store_block_data(
            header_id,
            parent_id,
            block.clone(),
            events.clone(),
            [(Slot::new(7), immutable_id)].into(),
        )
        .await
        .unwrap();

    assert_eq!(backend.get_block(header_id).await.unwrap(), Some(block));
    assert_eq!(
        backend.get_block_parent(header_id).await.unwrap(),
        Some(parent_id)
    );
    assert_eq!(
        backend.get_block_events(header_id).await.unwrap(),
        Some(events)
    );
    assert_eq!(
        backend.get_immutable_block_id(Slot::new(7)).await.unwrap(),
        Some(immutable_id)
    );
}

#[tokio::test]
async fn immutable_block_ids() {
    let temp_dir = TempDir::new().unwrap();
    let mut backend = RocksBackend::new(RocksBackendSettings {
        db_path: temp_dir.path().to_path_buf(),
        read_only: false,
        column_family: None,
    })
    .unwrap();

    // Store
    backend
        .store_immutable_block_ids(
            [(0.into(), [0u8; 32].into()), (1.into(), [1u8; 32].into())].into(),
        )
        .await
        .unwrap();

    // Get
    assert_eq!(
        backend.get_immutable_block_id(0.into()).await.unwrap(),
        Some([0u8; 32].into())
    );
    assert_eq!(
        backend.get_immutable_block_id(1.into()).await.unwrap(),
        Some([1u8; 32].into())
    );

    // Scan
    assert_eq!(
        streamed_immutable_block_ids_vec(
            &mut backend,
            RangeInclusive::new(0.into(), 1.into()),
            NonZeroUsize::new(2).unwrap()
        )
        .await
        .unwrap(),
        vec![[0u8; 32].into(), [1u8; 32].into()]
    );
    assert_eq!(
        streamed_immutable_block_ids_vec(
            &mut backend,
            RangeInclusive::new(0.into(), 1.into()),
            NonZeroUsize::new(1).unwrap()
        )
        .await
        .unwrap(),
        vec![[0u8; 32].into()]
    );
    assert_eq!(
        streamed_immutable_block_ids_vec(
            &mut backend,
            RangeInclusive::new(0.into(), 0.into()),
            NonZeroUsize::new(2).unwrap()
        )
        .await
        .unwrap(),
        vec![[0u8; 32].into()]
    );
    assert_eq!(
        streamed_immutable_block_ids_vec(
            &mut backend,
            RangeInclusive::new(1.into(), 2.into()),
            NonZeroUsize::new(2).unwrap()
        )
        .await
        .unwrap(),
        vec![[1u8; 32].into()]
    );

    // Reverse scan
    assert_eq!(
        streamed_immutable_block_ids_reverse_vec(
            &mut backend,
            RangeInclusive::new(0.into(), 1.into()),
            NonZeroUsize::new(2).unwrap()
        )
        .await
        .unwrap(),
        vec![[1u8; 32].into(), [0u8; 32].into()]
    );
    assert_eq!(
        streamed_immutable_block_ids_reverse_vec(
            &mut backend,
            RangeInclusive::new(0.into(), 1.into()),
            NonZeroUsize::new(1).unwrap()
        )
        .await
        .unwrap(),
        vec![[1u8; 32].into()]
    );
}

#[tokio::test]
async fn test_transaction_basic_flow() {
    let temp_dir = TempDir::new().unwrap();
    let mut backend = RocksBackend::new(RocksBackendSettings {
        db_path: temp_dir.path().to_path_buf(),
        read_only: false,
        column_family: None,
    })
    .unwrap();

    let tx_hash = TxHash::default();
    let tx_bytes = Bytes::from(vec![0x01, 0x02, 0x03]);

    let mut transactions = HashMap::new();
    transactions.insert(tx_hash, tx_bytes.clone());
    backend.store_transactions(transactions).await.unwrap();

    let retrieved_stream = backend.get_transactions(iter::once(tx_hash).collect());
    let retrieved: Vec<_> = retrieved_stream.collect().await;
    assert_eq!(retrieved, vec![tx_bytes]);

    backend.remove_transactions(&[tx_hash]).await.unwrap();

    let empty_stream = backend.get_transactions(iter::once(tx_hash).collect());
    let empty: Vec<_> = empty_stream.collect().await;
    assert!(empty.is_empty());
}
