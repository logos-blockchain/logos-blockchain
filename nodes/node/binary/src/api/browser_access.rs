//! Keeps web pages from driving the API through the operator's browser.
//!
//! The API has no authentication and whoever reaches it controls the node, so a
//! browser that can reach it must not act for the pages it opens. There are
//! three ways it could:
//!
//! - A page reads a response from another origin. CORS allows that only for the
//!   configured origins.
//! - A page sends a request that needs no preflight, such as a `POST` with no
//!   body. CORS only decides what the page can read back, and by then the
//!   handler has run, so such a request is refused here by its `Origin`.
//! - A page points its own host name at the server, which makes it same-origin
//!   and gets it past both of the above: DNS rebinding. Its `Host` still
//!   carries that name, so a request is refused here unless its `Host` is
//!   known.

use std::sync::Arc;

use axum::{
    Router,
    extract::{Request, State},
    http::{
        header::{CONTENT_TYPE, HOST, ORIGIN, USER_AGENT},
        uri::Authority,
    },
    middleware::{self, Next},
    response::{IntoResponse as _, Response},
};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use url::Host;

use crate::api::{
    errors::ApiError,
    settings::{CorsOrigin, HostName},
};

/// Applies the [module](self) rules to every route of `router`.
///
/// To be called once `router` has all its routes: a layer does not cover the
/// routes added after it.
pub fn restrict_browser_access(
    router: Router,
    cors_origins: Vec<CorsOrigin>,
    allowed_hosts: Vec<HostName>,
) -> Router {
    let cors_layer = CorsLayer::new()
        .allow_origin(AllowOrigin::list(
            cors_origins.iter().map(CorsOrigin::header_value),
        ))
        .allow_headers(vec![CONTENT_TYPE, USER_AGENT])
        .allow_methods(Any);
    let policy = Arc::new(BrowserPolicy {
        cors_origins,
        allowed_hosts,
    });

    router
        .layer(cors_layer)
        // Outermost, so that a refused request uses up none of the limits inside.
        .layer(middleware::from_fn_with_state(
            policy,
            refuse_foreign_requests,
        ))
}

async fn refuse_foreign_requests(
    State(policy): State<Arc<BrowserPolicy>>,
    request: Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    let host = headers
        .get(HOST)
        .and_then(|host| host.to_str().ok())
        // HTTP/2 carries it in the request target.
        .or_else(|| request.uri().authority().map(Authority::as_str));
    // An `Origin` that is not text matches nothing, but it was still sent.
    let origin = headers
        .get(ORIGIN)
        .map(|origin| origin.to_str().unwrap_or_default());

    match policy.check(host, origin) {
        Ok(()) => next.run(request).await,
        Err(error) => error.into_response(),
    }
}

struct BrowserPolicy {
    cors_origins: Vec<CorsOrigin>,
    allowed_hosts: Vec<HostName>,
}

impl BrowserPolicy {
    /// `host` is the request's `Host`, and `origin` the `Origin` that a browser
    /// adds to any request a page could use to change something.
    fn check(&self, host: Option<&str>, origin: Option<&str>) -> Result<(), ApiError> {
        let Some(host) = host.filter(|host| self.allows_host(host)) else {
            return Err(ApiError::Forbidden(
                "Host not allowed: to reach the node by this name, add it to \
                 `api.backend.allowed_hosts`."
                    .to_owned(),
            ));
        };
        if origin.is_some_and(|origin| !self.allows_origin(origin, host)) {
            return Err(ApiError::Forbidden(
                "Origin not allowed: to use the node from this page, add its origin to \
                 `api.backend.cors_origins`."
                    .to_owned(),
            ));
        }
        Ok(())
    }

    fn allows_host(&self, host: &str) -> bool {
        let Ok(authority) = host.parse::<Authority>() else {
            return false;
        };
        match Host::parse(authority.host()) {
            // Rebinding takes a name that its attacker controls.
            Ok(Host::Ipv4(_) | Host::Ipv6(_)) => true,
            // Browsers resolve this one themselves.
            Ok(Host::Domain(name)) if name == "localhost" => true,
            Ok(Host::Domain(name)) => self
                .allowed_hosts
                .iter()
                .any(|allowed| allowed.as_str() == name),
            Err(_) => false,
        }
    }

