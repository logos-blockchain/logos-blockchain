//! Procedural macros for the Mantle wire codec (`NomEncode`/`NomDecode`).
//!
//! Two entry points, both of which make a *well-known fixture* a prerequisite
//! to declaring a codec — so encode/decode cannot be added without a golden
//! vector and a round-trip test:
//!
//! - [`macro@NomCodec`] — `#[derive(NomCodec)]` for named or tuple structs.
//!   Generates `NomEncode` (field-order concatenation), `NomDecode` (decode
//!   each field in order then `Self { .. }` / `Self(..)`), the sealed
//!   `WireExamples` fixtures, and a `#[cfg(test)]` round-trip test. The decode
//!   is infallible positional construction, so newtypes needing a fallible
//!   `try_from` stay on `wire_fixture!`. The fixtures are pinned with a single
//!   `#[nom_fixtures((<value>, "<hex>"), ...)]`; omitting it is a compile
//!   error.
//! - [`wire_fixture!`] — for hand-written / foreign codec impls (primitives,
//!   newtypes, the generic blanket impls' element types). Generates the sealed
//!   `WireExamples` fixture and a round-trip test next to the existing impl.
//!
//! Generated code references the traits via `crate::mantle::nom::…`, so these
//! macros are intended for use *within* the `logos-blockchain-core` crate
//! (matching the existing `kms/macros` convention). Cross-crate use would
//! require switching the anchor to `::logos_blockchain_core::…` plus a
//! re-export shim.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::{
    Data, DeriveInput, Expr, Fields, Ident, LitStr, Token, Type,
    parse::{Parse, ParseStream},
    parse_macro_input,
    punctuated::Punctuated,
};

/// Derive `NomEncode` + `NomDecode` + the mandatory `WireExamples` fixtures for
/// a named or tuple struct with an infallible positional decode.
///
/// Fixtures are pinned with a single attribute holding one or more
/// `(value, "hex")` pairs:
/// `#[nom_fixtures((<expr>, "<hex>"), (<expr>, "<hex>"), ...)]`.
#[proc_macro_derive(NomCodec, attributes(nom_fixtures))]
pub fn derive_nom_codec(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand_derive(&input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn expand_derive(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let ident = &input.ident;

    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "`#[derive(NomCodec)]` does not yet support generic types; use a \
             hand-written impl plus `wire_fixture!`",
        ));
    }

    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            ident,
            "`#[derive(NomCodec)]` can only be derived for structs (for now)",
        ));
    };

    let FieldLayout {
        field_types,
        encode_accessors,
        decode_bindings,
        constructor,
    } = field_layout(ident, &data.fields)?;

    let fixtures = parse_fixtures(ident, &input.attrs)?;
    let fixture_exprs = fixtures.iter().map(fixture_tokens);
    let test_mod = Ident::new(&format!("__nom_codec_fixtures_{ident}"), Span::call_site());

    Ok(quote! {
        #[automatically_derived]
        impl crate::mantle::nom::NomEncode for #ident {
            fn encode(&self) -> ::std::vec::Vec<u8> {
                let mut bytes = ::std::vec::Vec::new();
                #( bytes.extend(crate::mantle::nom::NomEncode::encode(&#encode_accessors)); )*
                bytes
            }
        }

        #[automatically_derived]
        impl crate::mantle::nom::NomDecode for #ident {
            fn decode(bytes: &[u8]) -> ::nom::IResult<&[u8], Self> {
                let input = bytes;
                #(
                    let (input, #decode_bindings) =
                        <#field_types as crate::mantle::nom::NomDecode>::decode(input)?;
                )*
                ::core::result::Result::Ok((input, #constructor))
            }
        }

        #[automatically_derived]
        impl crate::mantle::nom::sealed::Sealed for #ident {}

        #[automatically_derived]
        impl crate::mantle::nom::WireExamples for #ident {
            fn fixtures() -> crate::mantle::nom::WireFixtures<Self> {
                crate::mantle::nom::WireFixtures::<Self>::new_unchecked(
                    ::std::vec![ #(#fixture_exprs),* ]
                )
            }
        }

        #[cfg(test)]
        mod #test_mod {
            use super::*;
            #[test]
            fn wire_fixtures_round_trip() {
                crate::mantle::nom::assert_wire_fixtures::<#ident>();
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
/// `try_from` (e.g. `Locator`) stays on `wire_fixture!` for now.
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
                "`#[derive(NomCodec)]` cannot be derived for unit structs",
            ));
        }
    })
}

