pub mod best_node;
pub mod checks;
pub mod submissions;
pub mod sync;

use thiserror::Error;

pub(super) const TARGET: &str = "cucumber_wallet";

pub use crate::common::wallet::{WalletOutputState, WalletStateView};

#[must_use]
pub const fn wallet_output_state_label(state: WalletOutputState) -> &'static str {
    match state {
        WalletOutputState::OnChain => "on-chain",
        WalletOutputState::Reserved => "encumbered",
        WalletOutputState::Available => "available",
    }
}

pub fn parse_wallet_output_state(
    s: &str,
) -> Result<WalletOutputState, WalletOutputStateParseError> {
    match s.trim().to_ascii_lowercase().as_str() {
        "on-chain" => Ok(WalletOutputState::OnChain),
        "encumbered" | "reserved" => Ok(WalletOutputState::Reserved),
        "available" => Ok(WalletOutputState::Available),
        _ => Err(WalletOutputStateParseError {
            value: s.to_owned(),
        }),
    }
}

#[derive(Debug, Error)]
#[error("unknown wallet state type: '{value}'")]
pub struct WalletOutputStateParseError {
    value: String,
}
