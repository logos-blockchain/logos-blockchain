//! The address a sender needs to pay a wallet, and its encoding.
//!
//! Spec: [Wallet Technical Standard: Address Encoding](https://lip.logos.co/blockchain/raw/wallet-technical-standard.html#address-encoding)

use lb_groth16::{fr_from_bytes, fr_to_bytes};
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_note_encryption::{ENCAPSULATION_KEY_SIZE, EncapsulationKey};
use thiserror::Error;

const KEY_ONLY_VERSION: u8 = 0x00;
const FULL_VERSION: u8 = 0x01;
const PUBLIC_KEY_SIZE: usize = 32;

const CHARSET: &[u8; 32] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
const CHECKSUM_SIZE: usize = 6;
// The constant of Bech32m, BIP-350.
const BECH32M: u32 = 0x2bc8_30a3;

/// The network an address is for, which its human-readable part names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Network {
    Mainnet,
    Other,
}

impl Network {
    const fn human_readable_part(self) -> &'static str {
        match self {
            Self::Mainnet => "logos",
            Self::Other => "tlogos",
        }
    }
}

/// The public key the notes of a wallet are committed to, and the key their
/// ciphertexts are encrypted to.
///
/// A wallet may hand out its public key alone. A sender holding only that
/// gives the recipient the value and the nonce of a note by other means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Address {
    pub public_key: ZkPublicKey,
    pub encryption_key: Option<EncapsulationKey>,
}

/// Why a string is not an address a sender may create a note from.
#[derive(Error, Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressError {
    #[error("The address mixes upper and lower case")]
    MixedCase,
    #[error("The address is not a Bech32m string with a valid checksum")]
    Checksum,
    #[error("The address is for another network")]
    Network,
    #[error("The address has an unknown version")]
    Version,
    #[error("The address has not the length of its version")]
    Length,
    #[error("The address has an invalid padding")]
    Padding,
    #[error("The public key of the address is not below the modulus")]
    PublicKey,
    #[error("The encryption key of the address is not a valid encapsulation key")]
    EncryptionKey,
}

impl Address {
    /// The Bech32m string of this address on `network`.
    #[must_use]
    pub fn encode(&self, network: Network) -> String {
        let mut bytes = Vec::with_capacity(1 + PUBLIC_KEY_SIZE + ENCAPSULATION_KEY_SIZE);
        match &self.encryption_key {
            None => {
                bytes.push(KEY_ONLY_VERSION);
                bytes.extend(fr_to_bytes(self.public_key.as_fr()));
            }
            Some(encryption_key) => {
                bytes.push(FULL_VERSION);
                bytes.extend(fr_to_bytes(self.public_key.as_fr()));
                bytes.extend(encryption_key.to_bytes());
            }
        }

        let hrp = network.human_readable_part();
        let mut values = bytes_to_values(&bytes);
        let checksum = polymod(hrp, values.iter().copied().chain([0; CHECKSUM_SIZE])) ^ BECH32M;
        values.extend(
            (0..CHECKSUM_SIZE)
                .rev()
                .map(|i| ((checksum >> (5 * i)) & 31) as u8),
        );

        let mut address = String::with_capacity(hrp.len() + 1 + values.len());
        address.push_str(hrp);
        address.push('1');
        address.extend(
            values
                .iter()
                .map(|value| CHARSET[usize::from(*value)] as char),
        );
        address
    }

