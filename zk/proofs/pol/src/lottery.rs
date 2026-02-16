use std::sync::LazyLock;

use astro_float::{BigFloat, Consts, Radix, RoundingMode, Sign};
use lb_groth16::Fr;
use lb_utils::math::NonNegativeRatio;
use num_bigint::BigUint;
use num_traits::{CheckedSub as _, Num as _};

/// The BN254 scalar field order,
///
/// The value is defined in [Proof of Leadership spec](https://nomos-tech.notion.site/v1-1-Proof-of-Leadership-Specification-2e9261aa09df80058244c902defc6da2#2e9261aa09df807abe78c815e7a31809)
pub static P: LazyLock<BigUint> = LazyLock::new(|| {
    BigUint::from_str_radix(
        "30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001",
        16,
    )
    .expect("P constant should parse")
});

/// Lottery approximation constants used for computing t₀ and t₁.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct LotteryConstants {
    pub t0_constant: BigUint,
    pub t1_constant: BigUint,
}

impl LotteryConstants {
    const PRECISION: usize = 512;
    const ROUNDING_MODE: RoundingMode = RoundingMode::ToEven;

    /// Computes the lottery approximation constants with 512 bits of precision.
    ///
    /// Formulas:
    /// - t₀_constant = floor(p * (-ln(1-f)))
    /// - t₁_constant = floor(p * ln²(1-f) / 2)
    ///
    /// Where `p` is the BN254 scalar field order and `f` is the slot activation
    /// coefficient.
    ///
    /// The calculations are defined in the [Proof of Leadership spec](https://nomos-tech.notion.site/v1-1-Proof-of-Leadership-Specification-2e9261aa09df80058244c902defc6da2#2e9261aa09df807abe78c815e7a31809).
    #[expect(clippy::doc_markdown, reason = "math formulas")]
    #[must_use]
    pub fn new(f: NonNegativeRatio) -> Self {
        let mut cc = Consts::new().expect("memory allocation should succeed");

        let p = Self::p_as_bigfloat(&mut cc);

        // f = f_numerator / f_denominator (exact rational arithmetic)
        let f_num = BigFloat::from_u32(f.numerator, Self::PRECISION);
        let f_den = BigFloat::from_u32(f.denominator.get(), Self::PRECISION);
        let f = f_num.div(&f_den, Self::PRECISION, Self::ROUNDING_MODE);

        // -ln(1-f)
        let one = BigFloat::from_u32(1, Self::PRECISION);
        let one_minus_f = one.sub(&f, Self::PRECISION, Self::ROUNDING_MODE);
        let ln_one_minus_f = one_minus_f.ln(Self::PRECISION, Self::ROUNDING_MODE, &mut cc);
        let neg_ln = ln_one_minus_f.neg();

        // t₀_constant = floor(p * (-ln(1-f)))
        let t0_constant = Self::floor_bigfloat(
            &p.mul(&neg_ln, Self::PRECISION, Self::ROUNDING_MODE),
            &mut cc,
        );

        // ln²(1-f)
        let ln_sq = ln_one_minus_f.mul(&ln_one_minus_f, Self::PRECISION, Self::ROUNDING_MODE);

        // p * ln²(1-f)
        let t1_float = p.mul(&ln_sq, Self::PRECISION, Self::ROUNDING_MODE);

        // t₁_constant = floor(p * ln²(1-f) / 2)
        let two = BigFloat::from_u32(2, Self::PRECISION);
        let t1_constant = Self::floor_bigfloat(
            &t1_float.div(&two, Self::PRECISION, Self::ROUNDING_MODE),
            &mut cc,
        );

        Self {
            t0_constant,
            t1_constant,
        }
    }

    /// Convert [`P`] (BN254 field order) to [`BigFloat`].
    fn p_as_bigfloat(cc: &mut Consts) -> BigFloat {
        // Use decimal to avoid any hex 'e' vs exponent ambiguity
        BigFloat::parse(
            P.to_str_radix(10).as_str(),
            Radix::Dec,
            Self::PRECISION,
            Self::ROUNDING_MODE,
            cc,
        )
    }

