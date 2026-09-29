use std::collections::HashMap;

use color_eyre::eyre::{Result, eyre};
use lb_key_management_system_service::backend::preload::KeyId;
use serde_yaml::{Mapping, Value};

use crate::cli::config::keystore::{KeyTitle, Keystore};

/// Whether the user config is the one of a node that has an HD wallet
pub fn is_upgraded(user_config: &Value) -> bool {
    get(user_config, &["kms", "backend", "mnemonic"]).is_some()
}

/// Upgrades the user config of a node that has no HD wallet.
///
/// The keys are known under their titles, and the ones that pay fees and
/// receive rewards are not configured anymore.
pub fn upgrade(
    user_config: &mut Value,
    keystore: &Keystore,
    key_titles: &HashMap<KeyId, KeyTitle>,
) -> Result<()> {
    set(
        user_config,
        &["kms", "backend"],
        serde_yaml::to_value(keystore.kms_backend_settings())?,
    )?;

    set(
        user_config,
        &["wallet", "known_keys"],
        serde_yaml::to_value(keystore.wallet_key_ids())?,
    )?;
    remove(user_config, &["wallet", "voucher_master_key_id"]);

    for path in [
        ["blend", "non_ephemeral_signing_key_id"].as_slice(),
        ["blend", "core", "zk", "secret_key_kms_id"].as_slice(),
    ] {
        let key_id = get(user_config, path)
            .and_then(Value::as_str)
            .ok_or_else(|| eyre!("'{}' is not set.", path.join(".")))?;
        let title = key_titles
            .get(key_id)
            .ok_or_else(|| eyre!("The key '{key_id}' is not in the keystore."))?;
        set(user_config, path, Value::String(title.0.clone()))?;
    }

    remove(
        user_config,
        &["cryptarchia", "leader", "wallet", "funding_pk"],
    );
    remove(user_config, &["sdp", "wallet", "funding_pk"]);

    if let Some(targets) = remove(user_config, &["pow", "auto_claim", "targets"]) {
        set(
            user_config,
            &["pow", "auto_claim", "threshold"],
            auto_claim_threshold(&targets),
        )?;
    }

    Ok(())
}

/// The balance that the wallet has to reach for the node to stop claiming,
/// which is the highest one that a target had to reach.
fn auto_claim_threshold(targets: &Value) -> Value {
    targets
        .as_sequence()
        .into_iter()
        .flatten()
        .filter_map(|target| target.get("threshold")?.as_u64())
        .max()
        .map_or(Value::Null, Value::from)
}

fn get<'value>(value: &'value Value, path: &[&str]) -> Option<&'value Value> {
    path.iter().try_fold(value, |value, key| value.get(*key))
}

/// Sets the value at the path, of which all but the last key have to exist.
fn set(value: &mut Value, path: &[&str], new_value: Value) -> Result<()> {
    let (last, parents) = path.split_last().expect("Path is not empty");
    let parent = parents
        .iter()
        .try_fold(value, |value, key| value.get_mut(*key))
        .and_then(Value::as_mapping_mut)
        .ok_or_else(|| eyre!("'{}' is not set.", parents.join(".")))?;
    parent.insert(Value::from(*last), new_value);
    Ok(())
}

fn remove(value: &mut Value, path: &[&str]) -> Option<Value> {
    let (last, parents) = path.split_last().expect("Path is not empty");
    parents
        .iter()
        .try_fold(value, |value, key| value.get_mut(*key))
        .and_then(Value::as_mapping_mut)
        .and_then(|parent: &mut Mapping| parent.remove(*last))
}
