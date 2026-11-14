//! Procedural macros for the unified binary codec
//! (`BinaryEncode`/`BinaryDecode`, crate `lb-binary-codec`).
//!
//! - [`macro@BinaryEncode`] — `#[derive(BinaryEncode)]` for named or tuple
//!   structs. Generates *only* the encoder: field-order concatenation, with a
//!   summed `encoded_length`.
//! - [`macro@BinaryDecode`] — `#[derive(BinaryDecode)]` for named or tuple
//!   structs. Generates *only* the decoder: decode each field in order, then
//!   `Self { .. }` / `Self(..)`. Fields take a `()` context, except the one
//!   marked `#[context]`, which takes the struct's. With `#[decode(try_from =
//!   "Raw")]` or `#[decode(from = "Raw")]`, a type decodes as `Raw` and
//!   converts from it instead. A type needing several contexts keeps a
//!   hand-written `BinaryDecode`, and can still derive `BinaryEncode`.
//! - [`codec_fixtures!`] — the single source of fixtures. Emits the sealed
//!   `CodecExamples` impl and a `#[cfg(test)]` round-trip test for any codec
//!   (derived, hand-written, primitive, or foreign). For codecs whose
//!   `BinaryDecode::Context` is not `()`, pass `context = <expr>` so the
//!   generated test can build a context. Because both codec traits require
//!   `CodecExamples`, a derived type that lacks a `codec_fixtures!` does not
//!   compile.
//!
//! Generated code refers to the codec crate by the absolute path
//! `::lb_binary_codec::canonical::…`, so the macros expand correctly in any
//! crate that depends on `lb-binary-codec` (the crate itself aliases its root
//! as `lb_binary_codec`).

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{quote, quote_spanned};
use syn::{
    Attribute, Data, DeriveInput, Expr, Field, Fields, Ident, LitStr, Token, Type,
    parse::{Parse, ParseStream},
    parse_macro_input,
    spanned::Spanned as _,
};

