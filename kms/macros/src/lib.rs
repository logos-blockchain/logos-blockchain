use proc_macro2::{Ident, Span, TokenStream};
use quote::quote;
use syn::{
    Attribute, Data, DeriveInput, Error, Field, FieldMutability, Fields, FieldsUnnamed, Type,
    Variant, Visibility, parse_quote,
    punctuated::{Pair, Punctuated},
    token::{Comma, Paren},
};

#[proc_macro_derive(KmsEnumKey)]
pub fn derive_kms_enum_key(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    let Data::Enum(key_enum) = &input.data else {
        return Error::new_spanned(input.ident, "KmsEnumKey can only be derived for enums")
            .to_compile_error()
            .into();
    };

    // If there are no variants, do nothing. This won't implement `SecuredKey` or
    // the encodings, but it will allow the crate to be compiled when no keys
    // are enabled.
    if key_enum.variants.is_empty() {
        return quote! {}.into();
    }

    if let Some(err) = validate_variants(&key_enum.variants) {
        return err.to_compile_error().into();
    }

    let key_enum_variants = &key_enum.variants;
    let encoding_enums = build_encoding_enums(key_enum_variants);
    let key_enum_impl_secured_key =
        build_key_enum_impl_secured_key(&input.ident, key_enum_variants);
    let operators_enum = build_operators_enum(&input.ident, key_enum_variants);
    quote! {
        #encoding_enums
        #operators_enum
        #key_enum_impl_secured_key
    }
    .into()
}

/// Validate each variant must have exactly one unnamed field (the key type).
fn validate_variants(key_enum_variants: &Punctuated<Variant, Comma>) -> Option<Error> {
    for variant in key_enum_variants {
        if !matches!(&variant.fields, Fields::Unnamed(f) if f.unnamed.len() == 1) {
            return Some(Error::new_spanned(
                variant,
                "KmsEnumKey expects each variant to have exactly one unnamed field (the key type)",
            ));
        }
    }
    None
}

fn build_encoding_enums(key_enum_variants: &Punctuated<Variant, Comma>) -> TokenStream {
    let payload_encoding_type_ident = &Ident::new("Payload", Span::call_site());
    let signature_encoding_type_ident = &Ident::new("Signature", Span::call_site());
    let public_key_encoding_type_ident = &Ident::new("PublicKey", Span::call_site());

    let payload_encoding_enum = build_encoding_enum(payload_encoding_type_ident, key_enum_variants);
    let signature_encoding_enum =
        build_encoding_enum(signature_encoding_type_ident, key_enum_variants);
    let public_key_encoding_enum =
        build_encoding_enum(public_key_encoding_type_ident, key_enum_variants);

    quote! {
        #payload_encoding_enum
        #signature_encoding_enum
        #public_key_encoding_enum
    }
}

fn build_operators_enum(
    base_enum_ident: &Ident,
    variants: &Punctuated<Variant, Comma>,
) -> TokenStream {
    let operator_enum_ident = Ident::new(&format!("{base_enum_ident}Operators"), Span::call_site());
    let operators_variants = variants.iter().map(|variant| {
        let variant_name = &variant.ident;
        let Fields::Unnamed(variant_inner_type) = &variant.fields else {
            panic!("Only unnamed fields are supported for variants. Ex: `Foo(Bar)`");
        };
        let variant_ident = variant_inner_type.unnamed.first().unwrap();
        quote! {
        #variant_name(crate::keys::secured_key::BoxedSecureKeyOperator<#
        variant_ident>) }
    });

    let operations_impl =
        build_operations_impl_for_enum(&operator_enum_ident, base_enum_ident, variants);

    quote! {
        #[derive(Debug)]
        pub enum #operator_enum_ident {
            #(#operators_variants),*
        }

        #operations_impl
    }
}

