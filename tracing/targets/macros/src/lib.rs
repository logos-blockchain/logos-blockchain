//! Proc-macro implementation for the log-targets crate.
//!
//! `log_targets!` defines one target namespace per file. The namespace root is
//! inferred from the call-site path:
//! - `blend.rs` -> `blend`
//! - `time.rs` -> `time`
//! - `mod.rs` -> parent directory name
//!
//! Inside the macro, declarations are written relative to that inferred root:
//!
//! ```ignore
//! // blend.rs
//! log_targets! {
//!     service::{CORE, core::KMS_POQ_GENERATOR},
//!     network::core::handler::{CORE_EDGE},
//! }
//! ```
//!
//! That input generates:
//! - nested modules and `ROOT` / leaf constants
//! - target collection helpers
use std::path::Path;

use proc_macro::TokenStream;
use proc_macro2::{Ident, Literal, TokenStream as TokenStream2};
use quote::quote;
use syn::{
    Error, Result, Token,
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
};

/// Define log targets for one namespace from grouped relative declarations.
///
/// The namespace root is inferred from the file where the macro is invoked:
/// - `blend.rs` -> `blend`
/// - `time.rs` -> `time`
/// - `mod.rs` -> parent directory name
///
/// Declarations inside the macro are written relative to that inferred root.
///
/// For example, in `blend.rs`:
///
/// ```ignore
/// log_targets! {
///     service::{CORE, core::KMS_POQ_GENERATOR},
///     network::core::handler::{CORE_EDGE},
/// }
/// ```
///
/// This generates nested modules and constants such as:
/// - `blend::ROOT`
/// - `blend::service::ROOT`
/// - `blend::service::CORE`
/// - `blend::service::core::KMS_POQ_GENERATOR`
/// - `blend::network::core::handler::CORE_EDGE`
///
/// It also generates target collection helpers under the inferred root module.
///
/// Naming convention:
/// - use one file per target namespace
/// - file name determines the root namespace
/// - leaf identifiers are written in `SHOUTY_SNAKE_CASE`
/// - leaf string segments are emitted in kebab-case
#[proc_macro]
pub fn log_targets(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as TargetList);
    expand_target_list(input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

/// A parsed list of target declarations passed to `log_targets!`.
struct TargetList {
    /// Top-level namespace root inferred from the call-site file name.
    root: Ident,
    /// Comma-separated grouped declarations such as `service::{CORE}`.
    groups: Punctuated<TargetGroup, Token![,]>,
}

/// One grouped declaration block such as `service::{CORE, core::LEAF}`.
struct TargetGroup {
    /// Path prefix shared by all items in this group, relative to the root.
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
}

impl Parse for TargetList {
    /// Parse the full macro input as a comma-separated list of grouped target
    /// declarations.
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        Ok(Self {
            root: infer_root_ident()?,
            groups: Punctuated::parse_terminated(input)?,
        })
    }
}

impl Parse for TargetGroup {
    /// Parse a grouped declaration such as `service::{CORE}`.
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

        let leaf = parts.pop().expect("target path must have a leaf");
        Ok(Self::Leaf(TargetLeafPath {
            modules: parts,
            leaf,
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

fn infer_root_ident() -> Result<Ident> {
    let Some(path) = proc_macro::Span::call_site().local_file() else {
        return Err(Error::new(
            proc_macro2::Span::call_site(),
            "could not infer target root; use `root = <ident>;` explicitly",
        ));
    };

    let root = infer_root_name_from_path(&path).ok_or_else(|| {
        Error::new(
            proc_macro2::Span::call_site(),
            "could not infer target root from call-site path; use `root = <ident>;` explicitly",
        )
    })?;

    syn::parse_str::<Ident>(&root).map_err(|_| {
        Error::new(
            proc_macro2::Span::call_site(),
            format!(
                "inferred target root `{root}` is not a valid Rust identifier; use `root = <ident>;` explicitly"
            ),
        )
    })
}

fn infer_root_name_from_path(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    if stem != "mod" {
        return Some(stem.to_owned());
    }

    path.parent()?.file_name()?.to_str().map(ToOwned::to_owned)
}

impl ModuleNode {
    /// Insert one parsed target path into the module tree.
    ///
    /// This also rejects invalid declarations where a leaf conflicts with a
    /// child module or a leaf is declared more than once in the same module.
    fn insert(&mut self, modules: &[Ident], leaf: Ident) -> Result<()> {
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

        current.leaves.push(TargetLeaf { ident: leaf });
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

/// Convert the parsed grouped declarations into a module tree, then emit code.
fn expand_target_list(input: TargetList) -> Result<TokenStream2> {
    let mut roots = vec![(input.root.clone(), ModuleNode::default())];

    for group in input.groups {
        flatten_group(group, &input.root, &mut roots)?;
    }

    let modules = roots
        .into_iter()
        .map(|(name, node)| emit_root_module(&name, &node))
        .collect::<Vec<_>>();

    Ok(quote! {
        #(#modules)*
    })
}

fn flatten_group(
    group: TargetGroup,
    root: &Ident,
    roots: &mut Vec<(Ident, ModuleNode)>,
) -> Result<()> {
    let mut prefix = vec![root.clone()];
    prefix.extend(group.prefix);

    for item in group.items {
        flatten_item(item, &prefix, roots)?;
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
    root.insert(modules, leaf.leaf)
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
        let leaf_segment = kebab_case_ident(ident);
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
