use std::{path::PathBuf, sync::LazyLock};

const CIRCUIT_NAME: &str = "poc";

pub static POC_PROVING_KEY_PATH: LazyLock<PathBuf> =
    LazyLock::new(|| lb_circuits_utils::proving_key_path(CIRCUIT_NAME));
