use crate::env as tf_env;

pub const DEFAULT_CFGSYNC_PORT: u16 = 4400;
pub const DEFAULT_ASSETS_STACK_DIR: &str = "tests/testing_framework/assets/runtime";

#[must_use]
pub fn cfgsync_port() -> u16 {
    tf_env::nomos_cfgsync_port().unwrap_or(DEFAULT_CFGSYNC_PORT)
}