fn build_operations_impl_for_enum(
    enum_ident: &Ident,
    keys_idents: &Ident,
    variants: &Punctuated<Variant, Comma>,
) -> TokenStream {
    let variants: Vec<TokenStream> = variants
        .iter()
        .map(|variant| {
            let variant = &variant.ident;
            quote! { (Self::#variant(inner), #keys_idents::#variant(key)) => inner.execute(key).await }
        })
        .collect();

    quote! {
        #[async_trait::async_trait]
        impl crate::keys::secured_key::SecureKeyOperator for #enum_ident {
            type Key = #keys_idents;
            type Error = crate::keys::errors::KeyError;
            async fn execute(self: Box<Self>, key: &Self::Key) -> Result<(), Self::Error> {
                match (*self, key) {
                    #(#variants,)*
                    (operator, key) => Err(crate::keys::errors::KeyError::UnsupportedKeyOperator {
                        operator: format!("{operator:?}"),
                        key: format!("{key:?}"),
                    }),
                }
            }
        }
    }
}

fn build_encoding_enum(
    encoding_type: &Ident,
    key_enum_variants: &Punctuated<Variant, Comma>,
) -> TokenStream {
    let encoding_enum_ident = Ident::new(&format!("{encoding_type}Encoding"), Span::call_site());
    let encoding_enum_variants = key_enum_variants
        .iter()
        .map(|variant| build_enum_variant_for_associated_type(encoding_type, variant));

    quote! {
        #[derive(Debug, PartialEq, Eq, Clone)]
        pub enum #encoding_enum_ident {
            #(#encoding_enum_variants),*
        }
    }
}

