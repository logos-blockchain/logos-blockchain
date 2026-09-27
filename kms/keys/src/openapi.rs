//! `OpenAPI` schemas for the public key material of this crate.
//!
//! Each schema describes the human-readable (JSON) `serde` form of its type.

use std::borrow::Cow;

use lb_utils::openapi::hex_bytes_schema;
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{
        Ref, RefOr, Type,
        schema::{AnyOfBuilder, ArrayBuilder, ObjectBuilder, Schema},
    },
};

use crate::keys::{
    ED25519_PUBLIC_KEY_SIZE, ED25519_SIGNATURE_SIZE, Ed25519PublicKey, Ed25519Signature,
    MAX_ZK_SIGNING_KEYS, UnverifiedEd25519PublicKey, ZkPublicKey, ZkSignature,
};

fn described(schema: RefOr<Schema>, description: &str) -> RefOr<Schema> {
    match schema {
        RefOr::T(Schema::Object(mut object)) => {
            object.description = Some(description.to_owned());
            RefOr::T(Schema::Object(object))
        }
        other => other,
    }
}

impl PartialSchema for UnverifiedEd25519PublicKey {
    fn schema() -> RefOr<Schema> {
        described(
            hex_bytes_schema(ED25519_PUBLIC_KEY_SIZE),
            "Ed25519 public key: 32-byte compressed Edwards point, hex encoded. Must decode to a \
             valid curve point; small-order points are accepted.",
        )
    }
}

impl ToSchema for UnverifiedEd25519PublicKey {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("UnverifiedEd25519PublicKey")
    }
}

impl PartialSchema for Ed25519PublicKey {
    fn schema() -> RefOr<Schema> {
        described(
            hex_bytes_schema(ED25519_PUBLIC_KEY_SIZE),
            "Ed25519 public key: 32-byte compressed Edwards point, hex encoded. Must decode to a \
             valid curve point that is not of small order.",
        )
    }
}

impl ToSchema for Ed25519PublicKey {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("Ed25519PublicKey")
    }
}

impl PartialSchema for Ed25519Signature {
    fn schema() -> RefOr<Schema> {
        described(
            hex_bytes_schema(ED25519_SIGNATURE_SIZE),
            "Ed25519 signature: 64 bytes, hex encoded.",
        )
    }
}

impl ToSchema for Ed25519Signature {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("Ed25519Signature")
    }
}

/// A compressed Groth16 curve point: emitted as unprefixed hex, but also
/// accepted as an array of byte values.
fn compressed_point(bytes: usize) -> RefOr<Schema> {
    AnyOfBuilder::new()
        .item(hex_bytes_schema(bytes))
        .item(
            ArrayBuilder::new()
                .items(
                    ObjectBuilder::new()
                        .schema_type(Type::Integer)
                        .minimum(Some(0))
                        .maximum(Some(255)),
                )
                .min_items(Some(bytes))
                .max_items(Some(bytes)),
        )
        .description(Some(format!(
            "{bytes}-byte compressed curve point. Serialized as unprefixed hex; an array of \
             {bytes} byte values is also accepted on input."
        )))
        .into()
}

impl PartialSchema for ZkSignature {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::Object)
            .description(Some(
                "ZkSign signature: a compressed Groth16 proof over BN254.",
            ))
            .property("pi_a", compressed_point(32))
            .property("pi_b", compressed_point(64))
            .property("pi_c", compressed_point(32))
            .required("pi_a")
            .required("pi_b")
            .required("pi_c")
            .into()
    }
}

impl ToSchema for ZkSignature {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("ZkSignature")
    }
}

/// Schema-only stand-in for [`ZkPublicKeys`](crate::keys::ZkPublicKeys), a
/// bounded vector of [`ZkPublicKey`]s, for use as a `value_type`.
pub struct ZkPublicKeys;

impl PartialSchema for ZkPublicKeys {
    fn schema() -> RefOr<Schema> {
        ArrayBuilder::new()
            .items(Ref::from_schema_name(ZkPublicKey::name()))
            .max_items(Some(MAX_ZK_SIGNING_KEYS))
            .into()
    }
}

impl ToSchema for ZkPublicKeys {
    fn schemas(schemas: &mut Vec<(String, RefOr<Schema>)>) {
        schemas.push((ZkPublicKey::name().into_owned(), ZkPublicKey::schema()));
    }
}
