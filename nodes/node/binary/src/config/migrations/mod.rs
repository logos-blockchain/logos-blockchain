//! Upgrades user configs written by earlier releases to the format of this
//! build.
//!
//! A config carries the version of the build that wrote it in its top-level
//! `version`: the release of that build, or `master` for a build that isn't
//! one. A config without a `version` predates versioning and is read as 0.3.2.
//!
//! Each [`Migration`] rewrites configs older than its `to`, the first release
//! with its format. The last migration may still be unreleased, `to: Master`,
//! which stands for this build, whatever release it turns out to be; cutting a
//! release needs no change to the list. When adding a migration after that
//! release, set the previous one's `to` to the release, so that it stops
//! running on configs of that release and later ones.
//!
//! A migration lives in its own `v<from>` module and only knows the keys and
//! shapes that changed, with a struct where one is needed to read an old
//! shape; every other value passes through untouched.

mod v0_3_2;

use core::{fmt, str::FromStr};
use std::sync::LazyLock;

use color_eyre::eyre::{Result, WrapErr as _, eyre};
use regex::Regex;
use serde_yaml::Value;

/// The top-level key holding a config's version.
pub const VERSION_KEY: &str = "version";

/// The version of the configs written before they carried one.
const UNVERSIONED: ConfigVersion = ConfigVersion::Release(0, 3, 2);

/// The version of a build: the release it is, or `master`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfigVersion {
    Release(u32, u32, u32),
    /// A build that isn't a release: newer than every release.
    Master,
}

/// The workspace version on master, bumped to the release on a release branch.
const UNRELEASED: ConfigVersion = ConfigVersion::Release(0, 0, 0);

impl ConfigVersion {
    /// The version of this build: the release tag at its commit (`0.4.0`, as
    /// the release workflow requires; `0.4.0-rc.1` counts as its release),
    /// else the crate version, which release branches bump to the same, else
    /// `master`. A commit with several tags reports whichever `git describe`
    /// picks.
    #[must_use]
    pub fn current() -> Self {
        let build = lb_version::build_version_info();
        let tagged = build.tag.and_then(|tag| tag.parse().ok());
        match tagged.or_else(|| build.version.parse().ok()) {
            None | Some(UNRELEASED) => Self::Master,
            Some(version) => version,
        }
    }
}

impl fmt::Display for ConfigVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Release(major, minor, patch) => write!(f, "{major}.{minor}.{patch}"),
            Self::Master => f.write_str("master"),
        }
    }
}

impl FromStr for ConfigVersion {
    type Err = color_eyre::Report;

    fn from_str(s: &str) -> Result<Self> {
        /// A release tag as the release workflow accepts them, with the
        /// `-rc.N` of a release candidate.
        static RELEASE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
                r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-rc\.(?:0|[1-9][0-9]*))?$",
            )
            .expect("a valid regex")
        });
        if s == "master" {
            return Ok(Self::Master);
        }
        let (_, [major, minor, patch]) = RELEASE
            .captures(s)
            .ok_or_else(|| {
                eyre!("`{s}` is not a version, expected `<major>.<minor>.<patch>` or `master`")
            })?
            .extract();
        Ok(Self::Release(
            major.parse()?,
            minor.parse()?,
            patch.parse()?,
        ))
    }
}

/// A format change, applied to every config older than `to`.
pub struct Migration {
    /// The first release with the new format, or [`ConfigVersion::Master`]
    /// while unreleased.
    pub to: ConfigVersion,
    pub migrate: fn(&mut Value) -> Result<()>,
}

/// Every migration, oldest first. Only the last may be unreleased.
pub const MIGRATIONS: &[Migration] = &[Migration {
    to: ConfigVersion::Master,
    migrate: v0_3_2::migrate,
}];

/// Upgrades `config` to the format of this build and takes its `version` out.
/// Returns the version it was in if a migration changed it.
pub fn upgrade(config: &mut Value) -> Result<Option<ConfigVersion>> {
    let current = ConfigVersion::current();
    let from = match config.as_mapping_mut().and_then(|c| c.remove(VERSION_KEY)) {
        None => UNVERSIONED,
        Some(version) => version
            .as_str()
            .ok_or_else(|| eyre!("the config `{VERSION_KEY}` is not a string"))?
            .parse()?,
    };
    if from > current {
        return Err(eyre!(
            "the config was written by a newer node, {from}; this one is {current}"
        ));
    }
    let original = config.clone();
    for migration in pending(MIGRATIONS, from, current) {
        (migration.migrate)(config)
            .wrap_err_with(|| format!("upgrading the config from {from} to {}", migration.to))?;
    }
    Ok((*config != original).then_some(from))
}

