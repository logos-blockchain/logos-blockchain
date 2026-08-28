pub mod merkle;

macro_rules! display_hex_bytes_newtype {
    ($newtype:ty) => {
        impl core::fmt::Display for $newtype {
            fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
                write!(f, "0x")?;
                for v in self.0 {
                    write!(f, "{:02x}", v)?;
                }
                Ok(())
            }
        }
    };
}

macro_rules! serde_bytes_newtype {
    ($newtype:ty, $len:expr) => {
        impl serde::Serialize for $newtype {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                lb_utils::serde::serialize_bytes_array(self.0, serializer)
            }
        }

        impl<'de> serde::Deserialize<'de> for $newtype {
            fn deserialize<D>(deserializer: D) -> Result<$newtype, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                lb_utils::serde::deserialize_bytes_array::<$len, D>(deserializer).map(Self)
            }
        }

        #[cfg(feature = "openapi")]
        impl utoipa::PartialSchema for $newtype {
            fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
                $crate::utils::hex_bytes_schema($len)
            }
        }

        #[cfg(feature = "openapi")]
        impl utoipa::ToSchema for $newtype {}
    };
}

pub(crate) use display_hex_bytes_newtype;
pub(crate) use serde_bytes_newtype;

/// The documented schema for a fixed-size byte value encoded by
/// [`lb_utils::serde::serialize_bytes_array`].
///
/// Human-readable formats encode as unprefixed lowercase hex; the
/// deserializer additionally tolerates a `0x` prefix and uppercase, which the
/// pattern reflects.
#[cfg(feature = "openapi")]
pub(crate) fn hex_bytes_schema(
    bytes: usize,
) -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
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

#[cfg(all(test, feature = "openapi"))]
mod schema_conformance_tests {
    use boon::{Compiler, Schemas};
    use serde::{Deserialize, Serialize};
    use utoipa::PartialSchema;

    // Newtypes declared with the macro under test, one per length that
    // `serialize_bytes_array` treats differently.
    //
    // Every real call site is this same macro with a different `$len`: the
    // documented schema is derived from `$len` and the encoding from
    // `serialize_bytes_array`, both parameterised identically. Covering the
    // length parameter therefore covers every instantiation, so adding a
    // newtype elsewhere in the crate needs no change here.
    //
    // 33 and 64 matter specifically: `serialize_bytes_array` switches to an
    // explicit tuple above 32 elements, because `serde` only implements
    // `Serialize` for arrays up to that size.
    struct Bytes1([u8; 1]);
    struct Bytes16([u8; 16]);
    struct Bytes32([u8; 32]);
    struct Bytes33([u8; 33]);
    struct Bytes64([u8; 64]);

    serde_bytes_newtype!(Bytes1, 1);
    serde_bytes_newtype!(Bytes16, 16);
    serde_bytes_newtype!(Bytes32, 32);
    serde_bytes_newtype!(Bytes33, 33);
    serde_bytes_newtype!(Bytes64, 64);

    /// Asserts that what `serde` emits for `T` validates against the schema
    /// [`serde_bytes_newtype`] documents for it.
    ///
    /// The byte length itself is already tied at compile time — the macro's
    /// `deserialize_bytes_array::<$len, _>(..).map(Self)` will not compile if
    /// `$len` disagrees with the newtype. What this catches is a change to
    /// `lb_utils::serde::serialize_bytes_array`, which lives in another crate
    /// and is not tied to the macro: a different encoding, a different case, or
    /// a `0x` prefix appearing on output would all silently invalidate the
    /// published schema.
    fn assert_serde_output_matches_schema<T>(bytes: usize)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialSchema,
    {
        let raw: Vec<u8> = (0..bytes)
            .map(|byte| u8::try_from(byte % 256).expect("byte fits in u8"))
            .collect();
        let hex = hex::encode(&raw);
        let as_json = serde_json::Value::String(hex.clone());

        let value: T = serde_json::from_value(as_json.clone()).expect("hex string deserializes");
        let emitted = serde_json::to_value(&value).expect("value serializes");
        assert_eq!(
            emitted,
            as_json,
            "round trip changed the encoding for {}",
            std::any::type_name::<T>()
        );

        let schema = serde_json::to_value(T::schema()).expect("schema serializes");
        let mut schemas = Schemas::new();
        let mut compiler = Compiler::new();
        compiler
            .add_resource("schema.json", schema.clone())
            .expect("schema is a valid resource");
        let index = compiler
            .compile("schema.json", &mut schemas)
            .expect("schema compiles");

        assert!(
            schemas.validate(&emitted, index).is_ok(),
            "{} emits {emitted} which does not satisfy its documented schema {schema}",
            std::any::type_name::<T>(),
        );

        // The documented pattern allows an optional `0x` prefix. Hold the
        // deserializer to that, so the schema is not more permissive than the
        // API actually is.
        let prefixed: T = serde_json::from_value(serde_json::Value::String(format!("0x{hex}")))
            .expect("0x-prefixed hex deserializes");
        assert_eq!(
            serde_json::to_value(&prefixed).expect("value serializes"),
            emitted
        );
    }

    #[test]
    fn generated_schemas_match_the_generated_encoding() {
        assert_serde_output_matches_schema::<Bytes1>(1);
        assert_serde_output_matches_schema::<Bytes16>(16);
        assert_serde_output_matches_schema::<Bytes32>(32);
        assert_serde_output_matches_schema::<Bytes33>(33);
        assert_serde_output_matches_schema::<Bytes64>(64);
    }
}
