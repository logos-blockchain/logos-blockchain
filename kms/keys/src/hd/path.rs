//! HD path
//!
//! Spec: [Wallet Technical Standard: Key Hierarchy](https://lip.logos.co/blockchain/raw/wallet-technical-standard.html#key-hierarchy)

use std::{fmt, str::FromStr};

use arbitrary_int::u31;
use serde::{Deserialize, Serialize};

use crate::hd::{ExtendedSecretKey, HardenedIndex, MasterKey};

/// A path to a leaf of the key hierarchy
///
/// `m/154'/account'/role'/index'`, where every level is hardened.
///
/// A path is serialized in the BIP-32 notation, e.g. `m/154'/0'/0'/0'`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Path {
    /// A leaf holding the keys of a single note.
    Note {
        account: HardenedIndex,
        role: NoteRole,
        index: HardenedIndex,
    },
    /// The leaf holding the voucher master of an account.
    VoucherMaster { account: HardenedIndex },
}

/// The "purpose'" field, fixed to the slug of the spec, as in BIP-43.
const PURPOSE: HardenedIndex = HardenedIndex::new(u31::new(154));

impl Path {
    /// Derives the key at this path from `master`.
    pub(super) fn derive(self, master: &MasterKey) -> ExtendedSecretKey {
        let purpose = master.0.derive_child(PURPOSE);
        match self {
            Self::Note {
                account,
                role,
                index,
            } => purpose
                .derive_child(account)
                .derive_child(Role::Note(role).index())
                .derive_child(index),
            Self::VoucherMaster { account } => purpose
                .derive_child(account)
                .derive_child(Role::VoucherMaster.index()),
        }
    }
}

/// The "role'" field variants dedicated to notes in the spec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteRole {
    /// Notes received from others
    Receive,
    /// Change notes
    Change,
}

/// The "role'" field, which numbers every role defined in the spec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Note(NoteRole),
    VoucherMaster,
}

impl Role {
    const RECEIVE: u32 = 0;
    const CHANGE: u32 = 1;
    const VOUCHER_MASTER: u32 = 2;

    const fn index(self) -> HardenedIndex {
        HardenedIndex::new(u31::new(match self {
            Self::Note(NoteRole::Receive) => Self::RECEIVE,
            Self::Note(NoteRole::Change) => Self::CHANGE,
            Self::VoucherMaster => Self::VOUCHER_MASTER,
        }))
    }

    const fn from_index(index: HardenedIndex) -> Option<Self> {
        match index.child_number().value() {
            Self::RECEIVE => Some(Self::Note(NoteRole::Receive)),
            Self::CHANGE => Some(Self::Note(NoteRole::Change)),
            Self::VOUCHER_MASTER => Some(Self::VoucherMaster),
            _ => None,
        }
    }
}

impl TryFrom<String> for Path {
    type Error = PathError;

    fn try_from(path: String) -> Result<Self, Self::Error> {
        path.parse()
    }
}

impl From<Path> for String {
    fn from(path: Path) -> Self {
        path.to_string()
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "m/{PURPOSE}")?;
        match self {
            Self::Note {
                account,
                role,
                index,
            } => write!(f, "/{account}/{}/{index}", Role::Note(*role).index()),
            Self::VoucherMaster { account } => {
                write!(f, "/{account}/{}", Role::VoucherMaster.index())
            }
        }
    }
}

impl FromStr for Path {
    type Err = PathError;

    fn from_str(path: &str) -> Result<Self, Self::Err> {
        let (account, role, index) = parse_levels(path)?;
        match (Role::from_index(role), index) {
            (Some(Role::Note(role)), Some(index)) => Ok(Self::Note {
                account,
                role,
                index,
            }),
            (Some(Role::Note(_)), None) => Err(PathError::MissingIndex(role)),
            (Some(Role::VoucherMaster), None) => Ok(Self::VoucherMaster { account }),
            (Some(Role::VoucherMaster), Some(_)) => Err(PathError::UnexpectedIndex),
            (None, _) => Err(PathError::ReservedRole(role)),
        }
    }
}

/// Parses `path` into its account, role and optional index, checking the
/// master, purpose and depth on the way.
fn parse_levels(
    path: &str,
) -> Result<(HardenedIndex, HardenedIndex, Option<HardenedIndex>), PathError> {
    let mut levels = path.split('/');
    if levels.next() != Some("m") {
        return Err(PathError::MissingMaster);
    }
    let levels = levels.map(parse_level).collect::<Result<Vec<_>, _>>()?;
    match levels[..] {
        [purpose, ..] if purpose != PURPOSE => Err(PathError::InvalidPurpose(purpose)),
        [_, account, role] => Ok((account, role, None)),
        [_, account, role, index] => Ok((account, role, Some(index))),
        _ => Err(PathError::InvalidDepth(levels.len())),
    }
}

