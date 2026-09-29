//! HTTP API conformance suite.
//!
//! Every route the node serves is exercised through the production backend
//! (see [`harness`]) and each exchange is checked against the `OpenAPI`
//! document generated from the handler annotations, which is the published
//! specification:
//!
//! - the request body, if any, must match the operation's declared request
//!   schema;
//! - the response status must be declared for the operation;
//! - the response body must use a declared content type and match its schema
//!   (streams are checked line by line), or be empty if none is declared.
//!
//! [`every_route_has_a_conformance_case`] makes the suite total: a route
//! without a case fails the build, as does a case for a route that no longer
//! exists.

mod cases;
mod harness;
mod stubs;

use std::collections::BTreeSet;

use serde_json::Value;

use super::openapi::{ROUTE_TABLE, SPEC_URL, document, openapi_path};

/// One request against one route, and the status it must produce.
pub struct Case {
    /// Route-table method, lowercase (`get`, `post`, ...).
    pub method: &'static str,
    /// Route-table path, in axum syntax (`/channel/:id`).
    pub route: &'static str,
    /// Concrete path and query to request.
    pub uri: String,
    pub body: Option<Value>,
    pub status: u16,
}

impl Case {
    pub fn new(method: &'static str, route: &'static str) -> Self {
        Self {
            method,
            route,
            uri: route.to_owned(),
            body: None,
            status: 200,
        }
    }

    pub fn uri(mut self, uri: impl Into<String>) -> Self {
        self.uri = uri.into();
        self
    }

    pub fn body(mut self, body: Value) -> Self {
        self.body = Some(body);
        self
    }

    pub const fn status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }

    fn name(&self) -> String {
        format!(
            "{} {} -> {}",
            self.method.to_uppercase(),
            self.uri,
            self.status
        )
    }
}

/// Escapes a JSON-pointer token and percent-encodes it for a URI fragment.
fn pointer_token(token: &str) -> String {
    token
        .replace('~', "~0")
        .replace('/', "~1")
        .replace('{', "%7B")
        .replace('}', "%7D")
}

/// Validates `instance` against the schema at `pointer` inside the spec.
fn validate(spec: &Value, pointer: &[&str], instance: &Value) -> Result<(), String> {
    let location = format!(
        "openapi.json#/{}",
        pointer
            .iter()
            .map(|token| pointer_token(token))
            .collect::<Vec<_>>()
            .join("/")
    );
    let mut schemas = boon::Schemas::new();
    let mut compiler = boon::Compiler::new();
    compiler
        .add_resource("openapi.json", spec.clone())
        .map_err(|error| format!("spec is not a valid resource: {error}"))?;
    let index = compiler
        .compile(&location, &mut schemas)
        .map_err(|error| format!("schema {location} does not compile: {error}"))?;
    schemas
        .validate(instance, index)
        .map_err(|error| format!("does not match {location}:\n{error:#}"))
}

async fn check(node: &harness::Node, spec: &Value, case: &Case) -> Result<(), String> {
    let path = openapi_path(case.route);
    let operation = &spec["paths"][&path][case.method];
    if operation.is_null() {
        return Err(format!("{} {path} is not in the spec", case.method));
    }

    check_parameters(spec, &path, case)?;

    let client = reqwest::Client::new();
    let method = reqwest::Method::from_bytes(case.method.to_uppercase().as_bytes())
        .expect("route-table methods are valid");
    let mut request = client.request(method, format!("{}{}", node.base_url, case.uri));
    if let Some(body) = &case.body {
        validate(
            spec,
            &[
                "paths",
                &path,
                case.method,
                "requestBody",
                "content",
                "application/json",
                "schema",
            ],
            body,
        )
        .map_err(|error| format!("request body {error}"))?;
        request = request.json(body);
    }

    let response = request
        .send()
        .await
        .map_err(|error| format!("request failed: {error}"))?;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_owned()
        });
    let body = response
        .bytes()
        .await
        .map_err(|error| format!("reading body: {error}"))?;

    if status != case.status {
        return Err(format!(
            "expected status {}, got {status}: {}",
            case.status,
            String::from_utf8_lossy(&body)
        ));
    }
    check_response(spec, &path, case.method, status, content_type, &body)
}

