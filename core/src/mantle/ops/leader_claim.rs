use std::sync::LazyLock;

use lb_groth16::{fr_from_bytes, fr_to_bytes, serde::serde_fr};
use lb_poseidon2::{Fr, ZkHash};
use serde::{Deserialize, Serialize};

use crate::{crypto::ZkHasher, mantle::TxHash};

static REWARD_VOUCHER: LazyLock<Fr> = LazyLock::new(|| {
    fr_from_bytes(b"REWARD_VOUCHER").expect("BigUint should load from constant string")
});

static VOUCHER_NF: LazyLock<Fr> = LazyLock::new(|| {
    fr_from_bytes(b"VOUCHER_NF").expect("BigUint should load from constant string")
});

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Default, Serialize, Deserialize)]
pub struct RewardsRoot(#[serde(with = "serde_fr")] ZkHash);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct VoucherSecret(#[serde(with = "serde_fr")] pub Fr);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct VoucherNullifier(#[serde(with = "serde_fr")] ZkHash);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Default, Serialize, Deserialize)]
pub struct VoucherCm(#[serde(with = "serde_fr")] ZkHash);

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct LeaderClaimOp {
    pub rewards_root: RewardsRoot,
    pub voucher_nullifier: VoucherNullifier,
    pub mantle_tx_hash: TxHash,
}

impl From<Fr> for VoucherSecret {
    fn from(value: Fr) -> Self {
        Self(value)
    }
}

impl From<VoucherSecret> for Fr {
    fn from(value: VoucherSecret) -> Self {
        value.0
    }
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

impl VoucherNullifier {
    #[must_use]
    pub fn from_secret(voucher_secret: VoucherSecret) -> Self {
        let mut hash = ZkHasher::new();
        hash.compress(&[*VOUCHER_NF, voucher_secret.into()]);
        hash.finalize().into()
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

    #[must_use]
    pub fn from_secret(voucher_secret: VoucherSecret) -> Self {
        let mut hash = ZkHasher::new();
        hash.compress(&[*REWARD_VOUCHER, voucher_secret.into()]);
        hash.finalize().into()
    }
}