fn build_enum_variant_for_associated_type(
    associated_type: &Ident,
    key_enum_variant: &Variant,
) -> Variant {
    let variant_ident = key_enum_variant.ident.clone();
    let variant_attributes_cfg: Vec<Attribute> =
        get_cfg_attributes(&key_enum_variant.attrs).collect();

    let wrapped_key = get_wrapped_key_as_ref(key_enum_variant);
    let wrapped_key_type = &wrapped_key.ty;
    let wrapped_key_encoding_type: Type =
        parse_quote!(<#wrapped_key_type as crate::keys::secured_key::SecuredKey>::#associated_type);

    let wrapped_type_field = Field {
        attrs: vec![],
        vis: Visibility::Inherited,
        mutability: FieldMutability::None,
        ident: None,
        colon_token: None,
        ty: wrapped_key_encoding_type,
    };
    let unnamed = Punctuated::<Field, _>::from_iter(vec![Pair::End(wrapped_type_field)]);
    let field = Fields::Unnamed(FieldsUnnamed {
        paren_token: Paren::default(),
        unnamed,
    });
    Variant {
        attrs: variant_attributes_cfg,
        ident: variant_ident,
        fields: field,
        discriminant: None,
    }
}

fn build_key_enum_impl_secured_key(
    ident: &Ident,
    key_enum_variants: &Punctuated<Variant, Comma>,
) -> TokenStream {
    let key_enum_impl_secured_key_method_sign =
        build_key_enum_impl_secured_key_method_sign(key_enum_variants);
    let key_enum_impl_secured_key_method_sign_multiple =
        build_key_enum_impl_secured_key_method_sign_multiple(key_enum_variants);
    let key_enum_impl_secured_key_method_as_public_key =
        build_key_enum_impl_secured_key_method_as_public_key(key_enum_variants);

    quote! {
        impl crate::keys::secured_key::SecuredKey for #ident
        {
            type Payload = PayloadEncoding;
            type Signature = SignatureEncoding;
            type PublicKey = PublicKeyEncoding;
            type Error = crate::keys::errors::KeyError;

            #key_enum_impl_secured_key_method_sign
            #key_enum_impl_secured_key_method_sign_multiple
            #key_enum_impl_secured_key_method_as_public_key
        }
    }
}

fn build_key_enum_impl_secured_key_method_sign(
    key_enum_variants: &Punctuated<Variant, Comma>,
) -> TokenStream {
    let sign_arms_ok = key_enum_variants.iter().map(|variant| {
        let variant_ident = &variant.ident;
        let variant_attributes_cfg: Vec<Attribute> = get_cfg_attributes(&variant.attrs).collect();

        quote! {
            #(#variant_attributes_cfg)*
            (Self::#variant_ident(key), Self::Payload::#variant_ident(payload)) => {
                key.sign(payload).map(Self::Signature::#variant_ident)
            }
        }
    });

    let sign_arm_error = quote! {
        (key, payload) => Err(crate::keys::errors::EncodingError::requires(key, payload).into()),
    };

    quote! {
        fn sign(&self, payload: &Self::Payload) -> Result<Self::Signature, Self::Error> {
            match (self, payload) {
                #(#sign_arms_ok)*
                #sign_arm_error
            }
        }
    }
}

fn build_key_enum_impl_secured_key_method_sign_multiple(
    key_enum_variants: &Punctuated<Variant, Comma>,
) -> TokenStream {
    let unpack_keys = quote! {
        let [head, tail @ ..] = keys else {
            let error_message = String::from("Multi-key signature requires at least two keys.");
            return Err(Self::Error::UnsupportedKey(error_message));
        };
    };

    let verify_same_variant = quote! {
        if !lb_utils::types::enumerated::is_same_variant(keys) {
            let error_message = String::from("Multi-key signature requires all keys to have the same variant.");
            return Err(Self::Error::UnsupportedKey(error_message));
        }
    };

    let sign_arms_ok = key_enum_variants.iter().map(|variant| {
        let variant_ident = &variant.ident;
        let variant_attributes_cfg: Vec<Attribute> = get_cfg_attributes(&variant.attrs).collect();
        let wrapped_key = &get_wrapped_key_as_ref(variant).ty;

        quote! {
            #(#variant_attributes_cfg)*
            (Self::#variant_ident(key_head), Self::Payload::#variant_ident(payload)) => {
                let key_tails = tail.iter().map(|tail_item| {
                    // Tail items are guaranteed to be of the same type as the head.
                    match tail_item {
                        Self::#variant_ident(key_tail_item) => key_tail_item,
                        _ => unreachable!("Validated in `validate_variants`"),
                    }
                });

                let keys = std::iter::once(key_head).chain(key_tails).collect::<Vec<_>>();

                <#wrapped_key as crate::keys::secured_key::SecuredKey>::sign_multiple(
                    keys.as_slice(), payload
                ).map(Self::Signature::#variant_ident)
            }
        }
    });

    let sign_arm_error = quote! {
        (key_head, payload) => Err(crate::keys::errors::EncodingError::requires(*key_head, payload).into()),
    };

    quote! {
        fn sign_multiple(keys: &[&Self], payload: &Self::Payload) -> Result<Self::Signature, Self::Error> {
            #unpack_keys
            #verify_same_variant
            match (head, payload) {
                #(#sign_arms_ok)*
                #sign_arm_error
            }
        }
    }
}

fn build_key_enum_impl_secured_key_method_as_public_key(
    key_enum_variants: &Punctuated<Variant, Comma>,
) -> TokenStream {
    let as_public_key_arms = key_enum_variants.iter().map(|variant| {
        let variant_ident = &variant.ident;
        let variant_attributes_cfg: Vec<Attribute> = get_cfg_attributes(&variant.attrs).collect();
        quote! {
            #(#variant_attributes_cfg)*
            Self::#variant_ident(key) => Self::PublicKey::#variant_ident(key.as_public_key()),
        }
    });

    quote! {
        fn as_public_key(&self) -> Self::PublicKey {
            match self {
                #(#as_public_key_arms)*
            }
        }
    }
}

fn get_wrapped_key_as_ref(variant: &Variant) -> &Field {
    match &variant.fields {
        Fields::Unnamed(field) if field.unnamed.len() == 1 => field.unnamed.first().unwrap(),
        _ => unreachable!("Validated in `validate_variants`"),
    }
}

fn is_cfg(attribute: &Attribute) -> bool {
    attribute.path().is_ident("cfg")
}

fn get_cfg_attributes(attributes: &[Attribute]) -> impl Iterator<Item = Attribute> {
    attributes
        .iter()
        .filter(|attribute| is_cfg(attribute))
        .cloned()
}
