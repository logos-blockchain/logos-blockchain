//! Procedural macros for the Mantle wire codec (`NomEncode`/`NomDecode`).
//!
//! Two entry points, both of which make a *well-known fixture* a prerequisite
//! to declaring a codec — so encode/decode cannot be added without a golden
//! vector and a round-trip test:
//!
//! - [`macro@NomCodec`] — `#[derive(NomCodec)]` for named-field structs.
//!   Generates `NomEncode` (field-order concatenation), `NomDecode` (decode
//!   each field in order then `Self { .. }`), the sealed `WireExamples`
//!   fixture, and a `#[cfg(test)]` round-trip test. Omitting `#[nom_fixture]`
//!   is a compile error.
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
};

/// A single parsed well-known fixture: a value expression and its canonical
/// wire bytes (decoded from hex at macro-expansion time).
struct Fixture {
    value: Expr,
    bytes: Vec<u8>,
}

/// Derive `NomEncode` + `NomDecode` + the mandatory `WireExamples` fixture for
/// a named-field struct.
///
/// Requires at least one `#[nom_fixture(value = <expr>, bytes = "<hex>")]`;
/// the first is the canonical fixture, any others become extra fixtures.
#[proc_macro_derive(NomCodec, attributes(nom_fixture))]
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
    let Fields::Named(named) = &data.fields else {
        return Err(syn::Error::new_spanned(
            ident,
            "`#[derive(NomCodec)]` requires a struct with named fields",
        ));
    };

    let field_idents: Vec<&Ident> = named
        .named
        .iter()
        .map(|field| field.ident.as_ref().expect("named field has an ident"))
        .collect();
    let field_types: Vec<&Type> = named.named.iter().map(|field| &field.ty).collect();

    let fixtures = parse_fixtures(&input.attrs)?;
    let Some((canonical, extras)) = fixtures.split_first() else {
        return Err(syn::Error::new_spanned(
            ident,
            "`#[derive(NomCodec)]` requires at least one well-known fixture: add \
             `#[nom_fixture(value = <expr>, bytes = \"<hex>\")]`",
        ));
    };

    let canonical_tokens = fixture_tokens(canonical);
    let extra_tokens = extras.iter().map(fixture_tokens);
    let test_mod = Ident::new(&format!("__nom_codec_fixtures_{ident}"), Span::call_site());

    Ok(quote! {
        #[automatically_derived]
        impl crate::mantle::nom::NomEncode for #ident {
            fn encode(&self) -> ::std::vec::Vec<u8> {
                let mut bytes = ::std::vec::Vec::new();
                #( bytes.extend(crate::mantle::nom::NomEncode::encode(&self.#field_idents)); )*
                bytes
            }
        }

        #[automatically_derived]
        impl crate::mantle::nom::NomDecode for #ident {
            fn decode(bytes: &[u8]) -> ::nom::IResult<&[u8], Self> {
                let input = bytes;
                #(
                    let (input, #field_idents) =
                        <#field_types as crate::mantle::nom::NomDecode>::decode(input)?;
                )*
                ::core::result::Result::Ok((input, Self { #(#field_idents),* }))
            }
        }

        #[automatically_derived]
        impl crate::mantle::nom::sealed::Sealed for #ident {}

        #[automatically_derived]
        impl crate::mantle::nom::WireExamples for #ident {
            fn canonical_fixture() -> crate::mantle::nom::WireFixture<Self> {
                #canonical_tokens
            }
            fn extra_fixtures() -> ::std::vec::Vec<crate::mantle::nom::WireFixture<Self>> {
                ::std::vec![ #(#extra_tokens),* ]
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

/// Collect every `#[nom_fixture(value = …, bytes = "…")]` on the item, in
/// order.
fn parse_fixtures(attrs: &[syn::Attribute]) -> syn::Result<Vec<Fixture>> {
    let mut fixtures = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("nom_fixture") {
            continue;
        }

        let mut value: Option<Expr> = None;
        let mut bytes: Option<Vec<u8>> = None;
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("value") {
                value = Some(meta.value()?.parse()?);
                Ok(())
            } else if meta.path.is_ident("bytes") {
                let lit: LitStr = meta.value()?.parse()?;
                let decoded = hex::decode(lit.value())
                    .map_err(|err| meta.error(format!("`bytes` is not valid hex: {err}")))?;
                bytes = Some(decoded);
                Ok(())
            } else {
                Err(meta.error("expected `value` or `bytes`"))
            }
        })?;

        match (value, bytes) {
            (Some(value), Some(bytes)) => fixtures.push(Fixture { value, bytes }),
            _ => {
                return Err(syn::Error::new_spanned(
                    attr,
                    "`#[nom_fixture]` needs both `value = <expr>` and `bytes = \"<hex>\"`",
                ));
            }
        }
    }
    Ok(fixtures)
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

/// Parsed input of [`wire_fixture!`]: `Type, value_expr, "hex"`.
struct WireFixtureInput {
    ty: Type,
    fixture: Fixture,
}

impl Parse for WireFixtureInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let ty: Type = input.parse()?;
        input.parse::<Token![,]>()?;
        let value: Expr = input.parse()?;
        input.parse::<Token![,]>()?;
        let lit: LitStr = input.parse()?;
        let bytes = hex::decode(lit.value()).map_err(|err| {
            syn::Error::new(lit.span(), format!("`bytes` is not valid hex: {err}"))
        })?;
        let _: Option<Token![,]> = input.parse()?; // tolerate a trailing comma
        Ok(Self {
            ty,
            fixture: Fixture { value, bytes },
        })
    }
}

/// Attach a well-known fixture (and its round-trip test) to a hand-written
/// codec.
///
/// For primitives, foreign types, newtypes, and the element types of the
/// generic blanket impls — anything `#[derive(NomCodec)]` cannot reach.
///
/// ```ignore
/// wire_fixture!(u32, 0x0403_0201_u32, "01020304");
/// ```
#[proc_macro]
pub fn wire_fixture(input: TokenStream) -> TokenStream {
    let WireFixtureInput { ty, fixture } = parse_macro_input!(input as WireFixtureInput);
    let fixture_tokens = fixture_tokens(&fixture);

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
            fn canonical_fixture() -> crate::mantle::nom::WireFixture<Self> {
                #fixture_tokens
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
