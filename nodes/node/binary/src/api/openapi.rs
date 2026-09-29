#![allow(clippy::needless_for_each, reason = "Utoipa implementation")]

use crate::api::routes::api_routes;

/// Builds [`ApiDoc`] from the shared route table.
///
/// Only the `$doc` half of each row is used; the router half is matched and
/// discarded, so the handler type parameters it names are never resolved here.
macro_rules! declare_api_doc {
    ($( $method:ident $path:expr => $doc:path, $handler:expr ; )*) => {
        #[derive(utoipa::OpenApi)]
        #[openapi(
            paths($($doc),*),
            components(schemas(
                lb_tx_service::backend::Status,
                lb_tx_service::MempoolMetrics,
                schema::Null,
                crate::api::errors::ErrorBody,
                // Referenced by `BlocksStreamQuery`'s `IntoParams` derive.
                // utoipa collects schemas reached through request and response
                // bodies automatically, but not through query parameters.
                lb_http_api_common::queries::BlockFilter,
                lb_http_api_common::queries::BlockSortOrder,
                // Referenced only by path parameters, which utoipa does not
                // collect schemas from either.
                lb_core::mantle::ops::channel::ChannelId
            )),
            tags(),
            modifiers(&AxumPathTemplates)
        )]
        pub struct ApiDoc;

        /// The `(method, path)` pairs the router serves, as declared by the
        /// table. Compared against the generated document in the tests below.
        #[cfg(test)]
        pub(in crate::api) const ROUTE_TABLE: &[(&str, &str)] = &[$((stringify!($method), $path)),*];
    };
}

api_routes!(declare_api_doc);

/// Where the node HTTP API is specified. Every test that guards the API
/// surface points here, so an engineer changing the API knows the
/// specification has to change with it.
#[cfg(test)]
pub const SPEC_URL: &str = "https://lip.logos.co/blockchain/raw/node-http-api.html";

/// Rewrites axum's `:param` path segments into `OpenAPI`'s `{param}` form.
///
/// The route table is shared with the router, so its paths use axum syntax;
/// the published document must use `OpenAPI` path templating.
struct AxumPathTemplates;

impl utoipa::Modify for AxumPathTemplates {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let paths = std::mem::take(&mut openapi.paths.paths);
        openapi.paths.paths = paths
            .into_iter()
            .map(|(path, item)| (openapi_path(&path), item))
            .collect();
    }
}

