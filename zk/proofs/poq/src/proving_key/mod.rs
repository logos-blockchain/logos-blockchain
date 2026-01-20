use std::{path::PathBuf, sync::LazyLock};

const CIRCUIT_NAME: &str = "poq";

pub static POQ_PROVING_KEY_PATH: LazyLock<PathBuf> =
    LazyLock::new(|| lb_circuits_utils::proving_key_path(CIRCUIT_NAME));
