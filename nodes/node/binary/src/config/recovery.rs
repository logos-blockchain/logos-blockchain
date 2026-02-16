use std::path::{Path, PathBuf};

const RECOVERY_FOLDER_NAME: &str = "recovery";

pub fn get_path_for_base_folder_and_file_name(base_folder: &Path, file_name: &Path) -> PathBuf {
    base_folder.join(RECOVERY_FOLDER_NAME).join(file_name)
}
