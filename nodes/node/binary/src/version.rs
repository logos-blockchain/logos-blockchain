//! Version and build information, baked in at compile time by `build.rs`.

pub(crate) const HEAD_COMMIT_HASH: &str = env!("HEAD_COMMIT_HASH");
pub(crate) const HEAD_TAG_NAME: &str = env!("HEAD_TAG_NAME");
pub(crate) const PKG_VERSION: &str = env!("PKG_VERSION");
pub(crate) const TARGET: &str = env!("TARGET");
pub(crate) const PROFILE: &str = env!("PROFILE");
pub(crate) const RUSTC_VERSION: &str = env!("RUSTC_VERSION");

/// Version of the running binary, with the commit it was built from.
///
/// The commit is omitted when the binary is not built from a git checkout.
#[must_use]
pub fn node_version() -> String {
    if HEAD_COMMIT_HASH.is_empty() {
        PKG_VERSION.to_owned()
    } else {
        format!("{PKG_VERSION} ({HEAD_COMMIT_HASH})")
    }
}
