use color_eyre::eyre::{Result, eyre};
use lb_storage_service::{
    backend::StorageBackend as _, recovery::recovery_key, rocksdb::RocksBackend,
};
use lb_wallet_service::upgrade_recovery_state;

use crate::{
    UserConfig, cli::upgrade::keystore::voucher_master_title,
    config::storage::ServiceConfig as StorageConfig,
};

/// The key of the recovery state of the wallet, without the prefix of the
/// recovery states
const WALLET_RECOVERY_KEY_SUFFIX: &[u8] = b"wallet";

/// Upgrades the recovery state of the wallet in the database of the node, if
/// there is any.
pub fn upgrade(user_config: &UserConfig) -> Result<()> {
    let settings = StorageConfig {
        user: user_config.storage.clone(),
    }
    .into_rocks_backend_settings(&user_config.state);
    if !settings.db_path.exists() {
        return Ok(());
    }

    let runtime = tokio::runtime::Builder::new_current_thread().build()?;
    runtime.block_on(async {
        let mut database = RocksBackend::new(settings).map_err(|error| {
            eyre!("The database cannot be opened. Is the node stopped? {error}")
        })?;
        let key = recovery_key(WALLET_RECOVERY_KEY_SUFFIX);
        let Some(legacy_state) = database.load(&key).await? else {
            return Ok(());
        };
        let state = upgrade_recovery_state(&legacy_state, &voucher_master_title().into())?;
        database.store(key, state).await?;
        Ok(())
    })
}
