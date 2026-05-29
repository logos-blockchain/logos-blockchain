use std::{path::PathBuf, sync::LazyLock};

pub static POC_PROVING_KEY_PATH: LazyLock<PathBuf> = LazyLock::new(|| {
    let proving_key_path = PathBuf::from(lbc_poc_sys::artifacts::PROVING_KEY_PATH);
    if proving_key_path.exists() {
        return proving_key_path;
    }
    panic!(
        "Proving key not found at the expected path: {}. Please ensure the proving key is built and available at this location.",
        proving_key_path.display()
    );
});