#[must_use]
pub fn openapi_path(axum_path: &str) -> String {
    axum_path
        .split('/')
        .map(|segment| {
            segment
                .strip_prefix(':')
                .map_or_else(|| segment.to_owned(), |name| format!("{{{name}}}"))
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Schemas for bodies whose Rust types live outside the `OpenAPI`-aware
/// crates, or have no Rust type at all.
pub mod schema {
    use std::{borrow::Cow, collections::HashMap};

    use utoipa::{
        PartialSchema, ToSchema,
        openapi::{ObjectBuilder, RefOr, Schema, Type},
    };

    /// A signed transaction as clients submit it. Named without generic
    /// parameters because `#[utoipa::path]` would otherwise require schemas
    /// for the verification-state markers.
    pub type SignedTx = lb_core::mantle::SignedOps<
        lb_core::mantle::transactions::states::Preverified,
        lb_core::mantle::ledger::verification_mode::StandardMode,
    >;

    /// One value of the block streams; see [`SignedTx`] for why it is named.
    pub type BlockEvent = crate::api::serializers::blocks::ApiProcessedBlockEvent<
        'static,
        lb_core::mantle::transactions::states::Preverified,
        lb_core::mantle::ledger::verification_mode::StandardMode,
    >;

    /// The JSON literal `null`: the body of endpoints that acknowledge a
    /// command without returning a result.
    pub struct Null;

    impl PartialSchema for Null {
        fn schema() -> RefOr<Schema> {
            ObjectBuilder::new()
                .schema_type(Type::Null)
                .description(Some("The JSON literal `null`."))
                .into()
        }
    }

    impl ToSchema for Null {
        fn name() -> Cow<'static, str> {
            Cow::Borrowed("Null")
        }
    }

    /// A tracing verbosity level. Matched case-insensitively; `1` (error) to
    /// `5` (trace) are accepted as well.
    #[derive(ToSchema)]
    #[schema(rename_all = "lowercase")]
    #[expect(dead_code, reason = "Only describes the wire format.")]
    pub enum LogLevel {
        Error,
        Warn,
        Info,
        Debug,
        Trace,
    }

    /// Mirrors `lb_tracing::filter::envfilter::EnvFilterConfig`.
    #[derive(ToSchema)]
    #[schema(as = EnvFilterConfig)]
    #[expect(dead_code, reason = "Only describes the wire format.")]
    pub struct EnvFilterConfig {
        /// Level per tracing target. The `*` key sets the default level.
        #[schema(example = json!({"*": "info", "lb_blend": "debug"}))]
        pub filters: HashMap<String, LogLevel>,
    }
}

#[cfg(test)]
pub(in crate::api) fn document() -> serde_json::Value {
    use utoipa::OpenApi as _;
    serde_json::from_str(&ApiDoc::openapi().to_json().expect("serialize document"))
        .expect("document is valid JSON")
}

/// Compiles the named component of the generated document, resolving any
/// `$ref` it contains against the document itself.
#[cfg(test)]
fn compile_component(component: &str) -> (boon::Schemas, boon::SchemaIndex) {
    let mut schemas = boon::Schemas::new();
    let mut compiler = boon::Compiler::new();
    compiler
        .add_resource("openapi.json", document())
        .expect("document is a valid resource");
    let index = compiler
        .compile(
            &format!("openapi.json#/components/schemas/{component}"),
            &mut schemas,
        )
        .unwrap_or_else(|error| panic!("component {component} is not a valid schema: {error}"));
    (schemas, index)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{ROUTE_TABLE, document};

    /// `(METHOD, path)` pairs the generated document actually advertises.
    ///
    /// Read off the serialized document rather than
    /// [`utoipa::openapi::PathItem`], which exposes one field per method
    /// rather than a map.
    fn documented_operations() -> BTreeSet<(String, String)> {
        document()["paths"]
            .as_object()
            .expect("document has paths")
            .iter()
            .flat_map(|(path, item)| {
                item.as_object()
                    .expect("path item is an object")
                    .keys()
                    .map(|method| (method.to_uppercase(), path.clone()))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// The route table drives both the router and `paths(..)`, so an endpoint
    /// cannot be served without being documented. The HTTP method, however,
    /// still comes from the handler's own `#[utoipa::path]` attribute, so a
    /// row routed as `PUT` can be documented as `POST`. This pins them
    /// together.
    #[test]
    fn documented_methods_match_the_routed_methods() {
        let routed: BTreeSet<(String, String)> = ROUTE_TABLE
            .iter()
            .map(|(method, path)| ((*method).to_uppercase(), super::openapi_path(path)))
            .collect();
        let documented = documented_operations();

        assert_eq!(
            routed,
            documented,
            "route table and OpenAPI document disagree.\nrouted but not documented: {:?}\ndocumented but not routed: {:?}",
            routed.difference(&documented).collect::<Vec<_>>(),
            documented.difference(&routed).collect::<Vec<_>>(),
        );
    }

    /// Every `$ref` must resolve to a registered component. utoipa collects
    /// schemas reached through request and response bodies automatically, but
    /// not those reached only through `IntoParams` query parameters.
    #[test]
    fn document_has_no_dangling_schema_references() {
        fn collect_refs(value: &serde_json::Value, out: &mut BTreeSet<String>) {
            match value {
                serde_json::Value::Object(map) => {
                    if let Some(serde_json::Value::String(reference)) = map.get("$ref")
                        && let Some(name) = reference.strip_prefix("#/components/schemas/")
                    {
                        out.insert(name.to_owned());
                    }
                    for nested in map.values() {
                        collect_refs(nested, out);
                    }
                }
                serde_json::Value::Array(items) => {
                    for nested in items {
                        collect_refs(nested, out);
                    }
                }
                _ => {}
            }
        }

        let doc = document();
        let registered: BTreeSet<String> = doc["components"]["schemas"]
            .as_object()
            .map(|schemas| schemas.keys().cloned().collect())
            .unwrap_or_default();

        let mut referenced = BTreeSet::new();
        collect_refs(&doc, &mut referenced);

        let dangling: Vec<_> = referenced.difference(&registered).collect();
        assert!(dangling.is_empty(), "dangling $refs: {dangling:?}");
    }

    /// A component whose schema is malformed would otherwise only surface in a
    /// client generator.
    #[test]
    fn every_component_compiles_as_a_schema() {
        let doc = document();
        let components = doc["components"]["schemas"]
            .as_object()
            .expect("document registers components");
        assert!(!components.is_empty());
        for name in components.keys() {
            drop(super::compile_component(name));
        }
    }
}

/// Serialized-shape conformance for the hand-written
/// `schema(value_type = ..)` annotations.
///
/// Types whose schema is generated alongside their `serde` impl — the
/// `serde_bytes_newtype!` family — are covered by `lb-core`'s own tests. What
/// remains here are the foreign types whose schema is a hand-written claim the
/// compiler cannot check: `Slot`, `State`, `PeerId`, `Multiaddr`, `Locator`
/// and `NoteId`.
///
/// Each case starts from a representative JSON instance, so the test asserts
/// both that the documented shape deserializes into the Rust type and that
/// re-serializing it satisfies the published schema.
#[cfg(test)]
mod schema_conformance_tests {
    use serde::{Serialize, de::DeserializeOwned};

    use super::compile_component;

    fn assert_round_trip_matches_component<T>(component: &str, instance: serde_json::Value)
    where
        T: Serialize + DeserializeOwned,
    {
        let value: T = serde_json::from_value(instance).unwrap_or_else(|error| {
            panic!("sample instance for {component} does not deserialize: {error}")
        });
        let serialized = serde_json::to_value(&value).expect("value serializes");

        let (schemas, index) = compile_component(component);
        assert!(
            schemas.validate(&serialized, index).is_ok(),
            "{component} is documented in a way its own serialized form does not satisfy: \
             {serialized}",
        );
    }

    const HASH: &str = "0000000000000000000000000000000000000000000000000000000000000007";

    /// Covers `Slot` (documented as `u64`), `State` and `PhaseTag`.
    #[test]
    fn chain_service_info_matches_its_schema() {
        assert_round_trip_matches_component::<lb_chain_service::ChainServiceInfo>(
            "ChainServiceInfo",
            serde_json::json!({
                "cryptarchia_info": {
                    "lib": HASH,
                    "lib_slot": 11,
                    "tip": HASH,
                    "slot": 42,
                    "height": 42,
                    "state": "Online",
                },
                "phase": "Following",
            }),
        );
    }

    /// Covers the optional `commit` and `tag`, absent on non-checkout builds.
    #[test]
    fn build_version_info_matches_its_schema() {
        assert_round_trip_matches_component::<lb_version::BuildVersionInfo>(
            "BuildVersionInfo",
            serde_json::json!({
                "version": "0.3.0-rc.2",
                "commit": "ff337d8",
                "tag": "0.3.0-rc.2",
                "target": "aarch64-apple-darwin",
                "profile": "release",
                "rustc": "rustc 1.98.1 (48a229cea 2026-09-01)",
            }),
        );
    }

    /// Covers `PeerId` and `Multiaddr`, both documented as `String`.
    #[test]
    fn libp2p_info_matches_its_schema() {
        assert_round_trip_matches_component::<lb_network_service::backends::libp2p::Libp2pInfo>(
            "Libp2pInfo",
            serde_json::json!({
                "listen_addresses": ["/ip4/127.0.0.1/tcp/3000"],
                "peer_id": "12D3KooWPjceQrSwdWXPyLLeABRXmuqt69Rg3sBYbU1Nft9HyQ6X",
                "connected_peers": ["12D3KooWH3uVF6wv47WnArKHk5p6cvgCJEb74UTmxztmQDc298L3"],
                "n_peers": 1,
                "n_connections": 1,
                "n_pending_connections": 0,
                "discovered_peers": [],
                "n_discovered_peers": 0,
            }),
        );
    }

    /// Covers `Locator` and `NoteId`, both documented as `String`.
    #[test]
    fn join_blend_request_body_matches_its_schema() {
        assert_round_trip_matches_component::<
            lb_http_api_common::bodies::blend::JoinBlendRequestBody,
        >(
            "JoinBlendRequestBody",
            serde_json::json!({
                "locator": "/ip4/127.0.0.1/tcp/3000",
                "service_note_id": HASH,
            }),
        );
    }

    /// Covers `Multiaddr` in a request body.
    #[test]
    fn dial_peer_request_body_matches_its_schema() {
        assert_round_trip_matches_component::<crate::api::handlers::DialPeerRequestBody>(
            "DialPeerRequestBody",
            serde_json::json!({ "addr": "/ip4/127.0.0.1/tcp/3000" }),
        );
    }
}

/// Writes the generated document, pretty-printed, to the path in
/// `OPENAPI_OUT`.
///
/// CI runs this on both sides of a pull request and reports any difference,
/// since a change to the document is a change to the published
/// specification. It is also how the specification's copy of the document is
/// produced.
#[cfg(test)]
#[test]
#[ignore = "writes the OpenAPI document to $OPENAPI_OUT"]
fn dump_openapi() {
    let path = std::env::var_os("OPENAPI_OUT").expect("OPENAPI_OUT is set");
    let mut rendered = serde_json::to_string_pretty(&document()).expect("serialize document");
    rendered.push('\n');
    std::fs::write(path, rendered).expect("write the OpenAPI document");
}