    /// The address `address` encodes for `network`.
    ///
    /// Every check of the standard is made here: a note committed to a public
    /// key nobody holds is lost.
    pub fn decode(address: &str, network: Network) -> Result<Self, AddressError> {
        let has_lower = address.bytes().any(|byte| byte.is_ascii_lowercase());
        let has_upper = address.bytes().any(|byte| byte.is_ascii_uppercase());
        if has_lower && has_upper {
            return Err(AddressError::MixedCase);
        }
        let address = address.to_ascii_lowercase();

        let (hrp, data) = address.rsplit_once('1').ok_or(AddressError::Checksum)?;
        let values = data
            .bytes()
            .map(|character| {
                CHARSET
                    .iter()
                    .position(|candidate| *candidate == character)
                    .map(|value| value as u8)
            })
            .collect::<Option<Vec<_>>>()
            .ok_or(AddressError::Checksum)?;
        if hrp.is_empty()
            || !hrp.bytes().all(|byte| (33..=126).contains(&byte))
            || values.len() < CHECKSUM_SIZE
            || polymod(hrp, values.iter().copied()) != BECH32M
        {
            return Err(AddressError::Checksum);
        }
        if hrp != network.human_readable_part() {
            return Err(AddressError::Network);
        }

        let bytes = values_to_bytes(&values[..values.len() - CHECKSUM_SIZE])?;
        let (version, keys) = bytes.split_first().ok_or(AddressError::Length)?;
        let expected_size = match *version {
            KEY_ONLY_VERSION => PUBLIC_KEY_SIZE,
            FULL_VERSION => PUBLIC_KEY_SIZE + ENCAPSULATION_KEY_SIZE,
            _ => return Err(AddressError::Version),
        };
        if keys.len() != expected_size {
            return Err(AddressError::Length);
        }

        let (public_key, encryption_key) = keys.split_at(PUBLIC_KEY_SIZE);
        Ok(Self {
            public_key: ZkPublicKey::new(
                fr_from_bytes(public_key).map_err(|_| AddressError::PublicKey)?,
            ),
            encryption_key: if encryption_key.is_empty() {
                None
            } else {
                Some(
                    EncapsulationKey::from_bytes(encryption_key)
                        .map_err(|_| AddressError::EncryptionKey)?,
                )
            },
        })
    }
}

// The Bech32 checksum of the human-readable part followed by `values`.
fn polymod(hrp: &str, values: impl Iterator<Item = u8>) -> u32 {
    const GENERATOR: [u32; 5] = [
        0x3b6a_57b2,
        0x2650_8e6d,
        0x1ea1_19fa,
        0x3d42_33dd,
        0x2a14_62b3,
    ];
    let expanded_hrp = hrp
        .bytes()
        .map(|byte| byte >> 5)
        .chain([0])
        .chain(hrp.bytes().map(|byte| byte & 31));
    expanded_hrp.chain(values).fold(1, |checksum, value| {
        let top = checksum >> 25;
        GENERATOR.iter().enumerate().fold(
            ((checksum & 0x01ff_ffff) << 5) ^ u32::from(value),
            |checksum, (bit, generator)| {
                if (top >> bit) & 1 == 1 {
                    checksum ^ generator
                } else {
                    checksum
                }
            },
        )
    })
}

// The bytes as 5-bit values, most significant bit first, the last value
// padded with zero bits.
fn bytes_to_values(bytes: &[u8]) -> Vec<u8> {
    let mut values = Vec::with_capacity(bytes.len() * 8 / 5 + 1);
    let (mut accumulator, mut bits) = (0u32, 0u32);
    for byte in bytes {
        accumulator = (accumulator << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            values.push(((accumulator >> bits) & 31) as u8);
        }
    }
    if bits > 0 {
        values.push(((accumulator << (5 - bits)) & 31) as u8);
    }
    values
}

