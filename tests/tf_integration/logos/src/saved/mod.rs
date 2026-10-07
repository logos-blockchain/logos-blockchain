//! Temporary compatibility support for saved native Logos configuration.

mod adapter;
mod bundle;

pub use adapter::{SavedLogosDeployment, SavedLogosEnv, SavedLogosNodeConfig};
pub use bundle::{PreparedConfigBundle, SavedDeployment, SavedNode};
