//! Procedural macros for the unified binary codec
//! (`BinaryEncode`/`BinaryDecode`, crate `lb-codec`).
//!
//! - [`macro@BinaryCodec`] — `#[derive(BinaryCodec)]` for named or tuple
//!   structs. Generates *only* the codec: `BinaryEncode` (field-order
//!   concatenation, with a summed `encoded_length`) and `BinaryDecode` (decode
//!   each field in order with a `()` context, then `Self { .. }` / `Self(..)`).
//!   The decode is infallible positional construction, so a newtype needing a
//!   fallible `try_from` keeps a hand-written impl. The well-known fixture is
//!   supplied separately by [`codec_fixtures!`]; because both codec traits
//!   require `CodecExamples`, a derived type that lacks a `codec_fixtures!`
//!   does not compile.
//! - [`codec_fixtures!`] — the single source of fixtures. Emits the sealed
//!   `CodecExamples` impl and a `#[cfg(test)]` round-trip test for any codec
//!   (derived, hand-written, primitive, or foreign). For codecs whose
//!   `BinaryDecode::Context` is not `()`, pass `context = <expr>` so the
//!   generated test can build a context.
//!
//! Generated code refers to the codec crate by the absolute path
//! `::lb_codec::…`, so the macros expand correctly in any crate that depends
//! on `lb-codec` (the crate itself uses `extern crate self as
//! lb_codec;`).

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::{
    Data, DeriveInput, Expr, Fields, Ident, LitStr, Token, Type,
    parse::{Parse, ParseStream},
    parse_macro_input,
};

/// Derive `BinaryEncode` + `BinaryDecode` for a named or tuple struct with an
/// infallible positional decode and a `()` decode context.
///
/// Both codec traits require `CodecExamples`, so a derived type must also
/// pin its well-known fixture with [`codec_fixtures!`] or it will not
/// compile.
#[proc_macro_derive(BinaryCodec)]
pub fn derive_binary_codec(input: TokenStream) -> TokenStream {
    let parsed_input = parse_macro_input!(input as DeriveInput);
    match expand_derive(&parsed_input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn expand_derive(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let ident = &input.ident;

    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "`#[derive(BinaryCodec)]` does not yet support generic types; use a \
             hand-written impl plus `codec_fixtures!`",
        ));
    }

    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            ident,
            "`#[derive(BinaryCodec)]` can only be derived for structs (for now)",
        ));
    };

    let FieldLayout {
        field_types,
        encode_accessors,
        decode_bindings,
        constructor,
    } = field_layout(ident, &data.fields)?;

    Ok(quote! {
        #[automatically_derived]
        impl ::lb_codec::BinaryEncode for #ident {
            fn encoded_length(&self) -> usize {
                0usize #( + ::lb_codec::BinaryEncode::encoded_length(&#encode_accessors) )*
            }

            fn encode_into(&self, out: &mut ::std::vec::Vec<u8>) {
                #( ::lb_codec::BinaryEncode::encode_into(&#encode_accessors, out); )*
            }
        }

        #[automatically_derived]
        impl ::lb_codec::BinaryDecode for #ident {
            type Context = ();

            fn decode<'input>(
                input: &'input [u8],
                (): &Self::Context,
            ) -> ::core::result::Result<(&'input [u8], Self), ::lb_codec::DecodeError> {
                #(
                    let (input, #decode_bindings) =
                        <#field_types as ::lb_codec::BinaryDecode>::decode(input, &())?;
                )*
                ::core::result::Result::Ok((input, #constructor))
            }
        }
    })
}

/// The per-field tokens the codec impls need: each field's type, the
/// `self.<field>` accessor `encode` reads, the binding `decode` introduces, and
/// how to rebuild `Self` from those bindings.
struct FieldLayout<'a> {
    field_types: Vec<&'a Type>,
    encode_accessors: Vec<TokenStream2>,
    decode_bindings: Vec<Ident>,
    constructor: TokenStream2,
}

/// Compute the [`FieldLayout`] for a struct. Handles named structs
/// (`Self { a, b }`) and tuple structs (`Self(a, b)`) alike; the decode is
/// always the infallible positional form, so a newtype that needs a fallible
/// `try_from` stays on `codec_fixtures!` for now.
fn field_layout<'a>(ident: &Ident, fields: &'a Fields) -> syn::Result<FieldLayout<'a>> {
    Ok(match fields {
        Fields::Named(named) => {
            let decode_bindings: Vec<Ident> = named
                .named
                .iter()
                .map(|field| field.ident.clone().expect("named field has an ident"))
                .collect();
            FieldLayout {
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
                "`#[derive(BinaryCodec)]` cannot be derived for unit structs",
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
            quote!(::std::borrow::Cow::Owned(::lb_codec::decode_fixture_hex(#expr)))
        }
    };
    quote! {
        ::lb_codec::CodecFixture {
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
            ::lb_codec::assert_codec_fixtures::<#ty>();
        },
        (FixtureMode::Both, Some(context)) => quote! {
            ::lb_codec::assert_codec_fixtures_with::<#ty, _>(|| #context);
        },
        (FixtureMode::EncodeOnly, _) => quote! {
            ::lb_codec::assert_codec_fixtures_encode_only::<#ty>();
        },
        (FixtureMode::DecodeOnly, None) => quote! {
            ::lb_codec::assert_codec_fixtures_decode_only::<#ty>();
        },
        (FixtureMode::DecodeOnly, Some(context)) => quote! {
            ::lb_codec::assert_codec_fixtures_decode_only_with::<#ty, _>(|| #context);
        },
    };

    quote! {
        #[automatically_derived]
        impl ::lb_codec::sealed::Sealed for #ty {}

        #[automatically_derived]
        impl ::lb_codec::CodecExamples for #ty {
            fn fixtures() -> ::lb_codec::CodecFixtures<Self> {
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
