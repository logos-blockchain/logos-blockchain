use ark_bn254::Fr;
use ark_ff::Field as _;

use crate::{Digest, Poseidon2Bn254};

#[derive(Clone, Debug)]
pub struct Poseidon2Hasher {
    state: [Fr; 3],
}

impl Default for Poseidon2Hasher {
    fn default() -> Self {
        Self::new()
    }
}

impl Poseidon2Hasher {
    #[must_use]
    pub const fn new() -> Self {
        let state = [Fr::ZERO, Fr::ZERO, Fr::ZERO];
        Self { state }
    }

    fn update_one(&mut self, input: &Fr) {
        self.state[0] += input;
        Poseidon2Bn254::permute_mut::<jf_poseidon2::constants::bn254::Poseidon2ParamsBn3, 3>(
            &mut self.state,
        );
    }

    fn update(&mut self, input: &[Fr]) {
        for fr in input {
            self.update_one(fr);
        }
        self.update_one(&Fr::ONE);
    }

    /// Only use `compress` before `finalize` for poseidon2 compression without
    /// padding
    fn compress(&mut self, inputs: &[Fr; 2]) {
        self.state[0] += inputs[0];
        self.state[1] += inputs[1];
        Poseidon2Bn254::permute_mut::<jf_poseidon2::constants::bn254::Poseidon2ParamsBn3, 3>(
            &mut self.state,
        );
    }

    const fn finalize(self) -> Fr {
        self.state[0]
    }
}

impl Digest for Poseidon2Hasher {
    fn digest(inputs: &[Fr]) -> Fr {
        let mut hasher = Self::new();
        hasher.update(inputs);
        hasher.finalize()
    }

    fn compress(inputs: &[Fr; 2]) -> Fr {
        let mut hasher = Self::new();
        hasher.compress(inputs);
        hasher.finalize()
    }

    fn new() -> Self {
        Self::new()
    }

    fn update(&mut self, input: &Fr) {
        Self::update_one(self, input);
    }

    fn finalize(mut self) -> Fr {
        Self::update_one(&mut self, &Fr::ONE);
        Self::finalize(self)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use num_bigint::BigUint;

    use super::*;

    fn test_hasher(input: &[Fr], expected: Fr) {
        let mut hasher = Poseidon2Hasher::new();
        hasher.update(input);
        let result = hasher.finalize();
        assert_eq!(result, expected);
    }

    fn test_compresser(inputs: &[Fr; 2], expected: Fr) {
        let mut hasher = Poseidon2Hasher::new();
        hasher.compress(inputs);
        let result = hasher.finalize();
        assert_eq!(result, expected);
    }

    #[test]
    fn test_hashes() {
        // 0
        let expected_zero = Fr::from_str(
            "14440562208246903332530876912784724937356723424375796042690034647976142142243",
        )
        .unwrap();
        test_hasher(&[Fr::ZERO], expected_zero);
        // 1
        let expected_one = Fr::from_str(
            "13955187255749411516377601857453481686854514827536340092448578824571923228920",
        )
        .unwrap();
        test_hasher(&[Fr::ONE], expected_one);
        // 0, 0
        let expected = Fr::from_str(
            "14628790903668924121747280216643356178740756932733894594323650293252618457042",
        )
        .unwrap();
        test_hasher(&[Fr::ZERO, Fr::ZERO], expected);
        // 1, 2
        let expected = Fr::from_str(
            "14118544982895877636855211757199904519359053761360294109973292038354361461611",
        )
        .unwrap();
        test_hasher(&[Fr::ONE, Fr::from(BigUint::from(2u8))], expected);
        // 2, 1
        let expected = Fr::from_str(
            "17303708087492456923876794017773991179968227132845592592623864164460458364283",
        )
        .unwrap();
        test_hasher(&[Fr::from(BigUint::from(2u8)), Fr::ONE], expected);
        // 1, 0, 0
        let expected = Fr::from_str(
            "8421738025868928791358153716794664271727148606331557350959968139012924692418",
        )
        .unwrap();
        test_hasher(&[Fr::ONE, Fr::ZERO, Fr::ZERO], expected);
        // 0, 0, 1
        let expected = Fr::from_str(
            "19071010037288550145243369517116292645821506141834920037435390558790324604368",
        )
        .unwrap();
        test_hasher(&[Fr::ZERO, Fr::ZERO, Fr::ONE], expected);
        // 0, 1, 0, 1
        let expected = Fr::from_str(
            "3427143977204509234202184342982950793741509606314919897018772479233527131453",
        )
        .unwrap();
        test_hasher(&[Fr::ZERO, Fr::ONE, Fr::ZERO, Fr::ONE], expected);
    }

    #[test]
    fn test_compression() {
        // 0, 0
        let expected = Fr::from_str(
            "21177166670744647784289648293577786481357446166129397094207318338605633126018",
        )
        .unwrap();
        test_compresser(&[Fr::ZERO, Fr::ZERO], expected);
        // 1, 0
        let expected = Fr::from_str(
            "2820430044171165092709918704747590965614342875549110429217681435604321658469",
        )
        .unwrap();
        test_compresser(&[Fr::ONE, Fr::ZERO], expected);
        // 0, 1
        let expected = Fr::from_str(
            "15449469107951025862283679587511638593643295575495923463032662929748907033596",
        )
        .unwrap();
        test_compresser(&[Fr::ZERO, Fr::ONE], expected);
        // 1, 1
        let expected = Fr::from_str(
            "17847258390462923071212518425927834238796435801505415407318169918090986946609",
        )
        .unwrap();
        test_compresser(&[Fr::ONE, Fr::ONE], expected);
    }
}
