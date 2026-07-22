use lb_groth16::{Fr, Groth16Input, Groth16InputDeser};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Copy, Clone)]
pub struct PoQChainInputs {
    core_root: Groth16Input,
    pol_ledger_aged: Groth16Input,
    pol_epoch_nonce: Groth16Input,
    pol_t0: Groth16Input,
    pol_t1: Groth16Input,
}

#[derive(Clone, Copy)]
pub struct PoQChainInputsData {
    pub core_root: Fr,
    pub pol_ledger_aged: Fr,
    pub pol_epoch_nonce: Fr,
    pub lottery_0: Fr,
    pub lottery_1: Fr,
}

#[derive(Deserialize, Serialize)]
pub struct PoQChainInputsJson {
    core_root: Groth16InputDeser,
    pol_ledger_aged: Groth16InputDeser,
    pol_epoch_nonce: Groth16InputDeser,
    pol_t0: Groth16InputDeser,
    pol_t1: Groth16InputDeser,
}

impl TryFrom<PoQChainInputsJson> for PoQChainInputs {
    type Error = <Groth16Input as TryFrom<Groth16InputDeser>>::Error;

    fn try_from(
        PoQChainInputsJson {
            core_root,
            pol_ledger_aged,
            pol_epoch_nonce,
            pol_t0,
            pol_t1,
        }: PoQChainInputsJson,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            core_root: core_root.try_into()?,
            pol_ledger_aged: pol_ledger_aged.try_into()?,
            pol_epoch_nonce: pol_epoch_nonce.try_into()?,
            pol_t0: pol_t0.try_into()?,
            pol_t1: pol_t1.try_into()?,
        })
    }
}

impl From<&PoQChainInputs> for PoQChainInputsJson {
    fn from(
        PoQChainInputs {
            core_root,
            pol_ledger_aged,
            pol_epoch_nonce,
            pol_t0,
            pol_t1,
        }: &PoQChainInputs,
    ) -> Self {
        Self {
            core_root: core_root.into(),
            pol_ledger_aged: pol_ledger_aged.into(),
            pol_epoch_nonce: pol_epoch_nonce.into(),
            pol_t0: pol_t0.into(),
            pol_t1: pol_t1.into(),
        }
    }
}

#[derive(Debug, Error)]
pub enum PoQInputsFromDataError {
    #[error("Core quota is greater than 20 bits")]
    CoreQuotaMoreThan20Bits,
    #[error("Leader quota is greater than 20 bits")]
    LeaderQuotaMoreThan20Bits,
}

impl TryFrom<PoQChainInputsData> for PoQChainInputs {
    type Error = PoQInputsFromDataError;

    fn try_from(
        PoQChainInputsData {
            core_root,
            pol_ledger_aged,
            pol_epoch_nonce,
            lottery_0,
            lottery_1,
        }: PoQChainInputsData,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            core_root: core_root.into(),
            pol_ledger_aged: pol_ledger_aged.into(),
            pol_epoch_nonce: pol_epoch_nonce.into(),
            pol_t0: Groth16Input::new(lottery_0),
            pol_t1: Groth16Input::new(lottery_1),
        })
    }
}