/// The response must use a status, content type and body the operation
/// declares.
fn check_response(
    spec: &Value,
    path: &str,
    method: &str,
    status: u16,
    content_type: Option<String>,
    body: &[u8],
) -> Result<(), String> {
    let operation = &spec["paths"][path][method];
    let status_key = status.to_string();
    let declared = &operation["responses"][&status_key];
    if declared.is_null() {
        return Err(format!("status {status} is not declared"));
    }

    let Some(content) = declared["content"]
        .as_object()
        .filter(|content| !content.is_empty())
    else {
        return if body.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "status {status} declares no body, but one was returned: {}",
                String::from_utf8_lossy(body)
            ))
        };
    };
    let content_type = content_type.ok_or("response has no content type")?;
    if !content.contains_key(&content_type) {
        return Err(format!(
            "content type {content_type} is not declared (declared: {:?})",
            content.keys().collect::<Vec<_>>()
        ));
    }
    if content[&content_type]["schema"].is_null() {
        return Err(format!(
            "{content_type} response for {status} has no schema"
        ));
    }
    let schema = [
        "paths",
        path,
        method,
        "responses",
        &status_key,
        "content",
        &content_type,
        "schema",
    ];

    let text = std::str::from_utf8(body).map_err(|error| format!("body is not UTF-8: {error}"))?;
    let instances: Vec<Value> = match content_type.as_str() {
        "application/json" => vec![
            serde_json::from_str(text)
                .map_err(|error| format!("body is not JSON: {error}: {text}"))?,
        ],
        "application/x-ndjson" => text
            .lines()
            .map(|line| {
                serde_json::from_str(line)
                    .map_err(|error| format!("stream line is not JSON: {error}: {line}"))
            })
            .collect::<Result<_, _>>()?,
        "text/plain" => vec![Value::String(text.to_owned())],
        other => return Err(format!("no validator for content type {other}")),
    };
    for instance in &instances {
        validate(spec, &schema, instance).map_err(|error| format!("response {error}"))?;
    }
    Ok(())
}

/// Every path and query parameter the case sends must be declared by the
/// operation, and its value must match the declared schema.
fn check_parameters(spec: &Value, path: &str, case: &Case) -> Result<(), String> {
    let (uri_path, query) = case.uri.split_once('?').unwrap_or((&case.uri, ""));
    let path_values = case
        .route
        .split('/')
        .zip(uri_path.split('/'))
        .filter_map(|(template, value)| template.strip_prefix(':').map(|name| (name, value)));
    let query_values: Vec<(String, String)> =
        serde_urlencoded::from_str(query).map_err(|error| format!("bad query string: {error}"))?;
    let sent = path_values
        .map(|(name, value)| ("path", name.to_owned(), value.to_owned()))
        .chain(
            query_values
                .into_iter()
                .map(|(name, value)| ("query", name, value)),
        );

    let declared = spec["paths"][path][case.method]["parameters"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for (location, name, value) in sent {
        let Some(index) = declared.iter().position(|parameter| {
            parameter["in"] == location && parameter["name"] == name.as_str()
        }) else {
            return Err(format!("{location} parameter `{name}` is not declared"));
        };
        let pointer = [
            "paths",
            path,
            case.method,
            "parameters",
            &index.to_string(),
            "schema",
        ];
        // Parameters arrive as text; a numeric or boolean schema is checked
        // against the value the text denotes.
        let as_string = Value::String(value.clone());
        if validate(spec, &pointer, &as_string).is_err() {
            let denoted = serde_json::from_str(&value).unwrap_or(as_string);
            validate(spec, &pointer, &denoted)
                .map_err(|error| format!("{location} parameter `{name}` {error}"))?;
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn every_case_conforms_to_the_published_spec() {
    let node = harness::Node::start().await;
    let spec = document();

    let mut failures = Vec::new();
    for case in cases::all() {
        if let Err(error) = check(&node, &spec, &case).await {
            failures.push(format!(
                "{}\n  {}",
                case.name(),
                error.replace('\n', "\n  ")
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} of the node HTTP API exchanges do not match the handlers' `OpenAPI` annotations, \
         which are the published specification ({SPEC_URL}).\n\
         Fix the handler, or correct its annotation and update the specification to match.\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// Every served route has at least one successful case, and every case
/// targets a served route.
#[test]
fn every_route_has_a_conformance_case() {
    let routed: BTreeSet<(String, String)> = ROUTE_TABLE
        .iter()
        .map(|(method, path)| ((*method).to_owned(), (*path).to_owned()))
        .collect();
    let all = cases::all();
    let covered: BTreeSet<(String, String)> = all
        .iter()
        .filter(|case| (200..300).contains(&case.status))
        .map(|case| (case.method.to_owned(), case.route.to_owned()))
        .collect();
    let targeted: BTreeSet<(String, String)> = all
        .iter()
        .map(|case| (case.method.to_owned(), case.route.to_owned()))
        .collect();

    let uncovered: Vec<_> = routed.difference(&covered).collect();
    let unrouted: Vec<_> = targeted.difference(&routed).collect();
    assert!(
        uncovered.is_empty() && unrouted.is_empty(),
        "The conformance suite no longer covers the node HTTP API exactly.\n\
         Routes without a successful case: {uncovered:?}\n\
         Cases for routes that are not served: {unrouted:?}\n\n\
         Add or remove cases in `api/conformance/cases.rs`, and make sure the \
         specification at {SPEC_URL} describes the same set of endpoints."
    );
}
