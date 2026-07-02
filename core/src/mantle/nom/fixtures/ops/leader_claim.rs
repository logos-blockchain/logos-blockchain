use lb_core_macros::nom_wire_fixtures;
use lb_key_management_system_keys::keys::ZkPublicKey;
use num_bigint::BigUint;

use crate::mantle::ops::leader_claim::{LeaderClaimOp, RewardsRoot, VoucherNullifier};

nom_wire_fixtures!(RewardsRoot, {
    Self(BigUint::from(0x0807_0605_0403_0201u64).into())
} => "0102030405060708000000000000000000000000000000000000000000000000");

nom_wire_fixtures!(VoucherNullifier, {
    Self(BigUint::from(0x0807_0605_0403_0201u64).into())
} => "0102030405060708000000000000000000000000000000000000000000000000");

nom_wire_fixtures!(LeaderClaimOp, {
    Self {
        rewards_root: RewardsRoot(BigUint::from(0x1000u64).into()),
        voucher_nullifier: VoucherNullifier(BigUint::from(0x2000u64).into()),
        pk: ZkPublicKey::from(BigUint::from(0x3000u64)),
    }
} => "001000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000030000000000000000000000000000000000000000000000000000000000000");
