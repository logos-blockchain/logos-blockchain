use std::{
    ops::{Deref as _, Div as _},
    sync::LazyLock,
};

use num_bigint::BigUint;
use num_traits::{CheckedSub as _, Num as _};

/// The active slot coefficient coefficient that is expected to be achieved
/// probabilistically by the constants ([`P`], [`T0_CONSTANT`], and
/// [`T1_CONSTANT`]) defined in this module.
pub const TARGET_ACTIVE_SLOT_COEFF: f64 = 1.0 / 30.0;

/// From [Proof of Leadership spec](https://www.notion.so/nomos-tech/v1-1-Proof-of-Leadership-Specification-2e9261aa09df80058244c902defc6da2?source=copy_link#2e9261aa09df8010b506cea919608b8b)
pub static P: LazyLock<BigUint> = LazyLock::new(|| {
    BigUint::from_str_radix(
        "30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001",
        16,
    )
    .expect("P constant should parse")
});

/// From [Proof of Leadership spec](https://www.notion.so/nomos-tech/v1-1-Proof-of-Leadership-Specification-2e9261aa09df80058244c902defc6da2?source=copy_link#2e9261aa09df8010b506cea919608b8b)
pub static T0_CONSTANT: LazyLock<BigUint> = LazyLock::new(|| {
    BigUint::from_str_radix(
        "1a3fb997fd58374772808c13d1c2ddacb5ab3ea77413f86fd6e0d3d978e5438",
        16,
    )
    .expect("Constant should parse")
});

/// From [Proof of Leadership spec](https://www.notion.so/nomos-tech/v1-1-Proof-of-Leadership-Specification-2e9261aa09df80058244c902defc6da2?source=copy_link#2e9261aa09df8010b506cea919608b8b)
pub static T1_CONSTANT: LazyLock<BigUint> = LazyLock::new(|| {
    BigUint::from_str_radix(
        "71e790b41991052e30c93934b5612412e7958837bac8b1c524c24d84cc7d0",
        16,
    )
    .expect("Constant should parse")
});

pub fn compute_lottery_values(total_stake: u64) -> (BigUint, BigUint) {
    let total_stake = BigUint::from(total_stake);

    let lottery_0 = T0_CONSTANT.deref().div(total_stake.clone());
    let lottery_1 = P
        .checked_sub(&T1_CONSTANT.deref().div(total_stake.pow(2)))
        .expect("(T1 / (S^2)) must be less than P");
    (lottery_0, lottery_1)
}
