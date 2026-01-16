use std::{path::PathBuf, sync::LazyLock};

const CIRCUIT_NAME: &str = "zksign";

pub static ZKSIGN_PROVING_KEY_PATH: LazyLock<PathBuf> =
    LazyLock::new(|| logos_blockchain_circuits_utils::proving_key_path(CIRCUIT_NAME));
