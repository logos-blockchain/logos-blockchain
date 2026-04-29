//! Proc-macro implementation for the log-targets crate.
//!
//! A proc macro is used here so target definitions only need to be written
//! once, while still generating both the constants used by code and the
//! collected target list used for validation. A `macro_rules!` macro could do
//! this too, but the declaration format and expansion logic became harder to
//! read once nesting and collection were involved.
//!
//! The input we want to support is a flat list of target paths:
//!
//! ```ignore
//! log_targets! {
//!     blend::service::CORE,
//!     blend::service::core::KMS_POQ_GENERATOR,
//!     blend::network::core::handler::CORE_EDGE,
//! }
//! ```
//!
//! From that list we need to generate:
//! - nested modules and `ROOT` / leaf constants
//! - target collection helpers
//!
//! The proc macro does the following:
//! - parse the flat path list once
//! - build a small in-memory tree
//! - emit the nested module structure from that tree
//! - report duplicate/conflicting definitions with direct, readable errors

use proc_macro::TokenStream;
use proc_macro2::{Ident, Literal, TokenStream as TokenStream2};
use quote::quote;
use syn::{
    Error, LitStr, Result, Token,
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
};

/// A parsed list of target declarations passed to `log_targets!`.
struct TargetList {
    /// Comma-separated target declarations such as
    /// `blend::service::core::KMS_POQ_GENERATOR`.
    targets: Punctuated<TargetPath, Token![,]>,
}

/// One declared target path from the macro input.
struct TargetPath {
    /// Top-level namespace root, for example `blend`.
    root: Ident,
    /// Intermediate modules between the root and the leaf constant.
    modules: Vec<Ident>,
    /// Final constant name, for example `CORE_AND_LEADER`.
    leaf: Ident,
    /// Optional explicit string segment override for the leaf.
    ///
    /// If omitted, the leaf is converted from `SHOUTY_SNAKE` to kebab-case.
    segment_override: Option<LitStr>,
}

/// A mutable tree node used while grouping flat target paths into nested
/// modules.
#[derive(Default)]
struct ModuleNode {
    /// Direct child modules under this module.
    children: Vec<ChildModule>,
    /// Direct leaf targets under this module.
    leaves: Vec<TargetLeaf>,
}

/// One named child module in the generated tree.
struct ChildModule {
    /// Rust identifier used for the generated child module.
    ident: Ident,
    /// Subtree rooted at that child module.
    node: ModuleNode,
}

/// One leaf target constant in the generated tree.
struct TargetLeaf {
    /// Rust identifier used for the generated constant.
    ident: Ident,
    /// Optional explicit string segment override for the constant.
    ///
    /// If absent, the identifier is converted to kebab-case at compile time.
    segment_override: Option<LitStr>,
}

impl Parse for TargetList {
    /// Parse the full macro input as a comma-separated list of target paths.
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        Ok(Self {
            targets: Punctuated::parse_terminated(input)?,
        })
    }
}

impl Parse for TargetPath {
    /// Parse a single declaration such as `blend::service::CORE`.
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let root = input.parse()?;
        input.parse::<Token![::]>()?;

        let mut parts = Vec::new();
        parts.push(input.parse::<Ident>()?);

        while input.peek(Token![::]) {
            input.parse::<Token![::]>()?;
            parts.push(input.parse::<Ident>()?);
        }

        let segment_override = if input.peek(Token![=]) {
            input.parse::<Token![=]>()?;
            Some(input.parse()?)
        } else {
            None
        };

        let leaf = parts.pop().expect("target path must have a leaf");
        Ok(Self {
            root,
            modules: parts,
            leaf,
            segment_override,
        })
    }
}

impl ModuleNode {
    /// Insert one parsed target path into the module tree.
    ///
    /// This also rejects invalid declarations where a leaf conflicts with a
    /// child module or a leaf is declared more than once in the same module.
    fn insert(
        &mut self,
        modules: &[Ident],
        leaf: Ident,
        segment_override: Option<LitStr>,
    ) -> Result<()> {
        let mut current = self;
        for module in modules {
            current = current.child_mut(module)?;
        }

        if current.children.iter().any(|child| child.ident == leaf) {
            return Err(Error::new_spanned(
                &leaf,
                "target leaf conflicts with an existing child module",
            ));
        }

        if current.leaves.iter().any(|existing| existing.ident == leaf) {
            return Err(Error::new_spanned(&leaf, "duplicate target leaf"));
        }

        current.leaves.push(TargetLeaf {
            ident: leaf,
            segment_override,
        });
        Ok(())
    }

