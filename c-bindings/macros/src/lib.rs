use proc_macro::TokenStream;
use quote::quote;
use syn::{ItemFn, ReturnType, Type, parse_macro_input, parse_quote};

/// Turns a panic inside an exported function into an error return.
///
/// A panic cannot unwind out of an `extern "C"` function: the process aborts
/// instead, taking the host application down with it. This attribute runs the
/// function body inside [`std::panic::catch_unwind`] and, if it panics,
/// returns the function's own error value carrying the panic message, with
/// the `RuntimeError` status code by default.
///
/// The return type must implement the bindings' `FfiReturn` trait, which is
/// what builds the error value: `OperationStatus`, `FfiResult<_,
/// OperationStatus>` and `()` do.
///
/// The error type defaults to `crate::OperationStatus`; another library can
/// supply its own with `#[panic_to_error(SqlStatus)]`. It must implement
/// `lb_c_common::FfiError`, and the return type must implement
/// `lb_c_common::FfiReturn` for that error.
/// The consuming crate must expose the shared dependency as `lb_c_common`.
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
    let status: Type = if arguments.is_empty() {
        parse_quote!(crate::OperationStatus)
    } else {
        parse_macro_input!(arguments as Type)
    };

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
                Err(payload) => {
                    let message = ::lb_c_common::panic_message(payload.as_ref());
                    let error = <#status as ::lb_c_common::FfiError>::from_panic(message);
                    // SAFETY: The panic handler has just created this owned error.
                    unsafe { <#output as ::lb_c_common::FfiReturn<#status>>::from_error(error) }
                }
            }
        }
    }
    .into()
}
