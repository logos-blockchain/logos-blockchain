//! `OpenAPI` schema helpers for the wire encodings defined in this crate.

/// The schema for a value encoded by
/// [`serialize_bytes_array`](crate::serde::serialize_bytes_array).
///
/// The single description of that encoding: every type whose `serde` impl
/// routes through `serialize_bytes_array` documents itself with this, so they
/// cannot describe the same wire format differently.
///
/// Human-readable formats encode as unprefixed lowercase hex; the deserializer
/// additionally tolerates a `0x` prefix and uppercase, which the pattern
/// reflects.
#[must_use]
pub fn hex_bytes_schema(bytes: usize) -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
    let hex_len = bytes * 2;
    utoipa::openapi::schema::ObjectBuilder::new()
        .schema_type(utoipa::openapi::schema::Type::String)
        .pattern(Some(format!("^(0x)?[0-9a-fA-F]{{{hex_len}}}$")))
        .min_length(Some(hex_len))
        .max_length(Some(hex_len + 2))
        .description(Some(format!("{bytes}-byte value, hex encoded.")))
        .build()
        .into()
}
