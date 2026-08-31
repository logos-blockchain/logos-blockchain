pub mod serde_fr {
    use ark_bn254::Fr;
    use lb_utils::serde::{deserialize_bytes_array, serialize_bytes_array};
    use serde::{Deserializer, Serializer};

    use crate::{fr_from_bytes, fr_to_bytes};

    pub fn serialize<S>(item: &Fr, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let bytes = fr_to_bytes(item);
        serialize_bytes_array(bytes, serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Fr, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes = deserialize_bytes_array::<32, D>(deserializer)?;
        fr_from_bytes(&bytes).map_err(serde::de::Error::custom)
    }

    #[cfg(test)]
    mod tests {
        use ark_bn254::Fr;
        use num_bigint::BigUint;
        use serde::{Deserialize, Serialize};

        #[derive(Serialize, Deserialize, Eq, PartialEq, Debug)]
        struct FrDeser(#[serde(with = "crate::serde::serde_fr")] pub Fr);
        #[test]
        fn test_serialize_deserialize_json() {
            let fr1 = FrDeser(BigUint::from(123u8).into());
            let v = serde_json::to_string(&fr1).unwrap();
            let fr2: FrDeser = serde_json::from_str(&v).unwrap();
            assert_eq!(fr1, fr2);
        }

        #[test]
        fn test_serialize_deserialize_bin() {
            let fr1 = FrDeser(BigUint::from(123u8).into());
            let v = bincode::serialize(&fr1).unwrap();
            let fr2: FrDeser = bincode::deserialize(&v).unwrap();
            assert_eq!(fr1, fr2);
        }

        #[test]
        fn test_deserialize_rejects_oversized_json_hex() {
            let json = format!("\"{}\"", "00".repeat(33));
            assert!(serde_json::from_str::<FrDeser>(&json).is_err());
        }

        #[test]
        fn test_deserialize_rejects_out_of_range_json_hex() {
            let json = format!("\"{}\"", "ff".repeat(32));
            let error = serde_json::from_str::<FrDeser>(&json).unwrap_err();
            assert!(error.to_string().contains("bigger than the modulus"));
        }
    }
}

pub mod serde_fr_vec {
    use ark_bn254::Fr;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::serde_fr;

    #[derive(Serialize, Deserialize)]
    struct FrWrap(#[serde(with = "serde_fr")] Fr);

    pub fn serialize<S: Serializer>(v: &[Fr], s: S) -> Result<S::Ok, S::Error> {
        v.iter()
            .map(|x| FrWrap(*x))
            .collect::<Vec<_>>()
            .serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Fr>, D::Error> {
        Vec::<FrWrap>::deserialize(d).map(|v| v.into_iter().map(|FrWrap(x)| x).collect())
    }

    #[cfg(test)]
    mod tests {
        use ark_bn254::Fr;
        use num_bigint::BigUint;
        use serde::{Deserialize, Serialize};

        #[derive(Serialize, Deserialize, Eq, PartialEq, Debug)]
        struct TestWrap(#[serde(with = "crate::serde::serde_fr_vec")] pub Vec<Fr>);

        fn sample() -> TestWrap {
            TestWrap(vec![
                BigUint::from(0u8).into(),
                BigUint::from(1u8).into(),
                BigUint::from(123u64).into(),
                BigUint::from(u64::MAX).into(),
            ])
        }

        #[test]
        fn test_serialize_deserialize_json() {
            let v1 = sample();
            let s = serde_json::to_string(&v1).unwrap();
            let v2: TestWrap = serde_json::from_str(&s).unwrap();
            assert_eq!(v1, v2);
        }

        #[test]
        fn test_serialize_deserialize_bin() {
            let v1 = sample();
            let b = bincode::serialize(&v1).unwrap();
            let v2: TestWrap = bincode::deserialize(&b).unwrap();
            assert_eq!(v1, v2);
        }

        #[test]
        fn test_empty_json() {
            let v1 = TestWrap(Vec::new());
            let s = serde_json::to_string(&v1).unwrap();
            let v2: TestWrap = serde_json::from_str(&s).unwrap();
            assert_eq!(v1, v2);
        }

        #[test]
        fn test_empty_bin() {
            let v1 = TestWrap(Vec::new());
            let b = bincode::serialize(&v1).unwrap();
            let v2: TestWrap = bincode::deserialize(&b).unwrap();
            assert_eq!(v1, v2);
        }
    }
}
