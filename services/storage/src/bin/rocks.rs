use rocksdb::{DB, Options};

const TEMP_ROCKS_PATH: &str = "rocks";

pub fn rocksdb_ro() {
    let mut opts = Options::default();
    opts.create_if_missing(true);

    // open in read only mode
    let db = DB::open_cf_for_read_only(&opts, TEMP_ROCKS_PATH, ["blocks", "da"], false).unwrap();

    let blocks_cf = db.cf_handle("blocks").unwrap();
    let r = db.get_cf(blocks_cf, b"block1").unwrap().unwrap();

    assert_eq!(r, b"block1data");

    let da_cf = db.cf_handle("da").unwrap();
    let r = db.get_cf(da_cf, b"da1").unwrap().unwrap();
    assert_eq!(r, b"da1data");

    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

pub fn rocksdb_rw() {
    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.create_missing_column_families(true);

    let db = DB::open_cf(&opts, TEMP_ROCKS_PATH, ["blocks", "da"]).unwrap();

    // open blocks column family and insert a block
    let blocks_cf = db.cf_handle("blocks").unwrap();
    db.put_cf(blocks_cf, b"block1", b"block1data").unwrap();

    // open da column family and insert a blob
    let da_cf = db.cf_handle("da").unwrap();
    db.put_cf(da_cf, b"da1", b"da1data").unwrap();

    // A loop to mock a long running program
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

fn main() {
    let mut args = std::env::args();
    args.next();
    let o = args.next();
    if o.is_none() {
        println!("open in read-write mode");
        rocksdb_rw();
    } else {
        println!("open in read-only mode");
        rocksdb_ro();
    }
}
