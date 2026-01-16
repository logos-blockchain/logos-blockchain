use groth16::{fr_to_bytes, serde::serde_fr};
use poseidon2::{Fr, ZkHash};
use serde::{Deserialize, Serialize};

use crate::mantle::TxHash;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Default, Serialize, Deserialize)]
pub struct RewardsRoot(#[serde(with = "serde_fr")] ZkHash);
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct VoucherNullifier(#[serde(with = "serde_fr")] ZkHash);
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Default, Serialize, Deserialize)]
pub struct VoucherCm(#[serde(with = "serde_fr")] ZkHash);

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct LeaderClaimOp {
    pub rewards_root: RewardsRoot,
    pub voucher_nullifier: VoucherNullifier,
    pub mantle_tx_hash: TxHash,
}

impl AsRef<Fr> for VoucherCm {
    fn as_ref(&self) -> &Fr {
        &self.0
    }
}

impl From<Fr> for VoucherCm {
    fn from(value: Fr) -> Self {
        Self(value)
    }
}

impl From<Fr> for RewardsRoot {
    fn from(value: Fr) -> Self {
        Self(value)
    }
}

impl From<Fr> for VoucherNullifier {
    fn from(value: Fr) -> Self {
        Self(value)
    }
}

impl From<RewardsRoot> for Fr {
    fn from(value: RewardsRoot) -> Self {
        value.0
    }
}

impl From<VoucherNullifier> for Fr {
    fn from(value: VoucherNullifier) -> Self {
        value.0
    }
}

impl From<VoucherCm> for Fr {
    fn from(value: VoucherCm) -> Self {
        value.0
    }
}

impl VoucherCm {
    #[must_use]
    pub fn to_bytes(&self) -> [u8; 32] {
        fr_to_bytes(&self.0)
    }
}
