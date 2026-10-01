use lb_groth16::fr_to_bytes;
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_wallet_service::hd;

use super::*;

const USER_CONFIG: &str = include_str!("user_config.yaml");
const KEYSTORE: &str = include_str!("keystore.yaml");

const MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

const BLEND_SIGNING: &str = "aa70aafc48536ae13168ed4845981a40cbd2dc1c38df88d04c46250e9ad65ce0";
const BLEND_ZK: &str = "852efb444db8c3c811625850df39425f43aeffc69571192c0be9f72523256e0a";
const STAKE: &str = "e3635f207984ae779cf76b5f20714b514373f61ff96260879fe0a6d71f2dce07";

#[test]
fn user_config_of_v0_3_0_is_migrated() {
    let old_config: UserConfigV0_3_0 = serde_yaml::from_str(USER_CONFIG).unwrap();
    let old_keystore: KeystoreV0_3_0 = serde_yaml::from_str(KEYSTORE).unwrap();
    let keystore = migrate_keystore(
        old_keystore,
        MNEMONIC.parse().unwrap(),
        Some("passphrase".into()),
    );

    let config =
        migrate_user_config(serde_yaml::from_str(USER_CONFIG).unwrap(), &keystore).unwrap();

    assert_eq!(config.kms.backend.mnemonic, MNEMONIC.parse().unwrap());
    assert!(config.kms.backend.passphrase.is_some());
    assert_eq!(config.kms.backend.static_keys, old_config.kms.backend.keys);
    assert_eq!(config.wallet.static_keys, old_config.wallet.known_keys);
    // The stake address is derived with the passphrase, as the keystore does.
    let stake = config
        .kms
        .backend
        .derive_key(&hd::receive_path(hd::STAKE_RECEIVE_INDEX))
        .to_zk_key()
        .to_public_key();
    assert_ne!(stake, self::keystore().stake_public_key());
    assert_eq!(
        config.wallet.unspendable_keys,
        [stake, public_key(STAKE)].into()
    );
    assert_eq!(
        config.blend.non_ephemeral_signing_key_id,
        KeyId::from(BLEND_SIGNING)
    );
    assert_eq!(
        config.blend.core.zk.secret_key_kms_id,
        KeyId::from(BLEND_ZK)
    );
    config.blend_provider_id().unwrap();
    assert_eq!(config.blend_zk_key().unwrap().1, public_key(BLEND_ZK));

    // The settings of v0.3.0 that have no role anymore are dropped.
    let yaml = serde_yaml::to_string(&config).unwrap();
    assert!(!yaml.contains("funding_pk"));
    assert!(!yaml.contains("known_keys"));
    assert!(!yaml.contains("voucher_master_key_id"));
}

#[test]
fn keystore_of_v0_3_0_keeps_its_keys() {
    let old_keystore: KeystoreV0_3_0 = serde_yaml::from_str(KEYSTORE).unwrap();

    let keystore = keystore();

    for (title, key) in &old_keystore.secret_keys {
        let title = match title.0.as_str() {
            "LeaderFunding" => "LegacyLeaderFunding",
            "PoWClaim" => "LegacyPoWClaim",
            "SdpFunding" => "LegacySdpFunding",
            "Stake" => "LegacyStake",
            "VaucherMaster" => "LegacyVoucherMaster",
            title => title,
        };
        let (_, migrated) = keystore.get_static_key(title).unwrap();
        assert_eq!(migrated, key);
    }
    for title in [
        "LeaderFunding",
        "PoWClaim",
        "SdpFunding",
        "Stake",
        "VaucherMaster",
    ] {
        assert!(keystore.get_static_key(title).is_none());
    }
    assert_eq!(keystore.legacy_stake_public_key(), Some(public_key(STAKE)));
}

#[tokio::test]
async fn files_are_migrated_and_kept() {
    let dir = std::env::temp_dir().join(format!("migrate_0_3_0_{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let user_config_path = dir.join("user_config.yaml");
    let keystore_path = dir.join("keystore.yaml");
    // The DB is in a folder that does not exist, as for a node that never ran.
    let user_config = USER_CONFIG.replace(
        "base_folder: ./state",
        &format!("base_folder: {}", dir.join("state").display()),
    );
    fs::write(&user_config_path, &user_config).unwrap();
    fs::write(&keystore_path, KEYSTORE).unwrap();

    run(MigrateArgs {
        user_config: user_config_path.clone(),
        keystore: keystore_path.clone(),
        mnemonic: Some(MNEMONIC.parse().unwrap()),
        mnemonic_passphrase: None,
        auto_approve: true,
    })
    .await
    .unwrap();

    let config: UserConfig =
        serde_yaml::from_str(&fs::read_to_string(&user_config_path).unwrap()).unwrap();
    assert_eq!(config.wallet.unspendable_keys, unspendable_keys());
    let keystore: Keystore =
        serde_yaml::from_str(&fs::read_to_string(&keystore_path).unwrap()).unwrap();
    assert_eq!(keystore.legacy_stake_public_key(), Some(public_key(STAKE)));
    assert_eq!(
        fs::read_to_string(dir.join("user_config.yaml.v0.3.0")).unwrap(),
        user_config
    );
    assert_eq!(
        fs::read_to_string(dir.join("keystore.yaml.v0.3.0")).unwrap(),
        KEYSTORE
    );

    // The files of v0.3.0 are overwritten, so they are not migrated again.
    let result = run(MigrateArgs {
        user_config: user_config_path,
        keystore: keystore_path,
        mnemonic: None,
        mnemonic_passphrase: None,
        auto_approve: true,
    })
    .await;
    assert!(result.is_err());

    fs::remove_dir_all(dir).unwrap();
}

/// The fields of the user config of v0.3.0 that the migration keeps
#[derive(Deserialize)]
struct UserConfigV0_3_0 {
    kms: KmsConfigV0_3_0,
    wallet: WalletConfigV0_3_0,
}

#[derive(Deserialize)]
struct KmsConfigV0_3_0 {
    backend: KmsBackendV0_3_0,
}

#[derive(Deserialize)]
struct KmsBackendV0_3_0 {
    keys: HashMap<String, Key>,
}

#[derive(Deserialize)]
struct WalletConfigV0_3_0 {
    known_keys: HashMap<String, ZkPublicKey>,
}

/// The first receive address of the new mnemonic, and the stake key of v0.3.0
fn unspendable_keys() -> std::collections::HashSet<ZkPublicKey> {
    [
        keystore().receive_public_key(hd::STAKE_RECEIVE_INDEX),
        public_key(STAKE),
    ]
    .into()
}

fn keystore() -> Keystore {
    let old_keystore: KeystoreV0_3_0 = serde_yaml::from_str(KEYSTORE).unwrap();
    migrate_keystore(old_keystore, MNEMONIC.parse().unwrap(), None)
}

fn public_key(hex: &str) -> ZkPublicKey {
    let public_key: ZkPublicKey = serde_yaml::from_value(Value::String(hex.to_owned())).unwrap();
    assert_eq!(hex::encode(fr_to_bytes(public_key.as_fr())), hex);
    public_key
}
