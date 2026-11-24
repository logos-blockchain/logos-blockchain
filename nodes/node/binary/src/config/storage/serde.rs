use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The version of the layout of everything the node persists in its DB: the
/// blocks and the state its services save to recover from.
///
/// The DB folder is named after it, so a node whose layout differs from the
/// one that wrote the DB starts on an empty DB and syncs again, rather than
/// misreading what is there. Bump it with any change to what is persisted or
/// how it is encoded. Version 1 is the unversioned folder of the releases up
/// to 0.3.6.
pub const STATE_VERSION: u32 = 2;

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub backend: RocksDbSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RocksDbSettings {
    /// Name of the DB state folder, relative to the state path, which is
    /// provided as a separate config entry. The node suffixes it with the
    /// version of the state it persists: see [`Self::db_path`].
    pub folder_name: String,
    pub read_only: bool,
    pub column_family: Option<String>,
}

impl RocksDbSettings {
    /// The DB folder of this build under `state_folder`: `folder_name`,
    /// suffixed with [`STATE_VERSION`].
    #[must_use]
    pub fn db_path(&self, state_folder: &Path) -> PathBuf {
        versioned(&state_folder.join(&self.folder_name), STATE_VERSION)
    }

    /// The DB folders of the earlier state versions under `state_folder`,
    /// which this build does not read.
    #[must_use]
    pub fn earlier_db_paths(&self, state_folder: &Path) -> Vec<PathBuf> {
        let unversioned = state_folder.join(&self.folder_name);
        (1..STATE_VERSION)
            .map(|version| versioned(&unversioned, version))
            .collect()
    }
}

/// `path` with its last component suffixed with `-v<version>`, or as it is for
/// version 1, whose folder was not versioned.
fn versioned(path: &Path, version: u32) -> PathBuf {
    if version == 1 {
        return path.to_path_buf();
    }
    path.file_name().map_or_else(
        || path.join(format!("v{version}")),
        |name| {
            let mut name = name.to_os_string();
            name.push(format!("-v{version}"));
            path.with_file_name(name)
        },
    )
}

impl Default for RocksDbSettings {
    fn default() -> Self {
        Self {
            column_family: Some("blocks".to_owned()),
            folder_name: "./db".to_owned(),
            read_only: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(folder_name: &str) -> RocksDbSettings {
        RocksDbSettings {
            folder_name: folder_name.to_owned(),
            ..RocksDbSettings::default()
        }
    }

    #[test]
    fn the_db_folder_is_named_after_the_state_version() {
        let state = Path::new("state");
        let expected = state.join(format!("db-v{STATE_VERSION}"));
        for folder_name in ["db", "./db", "db/"] {
            let db_path = settings(folder_name).db_path(state);
            assert_eq!(
                db_path.components().collect::<PathBuf>(),
                expected,
                "{folder_name}"
            );
        }
        assert_eq!(
            settings("/var/lib/node/db").db_path(state),
            Path::new(&format!("/var/lib/node/db-v{STATE_VERSION}"))
        );
    }

    #[test]
    fn the_db_folders_of_earlier_versions_start_with_the_unversioned_one() {
        let earlier = settings("db").earlier_db_paths(Path::new("state"));
        assert_eq!(earlier.len(), usize::try_from(STATE_VERSION).unwrap() - 1);
        assert_eq!(earlier[0], Path::new("state/db"));
        assert!(!earlier.contains(&settings("db").db_path(Path::new("state"))));
    }
}
