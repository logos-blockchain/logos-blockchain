//! Proc-macro implementation for the log-targets crate.
//!
//! A proc macro is used here so target definitions only need to be written
//! once, while still generating both the constants used by code and the
//! collected target list used for validation. A `macro_rules!` macro could do
//! this too, but the declaration format and expansion logic became harder to
//! read once nesting and collection were involved.
//!
//! The input is a grouped list of target paths:
//!
//! ```ignore
//! log_targets! {
//!     blend::{
//!         service::{CORE, core::KMS_POQ_GENERATOR},
//!         network::core::handler::{CORE_EDGE},
//!     }
//! }
//! ```
//! From those declarations the macro generates:
//! - nested modules and `ROOT` / leaf constants
//! - target collection helpers
//!
//! The proc macro does the following:
//! - parse the grouped declarations once
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
    /// Comma-separated grouped declarations such as
    /// `blend::{service::{CORE}}`.
    groups: Punctuated<TargetGroup, Token![,]>,
}

/// One grouped declaration block such as `service::{CORE, core::LEAF}`.
struct TargetGroup {
    /// Path prefix shared by all items in this group.
    prefix: Vec<Ident>,
    /// Items declared under that prefix.
    items: Punctuated<TargetItem, Token![,]>,
}

/// One item inside a grouped declaration block.
enum TargetItem {
    /// A leaf target like `CORE` or `core::KMS_POQ_GENERATOR`.
    Leaf(TargetLeafPath),
    /// A nested grouped block such as `service::{...}`.
    Group(TargetGroup),
}

/// One declared leaf path relative to its surrounding group.
struct TargetLeafPath {
    /// Intermediate modules between the surrounding group prefix and the leaf.
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
    /// Parse the full macro input as a comma-separated list of grouped target
    /// declarations.
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        Ok(Self {
            groups: Punctuated::parse_terminated(input)?,
        })
    }
}

impl Parse for TargetGroup {
    /// Parse a grouped declaration such as `blend::{service::{CORE}}`.
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let prefix = parse_path_segments(input)?;
        input.parse::<Token![::]>()?;

        let content;
        syn::braced!(content in input);

        Ok(Self {
            prefix,
            items: Punctuated::parse_terminated(&content)?,
        })
    }
}

impl Parse for TargetItem {
    /// Parse one item inside a grouped declaration.
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let mut parts = parse_path_segments(input)?;

        if input.peek(Token![::]) {
            input.parse::<Token![::]>()?;
            let content;
            syn::braced!(content in input);

            return Ok(Self::Group(TargetGroup {
                prefix: parts,
                items: Punctuated::parse_terminated(&content)?,
            }));
        }

        let segment_override = if input.peek(Token![=]) {
            input.parse::<Token![=]>()?;
            Some(input.parse()?)
        } else {
            None
        };

        let leaf = parts.pop().expect("target path must have a leaf");
        Ok(Self::Leaf(TargetLeafPath {
            modules: parts,
            leaf,
            segment_override,
        }))
    }
}

fn parse_path_segments(input: ParseStream<'_>) -> Result<Vec<Ident>> {
    let mut parts = vec![input.parse::<Ident>()?];

    while input.peek(Token![::]) {
        let fork = input.fork();
        fork.parse::<Token![::]>()?;
        if fork.peek(syn::token::Brace) {
            break;
        }

        input.parse::<Token![::]>()?;
        parts.push(input.parse::<Ident>()?);
    }

    Ok(parts)
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

/// Convert the parsed grouped declarations into a module tree, then emit code.
fn expand_target_list(input: TargetList) -> Result<TokenStream2> {
    let mut roots: Vec<(Ident, ModuleNode)> = Vec::new();

    for group in input.groups {
        flatten_group(group, &mut roots)?;
    }

    let modules = roots
        .into_iter()
        .map(|(name, node)| emit_root_module(&name, &node))
        .collect::<Vec<_>>();

    Ok(quote! {
        #(#modules)*
    })
}

fn flatten_group(group: TargetGroup, roots: &mut Vec<(Ident, ModuleNode)>) -> Result<()> {
    for item in group.items {
        flatten_item(item, &group.prefix, roots)?;
    }

    Ok(())
}

fn flatten_item(
    item: TargetItem,
    parent_prefix: &[Ident],
    roots: &mut Vec<(Ident, ModuleNode)>,
) -> Result<()> {
    match item {
        TargetItem::Leaf(leaf) => insert_leaf(parent_prefix, leaf, roots),
        TargetItem::Group(group) => {
            let mut prefix = parent_prefix.to_vec();
            prefix.extend(group.prefix);
            for child in group.items {
                flatten_item(child, &prefix, roots)?;
            }
            Ok(())
        }
    }
}

fn insert_leaf(
    parent_prefix: &[Ident],
    leaf: TargetLeafPath,
    roots: &mut Vec<(Ident, ModuleNode)>,
) -> Result<()> {
    let mut full_prefix = parent_prefix.to_vec();
    full_prefix.extend(leaf.modules);

    let Some((root_name, modules)) = full_prefix.split_first() else {
        return Err(Error::new_spanned(
            &leaf.leaf,
            "target declaration must have a root prefix",
        ));
    };

    let root_index = roots
        .iter()
        .position(|(name, _)| *name == *root_name)
        .unwrap_or_else(|| {
            roots.push((root_name.clone(), ModuleNode::default()));
            roots.len() - 1
        });

    let root = &mut roots[root_index].1;
    root.insert(modules, leaf.leaf, leaf.segment_override)
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