/// Parses a hardened level, written as `3'`.
fn parse_level(level: &str) -> Result<HardenedIndex, PathError> {
    let error = || PathError::InvalidLevel(level.to_owned());
    let number = level.strip_suffix('\'').ok_or_else(error)?;
    if !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(error());
    }
    let number = number.parse::<u32>().map_err(|_| error())?;
    let number = u31::try_new(number).map_err(|_| error())?;
    Ok(HardenedIndex::new(number))
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PathError {
    #[error("path must start with `m/`")]
    MissingMaster,
    #[error("level `{0}` must be a hardened index in `[0, 2^31)`, such as `3'`")]
    InvalidLevel(String),
    #[error("path must have 3 or 4 levels, but has {0}")]
    InvalidDepth(usize),
    #[error("purpose must be {PURPOSE}, but is {0}")]
    InvalidPurpose(HardenedIndex),
    #[error("role {0} is reserved")]
    ReservedRole(HardenedIndex),
    #[error("note role {0} must be followed by an index")]
    MissingIndex(HardenedIndex),
    #[error("voucher master role {role} must not be followed by an index", role = Role::VoucherMaster.index())]
    UnexpectedIndex,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hd::tests::*;

    #[test]
    fn derive_leaf() {
        let leaf = note(NoteRole::Receive, 0).derive(&master());
        assert_eq!(hex::encode(leaf.key), RECEIVE_0_KEY);
        assert_eq!(hex::encode(leaf.chain_code), RECEIVE_0_CHAIN_CODE);

        let leaf = note(NoteRole::Receive, 1).derive(&master());
        assert_eq!(hex::encode(leaf.key), RECEIVE_1_KEY);
        assert_eq!(hex::encode(leaf.chain_code), RECEIVE_1_CHAIN_CODE);

        let leaf = note(NoteRole::Change, 0).derive(&master());
        assert_eq!(hex::encode(leaf.key), CHANGE_0_KEY);
        assert_eq!(hex::encode(leaf.chain_code), CHANGE_0_CHAIN_CODE);

        let leaf = voucher_master().derive(&master());
        assert_eq!(hex::encode(leaf.key), VOUCHER_MASTER_KEY);
        assert_eq!(hex::encode(leaf.chain_code), VOUCHER_MASTER_CHAIN_CODE);
    }

    #[test]
    fn accounts_derive_different_keys() {
        let path = |account| Path::Note {
            account,
            role: NoteRole::Receive,
            index: index(0),
        };
        assert_ne!(
            path(index(0)).derive(&master()).key,
            path(index(1)).derive(&master()).key
        );
    }

    #[test]
    fn display() {
        assert_eq!(note(NoteRole::Receive, 0).to_string(), "m/154'/0'/0'/0'");
        assert_eq!(note(NoteRole::Change, 3).to_string(), "m/154'/0'/1'/3'");
        assert_eq!(voucher_master().to_string(), "m/154'/0'/2'");
    }

    #[test]
    fn parse() {
        assert_eq!("m/154'/0'/0'/0'".parse(), Ok(note(NoteRole::Receive, 0)));
        assert_eq!("m/154'/0'/1'/3'".parse(), Ok(note(NoteRole::Change, 3)));
        assert_eq!("m/154'/0'/2'".parse(), Ok(voucher_master()));
    }

    #[test]
    fn parse_rejects_invalid_paths() {
        assert_eq!(
            "154'/0'/0'/0'".parse::<Path>(),
            Err(PathError::MissingMaster)
        );
        assert_eq!(
            "m/154'/0'/0'/0".parse::<Path>(),
            Err(PathError::InvalidLevel("0".into()))
        );
        assert_eq!(
            "m/154h/0h/0h/0h".parse::<Path>(),
            Err(PathError::InvalidLevel("154h".into()))
        );
        assert_eq!(
            "m/154'/0'/0'/+1'".parse::<Path>(),
            Err(PathError::InvalidLevel("+1'".into()))
        );
        assert_eq!(
            "m/154'/0'/0'/2147483648'".parse::<Path>(),
            Err(PathError::InvalidLevel("2147483648'".into()))
        );
        assert_eq!("m/154'".parse::<Path>(), Err(PathError::InvalidDepth(1)));
        assert_eq!(
            "m/44'/0'/0'/0'".parse::<Path>(),
            Err(PathError::InvalidPurpose(index(44)))
        );
        assert_eq!(
            "m/154'/0'/0'".parse::<Path>(),
            Err(PathError::MissingIndex(index(0)))
        );
        assert_eq!(
            "m/154'/0'/2'/0'".parse::<Path>(),
            Err(PathError::UnexpectedIndex)
        );
        assert_eq!(
            "m/154'/0'/3'".parse::<Path>(),
            Err(PathError::ReservedRole(index(3)))
        );
    }

    #[test]
    fn serialized_as_string() {
        let path = note(NoteRole::Receive, 0);
        assert_eq!(serde_yaml::to_string(&path).unwrap(), "m/154'/0'/0'/0'\n");
        assert_eq!(
            serde_yaml::from_str::<Path>("m/154'/0'/0'/0'").unwrap(),
            path
        );
    }

    const fn note(role: NoteRole, number: u32) -> Path {
        Path::Note {
            account: index(0),
            role,
            index: index(number),
        }
    }

    const fn voucher_master() -> Path {
        Path::VoucherMaster { account: index(0) }
    }
}
