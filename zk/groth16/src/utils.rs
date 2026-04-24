use std::str::FromStr;

use ark_bn254::{Fq, Fq2, G1Affine, G2Affine};

pub type JsonG1 = [String; 3];
pub type JsonG2 = [[String; 2]; 3];

pub struct StringifiedG1(pub [String; 3]);

impl TryFrom<StringifiedG1> for G1Affine {
    type Error = <Fq as FromStr>::Err;
    fn try_from(value: StringifiedG1) -> Result<Self, Self::Error> {
        let [x, y, _] = value.0;
        let x = Fq::from_str(&x)?;
        let y = Fq::from_str(&y)?;
        Ok(Self::new_unchecked(x, y))
    }
}

pub struct StringifiedG2(pub [[String; 2]; 3]);

impl TryFrom<StringifiedG2> for G2Affine {
    type Error = <Fq as FromStr>::Err;

    fn try_from(value: StringifiedG2) -> Result<Self, Self::Error> {
        let [[x1, y1], [x2, y2], _] = value.0;
        let x = Fq2::new(Fq::from_str(&x1)?, Fq::from_str(&y1)?);
        let y = Fq2::new(Fq::from_str(&x2)?, Fq::from_str(&y2)?);
        Ok(Self::new_unchecked(x, y))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stringified_g1() {
        let _: G1Affine = StringifiedG1([
            "8296175608850998036255335084231000907125502603097068078993517773809496732066"
                .to_owned(),
            "8263160927867860156491312948728748265016489542834411322655068343855704802368"
                .to_owned(),
            "1".to_owned(),
        ])
        .try_into()
        .unwrap();
    }

    #[test]
    fn stringified_g2() {
        let _: G2Affine = StringifiedG2([
            [
                "10857046999023057135944570762232829481370756359578518086990519993285655852781"
                    .to_owned(),
                "11559732032986387107991004021392285783925812861821192530917403151452391805634"
                    .to_owned(),
            ],
            [
                "8495653923123431417604973247489272438418190587263600148770280649306958101930"
                    .to_owned(),
                "4082367875863433681332203403145435568316851327593401208105741076214120093531"
                    .to_owned(),
            ],
            ["1".to_owned(), "0".to_owned()],
        ])
        .try_into()
        .unwrap();
    }
}