/// The migrations that bring a config of version `from` to `current`: those
/// first released after `from`, the unreleased one being released by `current`.
fn pending(
    migrations: &[Migration],
    from: ConfigVersion,
    current: ConfigVersion,
) -> impl Iterator<Item = &Migration> {
    migrations
        .iter()
        .filter(move |migration| migration.to.min(current) > from)
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use ConfigVersion::{Master, Release};
    use rustix::fs::Mode;

    use super::*;
    use crate::UserConfig;

    /// The config and the keystore the 0.3.2 release's `init-config -p
    /// <peer>` wrote, as two documents.
    const FIXTURE: &str = include_str!("v0_3_2.yaml");
    const PEER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";

    #[test]
    fn a_0_3_2_config_is_upgraded_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("user_config.yaml");
        let (config, keystore) = FIXTURE.split_once("\n---\n").unwrap();
        std::fs::write(&path, config).unwrap();
        let keystore_path = dir.path().join("keystore.yaml");
        std::fs::write(&keystore_path, keystore).unwrap();
        // 0.3.2 wrote it with the default mode: its owner has to restrict it before
        // this release loads it.
        rustix::fs::chmod(&keystore_path, Mode::RUSR | Mode::WUSR).unwrap();

        let loaded = UserConfig::load(&path).unwrap();
        // Everything else 0.3.2 wrote was a default, then or now.
        let expected = format!(
            "version: {}\n\
             network:\n  backend:\n    initial_peers:\n    - /ip4/1.2.3.4/udp/3000/quic-v1/p2p/{PEER}\n\
             cryptarchia:\n  network:\n    bootstrap:\n      ibd:\n        peers:\n        - {PEER}\n",
            ConfigVersion::current()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("user_config.yaml.0.3.2.bak")).unwrap(),
            config
        );
        let swarm = &loaded.network.backend.swarm;
        assert_eq!(swarm.kademlia.cache_size, Some(1));
        assert_eq!(
            swarm.chain_sync.peer_response_timeout,
            Duration::from_secs(5)
        );
        assert_eq!(
            swarm.gossipsub.duplicate_cache_time,
            Duration::from_hours(24)
        );
        assert_eq!(loaded.api.backend.timeout, Duration::from_secs(30));
        assert_eq!(loaded.mempool.tx_ttl, Some(Duration::from_hours(1)));
        assert_eq!(loaded.tracing.logger.file.unwrap().prefix, None);

        // Upgraded, it loads as it is.
        UserConfig::load(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 3);
    }

    #[test]
    fn a_current_config_is_left_as_it_is() {
        let standalone = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../standalone-node-config.yaml"
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::copy(standalone, &path).unwrap();
        UserConfig::load(&path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            std::fs::read_to_string(standalone).unwrap()
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn migrations_run_up_to_the_build() {
        // On master after 0.4.0, with a format change since; then 0.5.0 is cut.
        let migrations = [
            Migration {
                to: Release(0, 4, 0),
                migrate: |_| Ok(()),
            },
            Migration {
                to: Master,
                migrate: |_| Ok(()),
            },
        ];
        let runs = |from, current| pending(&migrations, from, current).count();
        for (from, on_master, on_0_5_0) in [
            (Release(0, 3, 2), 2, 2),
            (Release(0, 4, 0), 1, 1),
            // A patch release keeps its line's format.
            (Release(0, 4, 1), 1, 1),
            // Master is what 0.5.0 becomes.
            (Release(0, 5, 0), 1, 0),
            (Master, 0, 0),
        ] {
            assert_eq!(runs(from, Master), on_master, "{from} on master");
            assert_eq!(runs(from, Release(0, 5, 0)), on_0_5_0, "{from} on 0.5.0");
        }

        let unreleased: Vec<_> = MIGRATIONS.iter().filter(|m| m.to == Master).collect();
        assert!(unreleased.len() <= 1);
        assert!(unreleased.is_empty() || MIGRATIONS.last().unwrap().to == Master);
        assert!(Release(0, 3, 2) < Release(0, 3, 10) && Release(999, 0, 0) < Master);
        assert_eq!(
            "0.4.0-rc.1".parse::<ConfigVersion>().unwrap(),
            Release(0, 4, 0)
        );
        for not_a_version in ["0.4", "0.4.0-beta", "01.4.0", "v0.4.0"] {
            assert!(not_a_version.parse::<ConfigVersion>().is_err());
        }
    }
}