    /// Floor a non-negative integer-valued [`BigFloat`] to [`BigUint`]
    fn floor_bigfloat(val: &BigFloat, cc: &mut Consts) -> BigUint {
        // Floor the value first
        let floored = val.floor();
        if floored.is_zero() {
            return BigUint::ZERO;
        }

        // Since astro-float doesn't provide direct BigUint conversion,
        // we extract hex digits and reconstruct the BigUint.
        //
        // - digits: mantissa as hex digits (0-15 each)
        // - exp: num of digits before decimal points
        let (sign, digits, exp) = floored
            .convert_to_radix(Radix::Hex, RoundingMode::None, cc)
            .expect("floored BigFloat should convert to hex");
        assert_eq!(sign, Sign::Pos, "floored BigFloat should be positive");

        // Negative exponent means value < 1, which floors to 0
        let exp = exp.max(0) as usize;

        // Build BigUint by processing each hex digit left-to-right.
        // Each hex digit represents 4 bits, so we shift left by 4
        // and add the digit value (or 0 if beyond mantissa length).
        let mut result = BigUint::ZERO;
        for i in 0..exp {
            result <<= 4;
            if i < digits.len() {
                result += u64::from(digits[i]);
            }
        }
        result
    }

    /// Computes the lottery values t₀ and t₁ for a given total stake.
    pub fn compute_lottery_values(&self, total_stake: u64) -> (Fr, Fr) {
        let total_stake = BigUint::from(total_stake);

        let lottery_0 = &self.t0_constant / &total_stake;
        let lottery_1 = P
            .checked_sub(&(&self.t1_constant / &total_stake.pow(2)))
            .expect("(T1 / (S^2)) must be less than P");
        (lottery_0.into(), lottery_1.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_lottery_constants() {
        let constants = LotteryConstants::new(NonNegativeRatio::new(1, 30.try_into().unwrap()));
        assert_eq!(
            constants.t0_constant,
            BigUint::from_str_radix(
                "1a3fb997fd5838f2a1585ee090a95c88129ab25cc4d2e2d28f1a95f81d85465",
                16,
            )
            .unwrap(),
        );
        assert_eq!(
            constants.t1_constant,
            BigUint::from_str_radix(
                "71e790b4199113a9a00298d823c5716ddac764a110a45fe3b770bbb3e8a57",
                16,
            )
            .unwrap()
        );

        let constants = LotteryConstants::new(NonNegativeRatio::new(1, 10.try_into().unwrap()));
        assert_eq!(
            constants.t0_constant,
            BigUint::from_str_radix(
                "5193d04d01fb16d1b9c55677fc83950d1cf88207f4a5756431fde6db9ab1768",
                16,
            )
            .unwrap(),
        );
        assert_eq!(
            constants.t1_constant,
            BigUint::from_str_radix(
                "44c2a290c72d4dc7d6e514a4b9683cc6e7c15e64f0f59482ec7d3f5906784a",
                16,
            )
            .unwrap()
        );
    }

    #[test]
    fn test_compute_lottery_values() {
        let constants = LotteryConstants::new(NonNegativeRatio::new(1, 30.try_into().unwrap()));
        let (lottery_0, lottery_1) = constants.compute_lottery_values(1000);

        assert_eq!(
            lottery_0,
            BigUint::from_str_radix(
                "6b83fe55f9383508b9bbe2d335e8e78d9c133ce0554b4f251b0ca3b6be8c",
                16,
            )
            .unwrap()
            .into(),
        );
        assert_eq!(
            lottery_1,
            BigUint::from_str_radix(
                "30644e7269c19af80558c2b75767747a6fa9f2beb0e87df2e51121184e5e6c17",
                16,
            )
            .unwrap()
            .into(),
        );
    }

    #[test]
    fn floor_bigfloat() {
        let mut cc = Consts::new().unwrap();

        assert_eq!(
            LotteryConstants::floor_bigfloat(
                &BigFloat::from_u32(0, LotteryConstants::PRECISION),
                &mut cc
            ),
            BigUint::ZERO
        );
        assert_eq!(
            LotteryConstants::floor_bigfloat(
                &BigFloat::from_u32(42, LotteryConstants::PRECISION),
                &mut cc
            ),
            BigUint::from(42u32)
        );
        assert_eq!(
            LotteryConstants::floor_bigfloat(
                &make_bigfloat("123456789012345678901234567890", &mut cc),
                &mut cc
            ),
            BigUint::parse_bytes(b"123456789012345678901234567890", 10).unwrap()
        );
        assert_eq!(
            LotteryConstants::floor_bigfloat(&make_bigfloat("99.999", &mut cc), &mut cc),
            BigUint::from(99u32)
        );
        assert_eq!(
            LotteryConstants::floor_bigfloat(&make_bigfloat("0.999", &mut cc), &mut cc),
            BigUint::ZERO
        );
    }

    fn make_bigfloat(val: &str, cc: &mut Consts) -> BigFloat {
        BigFloat::parse(
            val,
            Radix::Dec,
            LotteryConstants::PRECISION,
            RoundingMode::None,
            cc,
        )
    }
}