/// Parse the single `#[nom_fixtures((<value>, "<hex>"), ...)]` attribute into
/// its list of fixtures (at least one).
fn parse_fixtures(ident: &Ident, attrs: &[syn::Attribute]) -> syn::Result<Vec<Fixture>> {
    let mut found: Option<&syn::Attribute> = None;
    for attr in attrs {
        if attr.path().is_ident("nom_fixtures") {
            if found.is_some() {
                return Err(syn::Error::new_spanned(
                    attr,
                    "expected a single `#[nom_fixtures(...)]` attribute",
                ));
            }
            found = Some(attr);
        }
    }

    let Some(attr) = found else {
        return Err(syn::Error::new_spanned(
            ident,
            "`#[derive(NomCodec)]` requires `#[nom_fixtures((<value>, \"<hex>\"), ...)]` \
             with at least one fixture",
        ));
    };

    let pairs = attr.parse_args_with(Punctuated::<FixturePair, Token![,]>::parse_terminated)?;
    if pairs.is_empty() {
        return Err(syn::Error::new_spanned(
            attr,
            "`#[nom_fixtures]` needs at least one `(<value>, \"<hex>\")` pair",
        ));
    }
    Ok(pairs.into_iter().map(|pair| pair.0).collect())
}

/// One `(<value>, "<hex>")` element of a `#[nom_fixtures(...)]` list.
struct FixturePair(Fixture);

impl Parse for FixturePair {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let content;
        syn::parenthesized!(content in input);
        let value: Expr = content.parse()?;
        content.parse::<Token![,]>()?;
        let lit: LitStr = content.parse()?;
        let _: Option<Token![,]> = content.parse()?; // tolerate a trailing comma in the pair
        let bytes = hex::decode(lit.value()).map_err(|err| {
            syn::Error::new(lit.span(), format!("`bytes` is not valid hex: {err}"))
        })?;
        Ok(Self(Fixture { value, bytes }))
    }
}

/// A single parsed well-known fixture: a value expression and its canonical
/// wire bytes (decoded from hex at macro-expansion time).
struct Fixture {
    value: Expr,
    bytes: Vec<u8>,
}

/// Render a [`Fixture`] into a `WireFixture { .. }` literal. The bytes were
/// decoded at expansion time, so they are emitted as a borrowed `&'static`
/// slice — no runtime hex decoding.
fn fixture_tokens(fixture: &Fixture) -> TokenStream2 {
    let value = &fixture.value;
    let bytes = &fixture.bytes;
    quote! {
        crate::mantle::nom::WireFixture {
            value: #value,
            bytes: ::std::borrow::Cow::Borrowed(&[ #(#bytes),* ]),
        }
    }
}

/// Attach well-known fixtures (and a round-trip test) to a hand-written codec.
///
/// For primitives, foreign types, newtypes, and the element types of the
/// generic blanket impls — anything `#[derive(NomCodec)]` cannot reach. Takes
/// one or more `value => "hex"` pairs.
///
/// ```ignore
/// wire_fixture!(u32, 0x0403_0201_u32 => "01020304");
/// wire_fixture!(u8, 0x07_u8 => "07", 0x00_u8 => "00");
/// ```
#[proc_macro]
pub fn wire_fixture(input: TokenStream) -> TokenStream {
    let WireFixtureInput { ty, fixtures } = parse_macro_input!(input as WireFixtureInput);
    let fixture_exprs = fixtures.iter().map(fixture_tokens);

    let sanitized: String = quote!(#ty)
        .to_string()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let test_mod = Ident::new(&format!("__nom_fixture_{sanitized}"), Span::call_site());

    quote! {
        #[automatically_derived]
        impl crate::mantle::nom::sealed::Sealed for #ty {}

        #[automatically_derived]
        impl crate::mantle::nom::WireExamples for #ty {
            fn fixtures() -> crate::mantle::nom::WireFixtures<Self> {
                crate::mantle::nom::WireFixtures::<Self>::new_unchecked(
                    ::std::vec![ #(#fixture_exprs),* ]
                )
            }
        }

        #[cfg(test)]
        mod #test_mod {
            use super::*;
            #[test]
            fn wire_fixture_round_trip() {
                crate::mantle::nom::assert_wire_fixtures::<#ty>();
            }
        }
    }
    .into()
}

/// Parsed input of [`wire_fixture!`]: `Type, value => "hex", value => "hex", …`
/// — at least one `value => "hex"` pair.
struct WireFixtureInput {
    ty: Type,
    fixtures: Vec<Fixture>,
}

impl Parse for WireFixtureInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let ty: Type = input.parse()?;
        input.parse::<Token![,]>()?;

        let mut fixtures = Vec::new();
        while !input.is_empty() {
            let value: Expr = input.parse()?;
            input.parse::<Token![=>]>()?;
            let lit: LitStr = input.parse()?;
            let bytes = hex::decode(lit.value()).map_err(|err| {
                syn::Error::new(lit.span(), format!("`bytes` is not valid hex: {err}"))
            })?;
            fixtures.push(Fixture { value, bytes });

            if input.is_empty() {
                break;
            }
            input.parse::<Token![,]>()?; // separator (a trailing comma ends the loop)
        }

        if fixtures.is_empty() {
            return Err(input.error("`wire_fixture!` needs at least one `value => \"hex\"` pair"));
        }
        Ok(Self { ty, fixtures })
    }
}
