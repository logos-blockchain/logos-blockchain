//! The node's version and build provenance, shared by everything that reports
//! it: the binary's `--version` output, the HTTP `/version` endpoint and the
//! client that reads it.
//!
//! The values are baked in at compile time by this crate's `build.rs`.

use core::fmt::{self, Display, Formatter};

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

const HEAD_COMMIT_HASH: &str = env!("HEAD_COMMIT_HASH");
const HEAD_TAG_NAME: &str = env!("HEAD_TAG_NAME");
const PKG_VERSION: &str = env!("PKG_VERSION");
const TARGET: &str = env!("TARGET");
const PROFILE: &str = env!("PROFILE");
const RUSTC_VERSION: &str = env!("RUSTC_VERSION");

/// Version and build provenance of a running node.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct BuildVersionInfo {
    /// Crate version of the binary, e.g. `0.3.0-rc.2`.
    pub version: String,
    /// Abbreviated commit the binary was built from, absent when it was not
    /// built from a git checkout.
    pub commit: Option<String>,
    /// Tag pointing exactly at that commit, absent when there is none.
    pub tag: Option<String>,
    /// Target triple the binary was compiled for.
    pub target: String,
    /// Cargo profile the binary was compiled with.
    pub profile: String,
    /// Version of the compiler that built the binary.
    pub rustc: String,
}

impl Display for BuildVersionInfo {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let Self {
            version,
            commit,
            tag,
            target,
            profile,
            rustc,
        } = self;

        let commit_line = match (commit.as_deref(), tag.as_deref()) {
            (Some(commit), Some(tag)) => format!("commit:  {commit} (tag {tag})"),
            (Some(commit), None) => format!("commit:  {commit}"),
            (None, _) => "commit:  unknown".to_owned(),
        };

        write!(
            f,
            "\
{version}
{commit_line}
target:  {target}
profile: {profile}
rustc:   {rustc}"
        )
    }
}

/// Version and build provenance of the running binary.
///
/// The commit and tag are omitted when the binary is not built from a git
/// checkout.
#[must_use]
pub fn build_version_info() -> BuildVersionInfo {
    BuildVersionInfo {
        version: PKG_VERSION.to_owned(),
        commit: optional(HEAD_COMMIT_HASH),
        tag: optional(HEAD_TAG_NAME),
        target: TARGET.to_owned(),
        profile: PROFILE.to_owned(),
        rustc: RUSTC_VERSION.to_owned(),
    }
}

fn optional(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}
