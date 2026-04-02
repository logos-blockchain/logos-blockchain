pub type Gas = crate::mantle::ledger::Value;

pub trait GasCost {
    type Context;

    /// Returns the gas cost of this operation.
    fn total_gas_cost<Constants: GasConstants>(&self, context: &Self::Context) -> Gas;
    fn storage_gas_cost(&self, context: &Self::Context) -> Gas;
    fn execution_gas_consumption<Constants: GasConstants>(&self, context: &Self::Context) -> Gas;
    fn storage_gas_consumption(&self, context: &Self::Context) -> Gas;
}

impl<T: GasCost> GasCost for &T {
    type Context = T::Context;

    fn total_gas_cost<Constants: GasConstants>(&self, context: &Self::Context) -> Gas {
        T::total_gas_cost::<Constants>(self, context)
    }

    fn storage_gas_cost(&self, context: &Self::Context) -> Gas {
        T::storage_gas_cost(self, context)
    }

    fn execution_gas_consumption<Constants: GasConstants>(&self, context: &Self::Context) -> Gas {
        T::execution_gas_consumption::<Constants>(self, context)
    }

    fn storage_gas_consumption(&self, context: &Self::Context) -> Gas {
        T::storage_gas_consumption(self, context)
    }
}

pub trait GasConstants {
    /// Verify the proof of ownership and relative balance.
    const TRANSFER: Gas;

    /// Verify the inscription signature.
    const CHANNEL_INSCRIBE: Gas;

    /// Verify the administrator signature.
    const CHANNEL_SET_KEYS: Gas;

    /// Verify the deposit signature.
    const CHANNEL_DEPOSIT: Gas;

    /// Verify the withdrawal signature.
    const CHANNEL_WITHDRAW: Gas;

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
    const TRANSFER: Gas = 2705;
    const CHANNEL_INSCRIBE: Gas = 22;
    const CHANNEL_SET_KEYS: Gas = 22;
    const CHANNEL_DEPOSIT: Gas = 0;
    const CHANNEL_WITHDRAW: Gas = 22;
    const SDP_DECLARE: Gas = 2727;
    const SDP_WITHDRAW: Gas = 2705;
    const SDP_ACTIVE: Gas = 2705;
    const LEADER_CLAIM: Gas = 1150;
}
