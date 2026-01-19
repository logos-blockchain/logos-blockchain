use lb_groth16::{Fr, Groth16Input, Groth16InputDeser};
use lb_pol::{P, compute_lottery_values};
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Copy, Clone)]
pub struct PoQChainInputs {
    session: Groth16Input,
    core_root: Groth16Input,
    pol_ledger_aged: Groth16Input,
    pol_epoch_nonce: Groth16Input,
    pol_t0: Groth16Input,
    pol_t1: Groth16Input,
}

#[derive(Clone, Copy)]
pub struct PoQChainInputsData {
    pub session: u64,
    pub core_root: Fr,
    pub pol_ledger_aged: Fr,
    pub pol_epoch_nonce: Fr,
    pub total_stake: u64,
}

#[derive(Deserialize, Serialize)]
pub struct PoQChainInputsJson {
    session: Groth16InputDeser,
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
            session,
            core_root,
            pol_ledger_aged,
            pol_epoch_nonce,
            pol_t0,
            pol_t1,
        }: PoQChainInputsJson,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            session: session.try_into()?,
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
            session,
            core_root,
            pol_ledger_aged,
            pol_epoch_nonce,
            pol_t0,
            pol_t1,
        }: &PoQChainInputs,
    ) -> Self {
        Self {
            session: session.into(),
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
    #[error("Session number is greater than P")]
    SessionGreaterThanP,
    #[error("Core quota is greater than 20 bits")]
    CoreQuotaMoreThan20Bits,
    #[error("Leader quota is greater than 20 bits")]
    LeaderQuotaMoreThan20Bits,
}

impl TryFrom<PoQChainInputsData> for PoQChainInputs {
    type Error = PoQInputsFromDataError;

    fn try_from(
        PoQChainInputsData {
            session,
            core_root,
            pol_ledger_aged,
            pol_epoch_nonce,
            total_stake,
        }: PoQChainInputsData,
    ) -> Result<Self, Self::Error> {
        let session = BigUint::from(session);
        if session > *P {
            return Err(PoQInputsFromDataError::SessionGreaterThanP);
        }

        let (lottery_0, lottery_1) = compute_lottery_values(total_stake);

        Ok(Self {
            session: Groth16Input::new(session.into()),
            core_root: core_root.into(),
            pol_ledger_aged: pol_ledger_aged.into(),
            pol_epoch_nonce: pol_epoch_nonce.into(),
            pol_t0: Groth16Input::new(lottery_0.into()),
            pol_t1: Groth16Input::new(lottery_1.into()),
        })
    }
}
