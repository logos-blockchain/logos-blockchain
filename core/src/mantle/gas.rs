pub type Gas = crate::mantle::ledger::Value;

pub trait GasCost {
    /// Returns the gas cost of this operation.
    fn gas_cost<Constants: GasConstants>(&self) -> Gas;
}

impl<T: GasCost> GasCost for &T {
    fn gas_cost<Constants: GasConstants>(&self) -> Gas {
        T::gas_cost::<Constants>(self)
    }
}

pub trait GasConstants {
    /// Verify the proof of ownership and relative balance.
    const LEDGER_TX: Gas;

    /// Verify the inscription signature.
    const CHANNEL_INSCRIBE: Gas;

    /// Verify blob availability and signature.
    const CHANNEL_BLOB_BASE: Gas;

    /// Store the message in a blob.
    const CHANNEL_BLOB_SIZED: Gas;

    /// Verify the administrator signature.
    const CHANNEL_SET_KEYS: Gas;

    /// Verify the proof of ownership.
    const SDP_DECLARE: Gas;

    /// Verify the proof of ownership.
    const SDP_WITHDRAW: Gas;

    /// Store the active message.
    const SDP_ACTIVE: Gas;

    /// Consume a reward ticket.
    const LEADER_CLAIM: Gas;
}

pub struct MainnetGasConstants;

impl GasConstants for MainnetGasConstants {
    const LEDGER_TX: Gas = 2705;
    const CHANNEL_INSCRIBE: Gas = 22;
    const CHANNEL_BLOB_BASE: Gas = 800;
    const CHANNEL_BLOB_SIZED: Gas = 100;
    const CHANNEL_SET_KEYS: Gas = 22;
    const SDP_DECLARE: Gas = 2727;
    const SDP_WITHDRAW: Gas = 2705;
    const SDP_ACTIVE: Gas = 2705;
    const LEADER_CLAIM: Gas = 1150;
}
