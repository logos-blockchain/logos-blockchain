use std::path::Path;

use color_eyre::eyre::{Result, WrapErr as _};
use lb_utils::yaml::{OnUnknownKeys, deserialize_from_value};
use libp2p::PeerId;
use serde_yaml::{Mapping, Value};
use thiserror::Error;

use crate::{
    UserConfig,
    cli::InitArgs,
    config::{
        changed_values, keystore::Keystore, update_api, update_blend, update_cryptarchia,
        update_network, update_sdp, update_state, update_tracing,
    },
};

#[derive(Error, Debug)]
enum InitError {
    #[error("User configuration file exists. Use `--overwrite` to replace it.")]
    UserFileExists,
}

/// Writes a user config holding only the values set through `args`. Everything
/// else is left to the defaults and to the keystore, which is generated unless
/// it already exists.
pub fn run(mut args: InitArgs) -> Result<()> {
    if args.output.exists() && !args.overwrite {
        return Err(InitError::UserFileExists.into());
    }
    let output_dir = args.output.parent().unwrap_or_else(|| Path::new("."));
    let keystore_path = output_dir.join(&args.keystore);
    args.keystore = keystore_path
        .strip_prefix(output_dir)
        .map_or_else(|_| keystore_path.clone(), Path::to_path_buf);
    let existing_keystore = keystore_path.is_file();
    let keystore = if existing_keystore {
        Keystore::load(&keystore_path)?
    } else {
        Keystore::default()
    };

    let defaults: UserConfig =
        deserialize_from_value(keystore.config_values()?, OnUnknownKeys::Fail)?;
    let mut config = defaults.clone();
    let output = args.output.clone();
    apply(&mut config, args)?;

    let overrides = changed_values(
        &serde_yaml::to_value(&defaults)?,
        serde_yaml::to_value(&config)?,
    )
    .unwrap_or_else(|| Value::Mapping(Mapping::new()));
    // Nothing is written before every override has been validated.
    if !existing_keystore {
        write_creating_parent_dirs(
            &keystore_path,
            &serde_yaml::to_string(&keystore)?,
            "keystore",
        )?;
    }
    write_creating_parent_dirs(&output, &serde_yaml::to_string(&overrides)?, "user config")?;

    Ok(())
}

/// Writes `contents` to `path`, creating any missing parent directories, and
/// names the offending path if either step fails.
fn write_creating_parent_dirs(path: &Path, contents: &str, description: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .wrap_err_with(|| format!("creating {description} directory {}", parent.display()))?;
    }
    std::fs::write(path, contents)
        .wrap_err_with(|| format!("writing {description} to {}", path.display()))
}

fn apply(config: &mut UserConfig, args: InitArgs) -> Result<()> {
    let InitArgs {
        keystore,
        log,
        network,
        blend,
        cryptarchia,
        sdp,
        api,
        state,
        storage_path,
        ..
    } = args;

    config.kms.keystore = keystore;
    if let Some(storage_path) = storage_path {
        config.storage.backend.folder_name = storage_path.to_string_lossy().into_owned();
    }
    if !cryptarchia.skip_ibd
        && let Some(initial_peers) = &network.initial_peers
    {
        config.cryptarchia.network.bootstrap.ibd.peers = initial_peers
            .iter()
            .filter_map(|addr| match addr.iter().last() {
                Some(lb_libp2p::Protocol::P2p(bytes)) => PeerId::from_multihash(bytes.into()).ok(),
                _ => None,
            })
            .collect();
    }
    update_tracing(&mut config.tracing, log)?;
    update_network(&mut config.network, network)?;
    update_blend(&mut config.blend, blend);
    update_cryptarchia(&mut config.cryptarchia, cryptarchia);
    update_sdp(&mut config.sdp, sdp);
    update_api(&mut config.api, api);
    update_state(&mut config.state, state);

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;

    fn init_args(output: PathBuf) -> InitArgs {
        InitArgs {
            output,
            ..InitArgs::default()
        }
    }

    #[test]
    fn run_creates_missing_output_directories() {
        let temp_dir = TempDir::new().unwrap();
        let output_dir = temp_dir.path().join("missing").join("nested");
        let output = output_dir.join("user_config.yaml");

        run(init_args(output.clone())).unwrap();

        assert!(output.is_file());
        assert!(output_dir.join("keystore.yaml").is_file());
    }

    #[test]
    fn run_error_names_the_unwritable_output_path() {
        let temp_dir = TempDir::new().unwrap();
        let blocking_file = temp_dir.path().join("not_a_directory");
        std::fs::write(&blocking_file, "").unwrap();
        let output = blocking_file.join("user_config.yaml");

        let error = run(init_args(output)).unwrap_err();

        assert!(
            format!("{error:?}").contains(&blocking_file.display().to_string()),
            "error should name the offending path, got: {error:?}"
        );
    }
}