    /// `host` has to be an allowed one, or a rebound page would pass as
    /// same-origin.
    fn allows_origin(&self, origin: &str, host: &str) -> bool {
        let same_origin = origin
            .split_once("://")
            .is_some_and(|(_, authority)| authority.eq_ignore_ascii_case(host));

        same_origin
            || self
                .cors_origins
                .iter()
                .any(|allowed| allowed.as_str() == origin)
    }
}

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{
            HeaderValue, Method, StatusCode,
            header::{ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_REQUEST_METHOD},
        },
        routing::post,
    };
    use tower::Service as _;

    use super::*;

    const NODE: &str = "127.0.0.1:8080";
    const ATTACKER: &str = "https://attacker.example";

    fn api(cors_origins: &[&str], allowed_hosts: &[&str]) -> Router {
        restrict_browser_access(
            Router::new().route("/", post(async || {})),
            cors_origins.iter().map(|o| o.parse().unwrap()).collect(),
            allowed_hosts.iter().map(|h| h.parse().unwrap()).collect(),
        )
    }

    async fn post_to(api: &Router, host: &str, origin: Option<&str>) -> Response {
        let mut request = Request::builder()
            .method(Method::POST)
            .uri("/")
            .header(HOST, host);
        if let Some(origin) = origin {
            request = request.header(ORIGIN, origin);
        }

        let request = request.body(Body::empty()).unwrap();
        api.clone().call(request).await.unwrap()
    }

    async fn preflight(api: &Router, origin: &str) -> Response {
        let request = Request::builder()
            .method(Method::OPTIONS)
            .uri("/")
            .header(HOST, NODE)
            .header(ORIGIN, origin)
            .header(ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .body(Body::empty())
            .unwrap();

        api.clone().call(request).await.unwrap()
    }

    #[tokio::test]
    async fn a_client_outside_a_browser_is_served() {
        let api = api(&[], &[]);

        for host in [
            NODE,
            "[::1]:8080",
            "192.0.2.1:8080",
            "localhost:8080",
            "LOCALHOST:8080",
            "localhost",
        ] {
            let response = post_to(&api, host, None).await;
            assert_eq!(response.status(), StatusCode::OK, "{host}");
        }
    }

    /// A `POST` with no body needs no preflight, so nothing but this refusal
    /// stands between a page and a handler such as the leader claim.
    #[tokio::test]
    async fn a_foreign_page_never_reaches_a_handler() {
        let api = api(&[], &[]);

        for origin in [ATTACKER, "null", "http://localhost:3000"] {
            let response = post_to(&api, NODE, Some(origin)).await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{origin}");
        }
    }

    #[tokio::test]
    async fn a_foreign_page_gets_no_cors_header() {
        let response = preflight(&api(&[], &[]), ATTACKER).await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(!response.headers().contains_key(ACCESS_CONTROL_ALLOW_ORIGIN));
    }

    #[tokio::test]
    async fn every_configured_origin_is_allowed() {
        let origins = ["https://one.example", "https://two.example"];
        let api = api(&origins, &[]);

        for origin in origins {
            let expected = HeaderValue::from_static(origin);

            let response = preflight(&api, origin).await;
            assert_eq!(response.status(), StatusCode::OK, "{origin}");
            assert_eq!(
                response.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN),
                Some(&expected)
            );

            let response = post_to(&api, NODE, Some(origin)).await;
            assert_eq!(response.status(), StatusCode::OK, "{origin}");
            assert_eq!(
                response.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN),
                Some(&expected)
            );
        }

        let response = post_to(&api, NODE, Some(ATTACKER)).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    /// The Swagger UI is such a page.
    #[tokio::test]
    async fn a_page_of_the_server_itself_is_allowed() {
        let api = api(&[], &[]);

        for (host, origin) in [
            (NODE, "http://127.0.0.1:8080"),
            ("[::1]:8080", "http://[::1]:8080"),
            ("localhost:8080", "http://localhost:8080"),
        ] {
            let response = post_to(&api, host, Some(origin)).await;
            assert_eq!(response.status(), StatusCode::OK, "{origin}");
        }
    }

    #[tokio::test]
    async fn a_rebound_host_name_is_refused() {
        let api = api(&[], &[]);
        let host = "attacker.example:8080";

        for origin in [None, Some("http://attacker.example:8080")] {
            let response = post_to(&api, host, origin).await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{origin:?}");
        }
    }

    #[tokio::test]
    async fn a_host_name_is_allowed_once_configured() {
        let host = "logos_blockchain_node_0:18080";

        let response = post_to(&api(&[], &[]), host, None).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let api = api(&[], &["logos_blockchain_node_0"]);

        let response = post_to(&api, host, None).await;
        assert_eq!(response.status(), StatusCode::OK);

        let response = post_to(&api, "logos_blockchain_node_1:18080", None).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn the_host_of_an_http2_request_comes_from_its_target() {
        let request = |uri| {
            Request::builder()
                .method(Method::POST)
                .uri(uri)
                .body(Body::empty())
                .unwrap()
        };
        let mut api = api(&[], &[]);

        let response = api.call(request("/")).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let response = api.call(request("http://127.0.0.1:8080/")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = api
            .call(request("http://attacker.example:8080/"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