    /// Return a mutable child module, creating it if it does not exist yet.
    ///
    /// This also rejects invalid declarations where a module name would collide
    /// with an already-declared leaf at the same level.
    fn child_mut(&mut self, module: &Ident) -> Result<&mut Self> {
        if self.leaves.iter().any(|leaf| leaf.ident == *module) {
            return Err(Error::new_spanned(
                module,
                "child module conflicts with an existing target leaf",
            ));
        }

        if let Some(index) = self
            .children
            .iter()
            .position(|child| child.ident == *module)
        {
            Ok(&mut self.children[index].node)
        } else {
            self.children.push(ChildModule {
                ident: module.clone(),
                node: Self::default(),
            });
            let last = self.children.len() - 1;
            Ok(&mut self.children[last].node)
        }
    }
}

/// Expand the `log_targets!` macro into nested modules plus target collection
/// helpers.
#[proc_macro]
pub fn log_targets(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as TargetList);
    expand_target_list(input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

/// Convert the parsed flat path list into a grouped module tree, then emit
/// code.
fn expand_target_list(input: TargetList) -> Result<TokenStream2> {
    let mut roots: Vec<(Ident, ModuleNode)> = Vec::new();

    for target in input.targets {
        let root_index =
            if let Some(index) = roots.iter().position(|(name, _)| *name == target.root) {
                index
            } else {
                roots.push((target.root.clone(), ModuleNode::default()));
                roots.len() - 1
            };

        let root = &mut roots[root_index].1;
        root.insert(&target.modules, target.leaf, target.segment_override)?;
    }

    let modules = roots
        .into_iter()
        .map(|(name, node)| emit_root_module(&name, &node))
        .collect::<Vec<_>>();

    Ok(quote! {
        #(#modules)*
    })
}

fn kebab_case_ident(ident: &Ident) -> String {
    ident.to_string().replace('_', "-").to_ascii_lowercase()
}

/// Emit one top-level root module such as `pub mod blend`.
fn emit_root_module(module_ident: &Ident, node: &ModuleNode) -> TokenStream2 {
    let root_path = module_ident.to_string();
    emit_module(module_ident, &root_path, node)
}

/// Emit one module and recurse into its child modules.
fn emit_module(module_ident: &Ident, root_path: &str, node: &ModuleNode) -> TokenStream2 {
    let root_literal = Literal::string(root_path);
    let leaves = node.leaves.iter().map(|leaf| {
        let ident = &leaf.ident;
        let leaf_segment = leaf
            .segment_override
            .as_ref()
            .map_or_else(|| kebab_case_ident(ident), LitStr::value);
        let leaf_literal = Literal::string(&format!("{root_path}::{leaf_segment}"));

        quote! {
            pub const #ident: &str = #leaf_literal;
        }
    });

    let children = node.children.iter().map(|child| {
        let child_root_path = format!("{root_path}::{}", child.ident);
        emit_module(&child.ident, &child_root_path, &child.node)
    });

    let collect_body = emit_collect_body(node);
    quote! {
        pub mod #module_ident {
            pub const ROOT: &str = #root_literal;

            #(#leaves)*
            #(#children)*

            pub fn collect_targets(targets: &mut Vec<&'static str>) {
                targets.push(ROOT);
                #collect_body
            }

            pub fn all_targets() -> Vec<&'static str> {
                let mut targets = Vec::new();
                collect_targets(&mut targets);
                targets
            }

        }
    }
}

/// Emit the statements that collect this module's leaves and child modules.
fn emit_collect_body(node: &ModuleNode) -> TokenStream2 {
    let leaf_pushes = node.leaves.iter().map(|leaf| {
        let ident = &leaf.ident;
        quote!(targets.push(#ident);)
    });
    let child_pushes = node.children.iter().map(|child| {
        let module_ident = &child.ident;
        quote!(#module_ident::collect_targets(targets);)
    });

    quote! {
        #(#leaf_pushes)*
        #(#child_pushes)*
    }
}
