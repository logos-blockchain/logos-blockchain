use lb_binary_codec::bincode::SerializeOp as _;
use lb_key_management_system_service::keys::Key;
use lb_storage_service::{
    backend::StorageBackend as _,
    recovery::recovery_key,
    rocksdb::{RocksBackend, RocksBackendSettings},
};
use tempfile::TempDir;

use super::*;
use crate::cli::config::keystore::{KeyTitle, Keystore};

const LEGACY_USER_CONFIG: &str = include_str!("fixtures/legacy_user_config.yaml");
const LEGACY_KEYSTORE: &str = include_str!("fixtures/legacy_keystore.yaml");
// Test vector of the spec
const MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

/// The files of a node that has no HD wallet, whose state is in the directory
struct Node {
    directory: TempDir,
}

impl Node {
    fn new() -> Self {
        let directory = TempDir::new().unwrap();
        let mut user_config: Value = serde_yaml::from_str(LEGACY_USER_CONFIG).unwrap();
        user_config["state"]["base_folder"] =
            Value::from(directory.path().join("state").to_str().unwrap());
        let node = Self { directory };
        std::fs::write(
            node.user_config_path(),
            serde_yaml::to_string(&user_config).unwrap(),
        )
        .unwrap();
        std::fs::write(node.keystore_path(), LEGACY_KEYSTORE).unwrap();
        node
    }

    fn user_config_path(&self) -> PathBuf {
        self.directory.path().join("user_config.yaml")
    }

    fn keystore_path(&self) -> PathBuf {
        self.directory.path().join("keystore.yaml")
    }

    fn database_settings(&self) -> RocksBackendSettings {
        RocksBackendSettings {
            db_path: self.directory.path().join("state").join("db"),
            read_only: false,
            column_family: Some("blocks".to_owned()),
        }
    }

    fn upgrade(&self) {
        run(UpgradeArgs::new(
            self.user_config_path(),
            self.keystore_path(),
            Some(MNEMONIC.parse().unwrap()),
            None,
        ))
        .unwrap();
    }

    fn user_config(&self) -> UserConfig {
        serde_yaml::from_str(&read(&self.user_config_path())).unwrap()
    }

    fn keystore(&self) -> Keystore {
        serde_yaml::from_str(&read(&self.keystore_path())).unwrap()
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

fn legacy_key(title: &str) -> Key {
    let keystore: Value = serde_yaml::from_str(LEGACY_KEYSTORE).unwrap();
    serde_yaml::from_value(keystore["secret_keys"][title].clone()).unwrap()
}

#[test]
fn keys_are_kept_under_their_titles() {
    let node = Node::new();

    node.upgrade();

    let keys = node.user_config().kms.backend.keys;
    assert_eq!(keys.len(), 8);
    for title in [
        "BlendSigning",
        "BlendZk",
        "NetworkSwarm",
        "LeaderFunding",
        "SdpFunding",
        "PoWClaim",
        "Stake",
    ] {
        assert_eq!(keys.get(title), Some(&legacy_key(title)), "{title}");
    }
    assert_eq!(
        keys.get("VoucherMaster"),
        Some(&legacy_key("VaucherMaster"))
    );
    for title in keys.keys() {
        let (key_id, key) = node.keystore().get(KeyTitle::from(title.clone())).unwrap();
        assert_eq!(&key_id, title);
        assert_eq!(keys.get(title), Some(&key));
    }
}

#[test]
fn wallet_knows_the_keys_by_their_titles() {
    let node = Node::new();

    node.upgrade();

    let user_config = node.user_config();
    assert_eq!(
        user_config.wallet.known_keys,
        [
            "BlendSigning",
            "BlendZk",
            "LeaderFunding",
            "PoWClaim",
            "SdpFunding",
            "Stake",
            "VoucherMaster"
        ]
    );
    assert_eq!(
        user_config.blend.non_ephemeral_signing_key_id,
        "BlendSigning"
    );
    assert_eq!(user_config.blend.core.zk.secret_key_kms_id, "BlendZk");
}

#[test]
fn mnemonic_is_the_one_given() {
    let node = Node::new();

    node.upgrade();

    assert_eq!(
        node.user_config().kms.backend.mnemonic,
        MNEMONIC.parse().unwrap()
    );
}

#[test]
fn auto_claim_has_the_threshold_of_its_target() {
    let node = Node::new();

    node.upgrade();

    assert_eq!(node.user_config().pow.auto_claim.threshold, Some(u64::MAX));
}

#[test]
fn other_settings_are_kept() {
    let node = Node::new();
    let mut legacy_user_config: Value =
        serde_yaml::from_str(&read(&node.user_config_path())).unwrap();

    node.upgrade();

    let mut user_config: Value = serde_yaml::from_str(&read(&node.user_config_path())).unwrap();
    for user_config in [&mut legacy_user_config, &mut user_config] {
        let sections = user_config.as_mapping_mut().unwrap();
        for upgraded in ["kms", "wallet", "blend", "cryptarchia", "sdp", "pow"] {
            sections.remove(upgraded).unwrap();
        }
    }
    assert_eq!(user_config, legacy_user_config);
}

#[test]
fn legacy_files_are_backed_up() {
    let node = Node::new();
    let legacy_user_config = read(&node.user_config_path());

    node.upgrade();

    assert_eq!(
        read(&with_suffix(&node.user_config_path(), "bak")),
        legacy_user_config
    );
    assert_eq!(
        read(&with_suffix(&node.keystore_path(), "bak")),
        LEGACY_KEYSTORE
    );
    assert!(!with_suffix(&node.user_config_path(), "new").exists());
    assert!(!with_suffix(&node.keystore_path(), "new").exists());
}

#[test]
fn upgraded_node_is_not_upgraded_again() {
    let node = Node::new();
    node.upgrade();
    let user_config = read(&node.user_config_path());
    let keystore = read(&node.keystore_path());

    node.upgrade();

    assert_eq!(read(&node.user_config_path()), user_config);
    assert_eq!(read(&node.keystore_path()), keystore);
}

#[test]
fn wallet_state_is_upgraded() {
    // A wallet that has neither vouchers nor claims encodes its state as the
    // index of its next voucher, followed by empty collections.
    let legacy_state = (8u64, (0u64, 0u64), None::<u8>, 0u64).to_bytes().unwrap();
    let state = (0u32, 0u32, 0u64, (0u64, 0u64), None::<u8>, 0u64)
        .to_bytes()
        .unwrap();
    let node = Node::new();
    let key = recovery_key(b"wallet");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut database = RocksBackend::new(node.database_settings()).unwrap();
        database.store(key.clone(), legacy_state).await.unwrap();
    });

    node.upgrade();

    runtime.block_on(async {
        let mut database = RocksBackend::new(node.database_settings()).unwrap();
        assert_eq!(database.load(&key).await.unwrap(), Some(state));
    });
}
