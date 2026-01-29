use std::{
    ops::{Deref as _, Div as _},
    sync::LazyLock,
};

use num_bigint::BigUint;
use num_traits::{CheckedSub as _, Num as _};

#[must_use]
pub const fn slot_activation_coefficient() -> f64 {
    #[cfg(not(feature = "high-active-slot-coefficient"))]
    {
        1f64 / 30f64
    }
    #[cfg(feature = "high-active-slot-coefficient")]
    0.5f64
}

/// From [Proof of Leadership spec](https://www.notion.so/nomos-tech/Proof-of-Leadership-Specification-21c261aa09df819ba5b6d95d0fe3066d?source=copy_link#256261aa09df800fbc88e5aae5ea7e06)
pub static P: LazyLock<BigUint> = LazyLock::new(|| {
    BigUint::from_str_radix(
        "30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001",
        16,
    )
    .expect("P constant should parse")
});

/// From [Proof of Leadership spec](https://www.notion.so/nomos-tech/Proof-of-Leadership-Specification-21c261aa09df819ba5b6d95d0fe3066d?source=copy_link#256261aa09df800fbc88e5aae5ea7e06)
pub static T0_CONSTANT: LazyLock<BigUint> = LazyLock::new(|| {
    #[cfg(not(feature = "high-active-slot-coefficient"))]
    {
        BigUint::from_str_radix(
            "1a3fb997fd58374772808c13d1c2ddacb5ab3ea77413f86fd6e0d3d978e5438",
            16,
        )
        .expect("Constant should parse")
    }
    #[cfg(feature = "high-active-slot-coefficient")]
    {
        BigUint::from_str_radix(
            "218aeb703858b6ef2bc62d5871c765025863620764726ce3f26fd914428c45b6",
            16,
        )
        .expect("Constant should parse")
    }
});

/// From [Proof of Leadership spec](https://www.notion.so/nomos-tech/Proof-of-Leadership-Specification-21c261aa09df819ba5b6d95d0fe3066d?source=copy_link#256261aa09df800fbc88e5aae5ea7e06)
pub static T1_CONSTANT: LazyLock<BigUint> = LazyLock::new(|| {
    #[cfg(not(feature = "high-active-slot-coefficient"))]
    {
        BigUint::from_str_radix(
            "71e790b41991052e30c93934b5612412e7958837bac8b1c524c24d84cc7d0",
            16,
        )
        .expect("Constant should parse")
    }
    #[cfg(feature = "high-active-slot-coefficient")]
    {
        BigUint::from_str_radix(
            "b9fffe4b14e22c8093204936e25194c3233e8167446a8089029be711a359baa",
            16,
        )
        .expect("Constant should parse")
    }
});

pub fn compute_lottery_values(total_stake: u64) -> (BigUint, BigUint) {
    let total_stake = BigUint::from(total_stake);

    let lottery_0 = T0_CONSTANT.deref().div(total_stake.clone());
    let lottery_1 = P
        .checked_sub(&T1_CONSTANT.deref().div(total_stake.pow(2)))
        .expect("(T1 / (S^2)) must be less than P");
    (lottery_0, lottery_1)
}
