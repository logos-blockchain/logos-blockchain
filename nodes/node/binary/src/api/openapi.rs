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
                schema::Status,
                schema::MempoolMetrics,
                crate::api::errors::ErrorBody,
                // Referenced by `BlocksStreamQuery`'s `IntoParams` derive.
                // utoipa collects schemas reached through request and response
                // bodies automatically, but not through query parameters.
                lb_http_api_common::queries::BlockFilter,
                lb_http_api_common::queries::BlockSortOrder
            )),
            tags()
        )]
        pub struct ApiDoc;

        /// The `(method, path)` pairs the router serves, as declared by the
        /// table. Compared against the generated document in the tests below.
        #[cfg(test)]
        const ROUTE_TABLE: &[(&str, &str)] = &[$((stringify!($method), $path)),*];
    };
}

api_routes!(declare_api_doc);

pub mod schema {
    use lb_tx_service::{MempoolMetrics as DomainMempoolMetrics, backend::Status as DomainStatus};
    use serde::Serialize;
    use utoipa::ToSchema;

    #[derive(ToSchema, Serialize)]
    #[serde(transparent)]
    pub struct MempoolMetrics(pub DomainMempoolMetrics);

    #[derive(ToSchema, Serialize)]
    #[serde(transparent)]
    pub struct Status(pub DomainStatus);
}

#[cfg(test)]
fn document() -> serde_json::Value {
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
            .map(|(method, path)| ((*method).to_uppercase(), (*path).to_owned()))
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
