use proc_macro::TokenStream;
use quote::quote;
use syn::{Error, ItemFn, ReturnType, parse_macro_input};

/// Turns a panic inside an exported function into an error return.
///
/// A panic cannot unwind out of an `extern "C"` function: the process aborts
/// instead, taking the host application down with it. This attribute runs the
/// function body inside [`std::panic::catch_unwind`] and, if it panics,
/// returns the function's own error value carrying the panic message, with
/// the `RuntimeError` status code.
///
/// The return type must implement the bindings' `FfiReturn` trait, which is
/// what builds the error value: `OperationStatus`, `FfiResult<_,
/// OperationStatus>` and `()` do.
///
/// # Example
///
/// ```rust,ignore
/// #[panic_to_error]
/// #[unsafe(no_mangle)]
/// pub unsafe extern "C" fn get_time_info(node: *const LogosBlockchainNode) -> FfiTimeInfoResult {
///     // ...
/// }
/// ```
#[proc_macro_attribute]
pub fn panic_to_error(arguments: TokenStream, item: TokenStream) -> TokenStream {
    if !arguments.is_empty() {
        return Error::new_spanned(
            proc_macro2::TokenStream::from(arguments),
            "`panic_to_error` takes no arguments",
        )
        .to_compile_error()
        .into();
    }

    let ItemFn {
        attrs,
        vis,
        sig,
        block,
    } = parse_macro_input!(item as ItemFn);

    let output = match &sig.output {
        ReturnType::Default => quote!(()),
        ReturnType::Type(_, output) => quote!(#output),
    };

    quote! {
        #(#attrs)*
        #vis #sig {
            match ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(
                move || -> #output #block
            )) {
                Ok(value) => value,
                Err(payload) => <#output as crate::macros::FfiReturn>::from_operation_status(
                    crate::errors::OperationStatus::from_panic(payload.as_ref()),
                ),
            }
        }
    }
    .into()
}