// The bytes of 5-bit values, whose padding is below 5 bits and all zero.
fn values_to_bytes(values: &[u8]) -> Result<Vec<u8>, AddressError> {
    let mut bytes = Vec::with_capacity(values.len() * 5 / 8);
    let (mut accumulator, mut bits) = (0u32, 0u32);
    for value in values {
        accumulator = ((accumulator << 5) | u32::from(*value)) & 0x0fff;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            bytes.push(((accumulator >> bits) & 255) as u8);
        }
    }
    if bits >= 5 || accumulator & ((1 << bits) - 1) != 0 {
        return Err(AddressError::Padding);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use lb_note_encryption::DecapsulationKey;

    use super::*;

    // The address of the leaf `m/154'/0'/0'/0'` of the Wallet Technical
    // Standard test vectors.
    const VECTORS: &str = include_str!("../test-vectors/address.json");

    fn vector(name: &str) -> String {
        let vectors: serde_json::Value = serde_json::from_str(VECTORS).unwrap();
        vectors[name].as_str().unwrap().to_owned()
    }

    fn vector_address(full: bool) -> Address {
        let seed: [u8; 32] = hex::decode(vector("sk_enc")).unwrap().try_into().unwrap();
        let k_logos: [u8; 32] = hex::decode(vector("k_logos")).unwrap().try_into().unwrap();
        assert_eq!(
            DecapsulationKey::from_secret_key(&k_logos).encapsulation_key(),
            DecapsulationKey::from_seed(seed).encapsulation_key()
        );
        Address {
            public_key: ZkPublicKey::new(
                fr_from_bytes(&hex::decode(vector("public_key")).unwrap()).unwrap(),
            ),
            encryption_key: full.then(|| DecapsulationKey::from_seed(seed).encapsulation_key()),
        }
    }

    // Replaces the data of `address` by `bytes` under a valid checksum.
    fn with_bytes(bytes: &[u8]) -> String {
        let hrp = Network::Mainnet.human_readable_part();
        let mut values = bytes_to_values(bytes);
        let checksum = polymod(hrp, values.iter().copied().chain([0; CHECKSUM_SIZE])) ^ BECH32M;
        values.extend(
            (0..CHECKSUM_SIZE)
                .rev()
                .map(|i| ((checksum >> (5 * i)) & 31) as u8),
        );
        let data: String = values
            .iter()
            .map(|value| CHARSET[usize::from(*value)] as char)
            .collect();
        format!("{hrp}1{data}")
    }

    #[test]
    fn polymod_accepts_the_valid_strings_of_bip_350() {
        for valid in [
            "a1lqfn3a",
            "abcdef1l7aum6echk45nj3s0wdvt2fg8x9yrzpqzd3ryx",
            "split1checkupstagehandshakeupstreamerranterredcaperredlc445v",
            "?1v759aa",
        ] {
            let (hrp, data) = valid.rsplit_once('1').unwrap();
            let values = data
                .bytes()
                .map(|character| CHARSET.iter().position(|c| *c == character).unwrap() as u8);
            assert_eq!(polymod(hrp, values), BECH32M, "{valid}");
        }
    }

    #[test]
    fn the_addresses_are_those_of_the_wallet_standard_vectors() {
        let key_only = vector_address(false);
        let full = vector_address(true);
        assert_eq!(
            hex::encode(full.encryption_key.as_ref().unwrap().to_bytes()),
            vector("pk_enc")
        );

        assert_eq!(
            key_only.encode(Network::Mainnet),
            vector("address_key_only")
        );
        assert_eq!(
            key_only.encode(Network::Other),
            vector("address_key_only_testnet")
        );
        assert_eq!(full.encode(Network::Mainnet), vector("address_full"));
        assert_eq!(full.encode(Network::Mainnet).len(), 2011);

        assert_eq!(
            Address::decode(&vector("address_key_only"), Network::Mainnet),
            Ok(key_only.clone())
        );
        assert_eq!(
            Address::decode(&vector("address_key_only_testnet"), Network::Other),
            Ok(key_only.clone())
        );
        assert_eq!(
            Address::decode(&vector("address_full"), Network::Mainnet),
            Ok(full)
        );
        // An all upper case string is the same address.
        assert_eq!(
            Address::decode(&vector("address_key_only").to_uppercase(), Network::Mainnet),
            Ok(key_only)
        );
    }

    #[test]
    fn decode_makes_every_rejection_of_the_standard() {
        let key_only = vector("address_key_only");
        let full = vector("address_full");
        let decode = |address: &str| Address::decode(address, Network::Mainnet);

        // The address with the character at `index` passed through `change`.
        let changed = |address: &str, index: usize, change: fn(char) -> char| -> String {
            address
                .chars()
                .enumerate()
                .map(|(i, character)| {
                    if i == index {
                        change(character)
                    } else {
                        character
                    }
                })
                .collect()
        };

        let mixed = changed(&key_only, 8, |character| character.to_ascii_uppercase());
        assert_eq!(decode(&mixed), Err(AddressError::MixedCase));

        // One character changed, in the short form and in the middle of the
        // 2011 characters of the full form.
        for (address, index) in [(&key_only, 20), (&full, 1000)] {
            let altered = changed(
                address,
                index,
                |character| {
                    if character == 'q' { 'p' } else { 'q' }
                },
            );
            assert_eq!(decode(&altered), Err(AddressError::Checksum));
        }
        let mut truncated = key_only.clone();
        truncated.pop();
        assert_eq!(decode(&truncated), Err(AddressError::Checksum));
        assert_eq!(decode("logos"), Err(AddressError::Checksum));

        assert_eq!(
            decode(&vector("address_key_only_testnet")),
            Err(AddressError::Network)
        );
        assert_eq!(
            Address::decode(&key_only, Network::Other),
            Err(AddressError::Network)
        );

        let public_key = hex::decode(vector("public_key")).unwrap();
        let version_and_key = |version: u8| [&[version][..], &public_key].concat();

        assert_eq!(
            decode(&with_bytes(&version_and_key(0x02))),
            Err(AddressError::Version)
        );
        // The full version over the bytes of the key-only form, and the
        // key-only version with one byte more.
        assert_eq!(
            decode(&with_bytes(&version_and_key(0x01))),
            Err(AddressError::Length)
        );
        assert_eq!(
            decode(&with_bytes(&[version_and_key(0x00), vec![0]].concat())),
            Err(AddressError::Length)
        );

        // The modulus itself, the smallest value that is not a field element.
        let modulus = lb_groth16::fr_modulus().to_bytes_le();
        assert_eq!(
            decode(&with_bytes(&[&[0x00][..], &modulus].concat())),
            Err(AddressError::PublicKey)
        );

        // The first coefficient of the ML-KEM key set to 4095, above its
        // modulus.
        let mut encryption_key = hex::decode(vector("pk_enc")).unwrap();
        encryption_key[0] = 0xff;
        encryption_key[1] |= 0x0f;
        assert_eq!(
            decode(&with_bytes(
                &[version_and_key(0x01), encryption_key].concat()
            )),
            Err(AddressError::EncryptionKey)
        );
    }

    #[test]
    fn decode_rejects_a_padding_that_is_not_zero_or_too_long() {
        let hrp = Network::Mainnet.human_readable_part();
        let public_key = hex::decode(vector("public_key")).unwrap();
        let checked = |mut values: Vec<u8>| {
            let checksum = polymod(hrp, values.iter().copied().chain([0; CHECKSUM_SIZE])) ^ BECH32M;
            values.extend(
                (0..CHECKSUM_SIZE)
                    .rev()
                    .map(|i| ((checksum >> (5 * i)) & 31) as u8),
            );
            let data: String = values
                .iter()
                .map(|value| CHARSET[usize::from(*value)] as char)
                .collect();
            format!("{hrp}1{data}")
        };
        // 33 bytes are 264 bits, 53 values with one bit of padding.
        let values = bytes_to_values(&[&[0x00][..], &public_key].concat());
        assert_eq!(values.len(), 53);

        let mut non_zero = values.clone();
        *non_zero.last_mut().unwrap() |= 1;
        assert_eq!(
            Address::decode(&checked(non_zero), Network::Mainnet),
            Err(AddressError::Padding)
        );

        let mut too_long = values;
        too_long.push(0);
        assert_eq!(
            Address::decode(&checked(too_long), Network::Mainnet),
            Err(AddressError::Padding)
        );
    }
}