/// Derive `BinaryEncode` for a named or tuple struct: its fields in declaration
/// order, each through its own `BinaryEncode`.
///
/// `BinaryEncode` requires `CodecExamples`, so a derived type must also pin its
/// well-known fixture with [`codec_fixtures!`] or it will not compile.
#[proc_macro_derive(BinaryEncode)]
pub fn derive_binary_encode(input: TokenStream) -> TokenStream {
    let parsed_input = parse_macro_input!(input as DeriveInput);
    match expand_encode(&parsed_input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// Derive `BinaryDecode` for a named or tuple struct with an infallible
/// positional decode.
///
/// Fields decode in declaration order with a `()` context, and the struct takes
/// `()` too. A field that needs a context is marked `#[context]`: the struct
/// then takes that field's context and passes it down.
///
/// ```ignore
/// // Takes the context of `UncleHeaders`, a `u8`.
/// #[derive(BinaryEncode, BinaryDecode)]
/// struct Proposal {
///     header: Header,
///     #[context]
///     uncle_headers: UncleHeaders,
///     signature: Ed25519Signature,
/// }
/// ```
///
/// The marks are checked at compile time: an unmarked field with a context, a
/// marked one without any and a second mark are all errors. A struct whose
/// fields need several contexts hand-writes its decoder around a named one.
///
/// A type that validates or converts what it reads decodes through a raw type
/// instead, the way `#[serde(try_from = "Raw")]` deserializes. It takes the raw
/// type's context, and can be an enum, since its fields play no part. A
/// `try_from` that fails is a `DecodeError::InvalidValue`. `Raw` must be as
/// visible as the type: the impl's `Context` names it.
///
/// ```ignore
/// #[derive(BinaryDecode)]
/// #[decode(try_from = "[u8; PROOF_OF_QUOTA_SIZE]")]
/// struct ProofOfQuota { .. }
///
/// #[derive(BinaryDecode)]
/// #[decode(from = "u64")]
/// struct Nonce { .. }
/// ```
///
/// `BinaryDecode` requires `CodecExamples`, so a derived type must also pin its
/// well-known fixture with [`codec_fixtures!`] or it will not compile.
#[proc_macro_derive(BinaryDecode, attributes(context, decode))]
pub fn derive_binary_decode(input: TokenStream) -> TokenStream {
    let parsed_input = parse_macro_input!(input as DeriveInput);
    match expand_decode(&parsed_input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn expand_encode(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let ident = &input.ident;
    let encode_accessors = field_layout(input, "BinaryEncode")?.encode_accessors;

    Ok(quote! {
        #[automatically_derived]
        impl ::lb_binary_codec::canonical::BinaryEncode for #ident {
            fn encoded_length(&self) -> usize {
                0usize #( + ::lb_binary_codec::canonical::BinaryEncode::encoded_length(&#encode_accessors) )*
            }

            fn encode_into(&self, out: &mut ::std::vec::Vec<u8>) {
                #( ::lb_binary_codec::canonical::BinaryEncode::encode_into(&#encode_accessors, out); )*
            }
        }
    })
}

fn expand_decode(input: &DeriveInput) -> syn::Result<TokenStream2> {
    if let Some(conversion) = decode_conversion(input)? {
        return expand_conversion(input, &conversion);
    }

    let ident = &input.ident;
    let FieldLayout {
        fields,
        field_types,
        decode_bindings,
        constructor,
        ..
    } = field_layout(input, "BinaryDecode")?;

    let context_type = context_field(fields)?.map(|field| &field.ty);
    let struct_context = context_type.map_or_else(
        || quote!(()),
        |ty| quote!(<#ty as ::lb_binary_codec::canonical::BinaryDecode>::Context),
    );
    let context_parameter = context_type.map_or_else(|| quote!(()), |_| quote!(context));
    // Spanned on the field, so that a field whose context is not `()` and which
    // is not marked fails there rather than on the derive.
    let field_contexts = fields.iter().map(|field| {
        if context_attribute(field).is_some() {
            quote_spanned!(field.ty.span()=> context)
        } else {
            quote_spanned!(field.ty.span()=> &())
        }
    });
    let marked_field_check = context_type.map(|ty| {
        quote_spanned! {ty.span()=>
            const _: () = assert!(
                ::core::mem::size_of::<<#ty as ::lb_binary_codec::canonical::BinaryDecode>::Context>() != 0,
                "`#[context]` marks a field that takes no context",
            );
        }
    });

    Ok(quote! {
        #marked_field_check

        #[automatically_derived]
        impl ::lb_binary_codec::canonical::BinaryDecode for #ident {
            type Context = #struct_context;

            fn decode<'input>(
                input: &'input [u8],
                #context_parameter: &Self::Context,
            ) -> ::core::result::Result<(&'input [u8], Self), ::lb_binary_codec::canonical::DecodeError> {
                #(
                    let (input, #decode_bindings) =
                        <#field_types as ::lb_binary_codec::canonical::BinaryDecode>::decode(input, #field_contexts)?;
                )*
                ::core::result::Result::Ok((input, #constructor))
            }
        }
    })
}

/// What `#[decode(..)]` asks of a type: to decode as `raw`, then convert.
struct Conversion {
    raw: Type,
    fallible: bool,
}

/// The conversion `#[decode(try_from = "Raw")]` or `#[decode(from = "Raw")]`
/// asks for, if any.
fn decode_conversion(input: &DeriveInput) -> syn::Result<Option<Conversion>> {
    let Some(attribute) = input
        .attrs
        .iter()
        .find(|attribute| attribute.path().is_ident("decode"))
    else {
        return Ok(None);
    };

    let mut conversion = None;
    attribute.parse_nested_meta(|meta| {
        let fallible = meta.path.is_ident("try_from");
        if !fallible && !meta.path.is_ident("from") {
            return Err(meta.error("expected `try_from` or `from`"));
        }
        if conversion.is_some() {
            return Err(meta.error("a type decodes through one conversion only"));
        }
        let raw: LitStr = meta.value()?.parse()?;
        conversion = Some(Conversion {
            raw: raw.parse()?,
            fallible,
        });
        Ok(())
    })?;

    conversion.map(Some).ok_or_else(|| {
        syn::Error::new_spanned(
            attribute,
            "expected `#[decode(try_from = \"Raw\")]` or `#[decode(from = \"Raw\")]`",
        )
    })
}

fn expand_conversion(
    input: &DeriveInput,
    Conversion { raw, fallible }: &Conversion,
) -> syn::Result<TokenStream2> {
    reject_generics(input, "BinaryDecode")?;

    let ident = &input.ident;
    let value = if *fallible {
        quote! {
            <Self as ::core::convert::TryFrom<#raw>>::try_from(raw).map_err(|_| {
                ::lb_binary_codec::canonical::DecodeError::invalid_value::<Self>(
                    "its conversion from the decoded value failed",
                )
            })?
        }
    } else {
        quote!(<Self as ::core::convert::From<#raw>>::from(raw))
    };

    Ok(quote! {
        #[automatically_derived]
        impl ::lb_binary_codec::canonical::BinaryDecode for #ident {
            type Context = <#raw as ::lb_binary_codec::canonical::BinaryDecode>::Context;

            fn decode<'input>(
                input: &'input [u8],
                context: &Self::Context,
            ) -> ::core::result::Result<(&'input [u8], Self), ::lb_binary_codec::canonical::DecodeError> {
                let (input, raw) =
                    <#raw as ::lb_binary_codec::canonical::BinaryDecode>::decode(input, context)?;
                ::core::result::Result::Ok((input, #value))
            }
        }
    })
}

/// The `#[context]` attribute of `field`, if it has one.
fn context_attribute(field: &Field) -> Option<&Attribute> {
    field
        .attrs
        .iter()
        .find(|attribute| attribute.path().is_ident("context"))
}

/// The field marked `#[context]`, if any. Only one can be: a struct passes its
/// whole context down, so several fields would need it split up, and a tuple
/// says nothing of what each of its values means.
fn context_field(fields: &Fields) -> syn::Result<Option<&Field>> {
    let mut marked = fields
        .iter()
        .filter_map(|field| Some((field, context_attribute(field)?)));
    let Some((field, attribute)) = marked.next() else {
        return Ok(None);
    };
    attribute.meta.require_path_only()?;
    if let Some((_, second)) = marked.next() {
        return Err(syn::Error::new_spanned(
            second,
            "only one field can take the struct's context; hand-write `BinaryDecode` for a \
             struct whose fields need several",
        ));
    }
    Ok(Some(field))
}

/// The per-field tokens the codec derives need: the fields themselves, each
/// field's type, the `self.<field>` accessor `encode` reads, the binding
/// `decode` introduces, and how to rebuild `Self` from those bindings.
struct FieldLayout<'a> {
    fields: &'a Fields,
    field_types: Vec<&'a Type>,
    encode_accessors: Vec<TokenStream2>,
    decode_bindings: Vec<Ident>,
    constructor: TokenStream2,
}

fn reject_generics(input: &DeriveInput, derive: &str) -> syn::Result<()> {
    if input.generics.params.is_empty() {
        return Ok(());
    }
    Err(syn::Error::new_spanned(
        &input.generics,
        format!(
            "`#[derive({derive})]` does not yet support generic types; use a hand-written impl \
             plus `codec_fixtures!`"
        ),
    ))
}

/// Compute the [`FieldLayout`] for the struct `derive` is applied to. Handles
/// named structs (`Self { a, b }`) and tuple structs (`Self(a, b)`) alike; the
/// decode is always the infallible positional form, so a type that validates
/// or converts what it reads goes through `#[decode(..)]` instead.
fn field_layout<'a>(input: &'a DeriveInput, derive: &str) -> syn::Result<FieldLayout<'a>> {
    let ident = &input.ident;

    reject_generics(input, derive)?;

    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            ident,
            format!("`#[derive({derive})]` can only be derived for structs (for now)"),
        ));
    };

    Ok(match &data.fields {
        Fields::Named(named) => {
            let decode_bindings: Vec<Ident> = named
                .named
                .iter()
                .map(|field| field.ident.clone().expect("named field has an ident"))
                .collect();
            FieldLayout {
                fields: &data.fields,
                field_types: named.named.iter().map(|field| &field.ty).collect(),
                encode_accessors: decode_bindings.iter().map(|id| quote!(self.#id)).collect(),
                constructor: quote!(Self { #(#decode_bindings),* }),
                decode_bindings,
            }
        }
        Fields::Unnamed(unnamed) => {
            let decode_bindings: Vec<Ident> = (0..unnamed.unnamed.len())
                .map(|i| Ident::new(&format!("field{i}"), Span::call_site()))
                .collect();
            FieldLayout {
                fields: &data.fields,
                field_types: unnamed.unnamed.iter().map(|field| &field.ty).collect(),
                encode_accessors: (0..unnamed.unnamed.len())
                    .map(|i| {
                        let index = syn::Index::from(i);
                        quote!(self.#index)
                    })
                    .collect(),
                constructor: quote!(Self( #(#decode_bindings),* )),
                decode_bindings,
            }
        }
        Fields::Unit => {
            return Err(syn::Error::new_spanned(
                ident,
                format!("`#[derive({derive})]` cannot be derived for unit structs"),
            ));
        }
    })
}

/// A single parsed well-known fixture: a value (or, for `decode_only`, the
/// expected reference value the bytes decode into) and its exact, pinned
/// encoding bytes.
struct Fixture {
    value: Expr,
    bytes: FixtureBytes,
}

/// The well-known bytes of a fixture, always supplied as a hex string.
enum FixtureBytes {
    /// A hex string literal (`=> "01ff"`), decoded to bytes at expansion time.
    Literal(Vec<u8>),
    /// A `&str` expression that is a hex string (`=> include_str!("big.hex")`),
    /// decoded at runtime — for components too large for an inline literal.
    HexStr(Expr),
}

/// Render a [`Fixture`] into a `CodecFixture { .. }` expression. Both byte
/// forms are pinned (well-known) hex — the bytes are never computed from
/// `encode()`.
fn fixture_tokens(fixture: &Fixture) -> TokenStream2 {
    let value = &fixture.value;
    let bytes = match &fixture.bytes {
        FixtureBytes::Literal(bytes) => quote!(::std::borrow::Cow::Borrowed(&[ #(#bytes),* ])),
        FixtureBytes::HexStr(expr) => {
            quote!(::std::borrow::Cow::Owned(::lb_binary_codec::canonical::decode_fixture_hex(#expr)))
        }
    };
    quote! {
        ::lb_binary_codec::canonical::CodecFixture {
            value: #value,
            bytes: #bytes,
        }
    }
}

/// Attach well-known fixtures (and a test) to a hand-written codec.
///
/// Every fixture pins the exact encoded bytes so a codec change that alters
/// the format is caught. The test the macro generates depends on which traits
/// the type implements:
///
/// - default (`BinaryEncode` + `BinaryDecode`): golden encode + golden decode +
///   round-trip.
/// - `encode_only,` (only `BinaryEncode`): encode the value and compare to the
///   bytes.
/// - `decode_only,` (only `BinaryDecode`): decode the bytes and compare to the
///   value (used as the reference).
///
/// Prefix with `context = <expr>,` for a codec whose `BinaryDecode::Context` is
/// not `()` (not valid with `encode_only`). Each entry's bytes are a hex
/// string: an inline literal, or a `&str` expression like
/// `include_str!("big.hex")` for components too large for an inline literal.
///
/// ```ignore
/// codec_fixtures!(u8, 0x07_u8 => "07", 0x00_u8 => "00");
/// codec_fixtures!(VerifiedPublicHeader, encode_only, header() => "0100…");
/// codec_fixtures!(EncapsulatedMessage, decode_only, context = layers(), msg() => include_str!("message.hex"));
/// ```
#[proc_macro]
pub fn codec_fixtures(input: TokenStream) -> TokenStream {
    let CodecFixtureInput {
        ty,
        mode,
        context,
        fixtures,
    } = parse_macro_input!(input as CodecFixtureInput);
    let fixture_exprs = fixtures.iter().map(fixture_tokens);

    let sanitized: String = quote!(#ty)
        .to_string()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let test_mod = Ident::new(
        &format!("__encoding_fixture_{sanitized}"),
        Span::call_site(),
    );

    let round_trip = match (mode, context) {
        (FixtureMode::Both, None) => quote! {
            ::lb_binary_codec::canonical::assert_codec_fixtures::<#ty>();
        },
        (FixtureMode::Both, Some(context)) => quote! {
            ::lb_binary_codec::canonical::assert_codec_fixtures_with::<#ty, _>(|| #context);
        },
        (FixtureMode::EncodeOnly, _) => quote! {
            ::lb_binary_codec::canonical::assert_codec_fixtures_encode_only::<#ty>();
        },
        (FixtureMode::DecodeOnly, None) => quote! {
            ::lb_binary_codec::canonical::assert_codec_fixtures_decode_only::<#ty>();
        },
        (FixtureMode::DecodeOnly, Some(context)) => quote! {
            ::lb_binary_codec::canonical::assert_codec_fixtures_decode_only_with::<#ty, _>(|| #context);
        },
    };

    quote! {
        #[automatically_derived]
        impl ::lb_binary_codec::canonical::sealed::Sealed for #ty {}

        #[automatically_derived]
        impl ::lb_binary_codec::canonical::CodecExamples for #ty {
            fn fixtures() -> ::lb_binary_codec::canonical::CodecFixtures<Self> {
                [ #(#fixture_exprs),* ].into()
            }
        }

        #[cfg(test)]
        mod #test_mod {
            #[allow(unused_imports)]
            use super::*;

            #[test]
            fn codec_fixtures_round_trip() {
                #round_trip
            }
        }
    }
    .into()
}

/// Which pair of traits the codec implements — selects the generated test.
enum FixtureMode {
    /// Implements both `BinaryEncode` and `BinaryDecode`.
    Both,
    /// Implements only `BinaryEncode`.
    EncodeOnly,
    /// Implements only `BinaryDecode`.
    DecodeOnly,
}

/// Parsed input of [`codec_fixtures!`]: `Type, [encode_only,|decode_only,]
/// [context = <expr>,] value => "hex"|&expr, …` — at least one entry.
struct CodecFixtureInput {
    ty: Type,
    mode: FixtureMode,
    context: Option<Expr>,
    fixtures: Vec<Fixture>,
}

impl Parse for CodecFixtureInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let ty: Type = input.parse()?;
        input.parse::<Token![,]>()?;

        // Optional mode selector: `encode_only,` or `decode_only,`.
        let mode = if input.peek(Ident) && input.peek2(Token![,]) {
            let keyword: Ident = input.parse()?;
            let mode = match keyword.to_string().as_str() {
                "encode_only" => FixtureMode::EncodeOnly,
                "decode_only" => FixtureMode::DecodeOnly,
                _ => {
                    return Err(syn::Error::new_spanned(
                        &keyword,
                        "expected `encode_only`, `decode_only`, `context = <expr>`, or a `value => bytes` entry",
                    ));
                }
            };
            input.parse::<Token![,]>()?;
            mode
        } else {
            FixtureMode::Both
        };

        // Optional `context = <expr>,` for a non-`()` decode context.
        let context = if input.peek(Ident) && input.peek2(Token![=]) && !input.peek2(Token![==]) {
            let keyword: Ident = input.parse()?;
            if keyword != "context" {
                return Err(syn::Error::new_spanned(
                    &keyword,
                    "expected `context = <expr>` or a `value => bytes` entry",
                ));
            }
            input.parse::<Token![=]>()?;
            let expr: Expr = input.parse()?;
            input.parse::<Token![,]>()?;
            Some(expr)
        } else {
            None
        };

        if matches!(mode, FixtureMode::EncodeOnly) && context.is_some() {
            return Err(input.error("`encode_only` fixtures have no decode context"));
        }

        let mut fixtures = Vec::new();
        while !input.is_empty() {
            let value: Expr = input.parse()?;
            input.parse::<Token![=>]>()?;
            // RHS bytes: a hex string literal, or a `&str` hex expression such
            // as `include_str!("big.hex")` (decoded at runtime).
            let bytes = if input.peek(LitStr) {
                let lit: LitStr = input.parse()?;
                FixtureBytes::Literal(hex::decode(lit.value()).map_err(|err| {
                    syn::Error::new(lit.span(), format!("fixture is not valid hex: {err}"))
                })?)
            } else {
                FixtureBytes::HexStr(input.parse()?)
            };
            fixtures.push(Fixture { value, bytes });

            if input.is_empty() {
                break;
            }
            input.parse::<Token![,]>()?; // separator (a trailing comma ends the
            // loop)
        }

        if fixtures.is_empty() {
            return Err(input.error("`codec_fixtures!` needs at least one `value => bytes` entry"));
        }
        Ok(Self {
            ty,
            mode,
            context,
            fixtures,
        })
    }
}
