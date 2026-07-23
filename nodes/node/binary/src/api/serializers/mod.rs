//! Serializers for API responses.
//! Api*-prefixed types handle serde for their corresponding domain type.
//!
//! Whenever possible they are shadow structs (`#[serde(remote = "...")]`),
//! suffixed `Serializer`.
//! E.g. [`ApiTransactionSerializer`](transactions::ApiTransactionSerializer).
//!
//! Sometimes, due to generic nesting, this can't be achieved (`remote` only
//! substitutes generics flatly).
//! When that happens, the serializer is a plain struct with a handwritten
//! `From<&Foreign>` impl and an inherent `serialize(&Foreign, S)` method
//! instead. These don't have the `Serializer` suffix.
//! E.g. [`ApiBlock`](blocks::ApiBlock).
//! An `Owned` suffix marks the variant that holds the domain value itself, for
//! call sites that can't borrow it long enough (e.g. across an `.await`).
//! E.g. [`ApiBlockOwned`](blocks::ApiBlockOwned).

pub mod blocks;
pub mod transactions;
