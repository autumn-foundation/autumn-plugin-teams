//! The Bot Framework messaging route.

use std::sync::{Arc, OnceLock};

use autumn_web::openapi::ApiDoc;
use autumn_web::reexports::axum::{
    Router,
    body::Body,
    http::{HeaderMap, HeaderValue, Method, StatusCode, header::CONTENT_TYPE},
    response::Response,
    routing::{MethodRouter, post},
};
use autumn_web::{AppState, Route};

use crate::engine::Engine;

/// Route path under the base path.
pub(crate) const MESSAGES: &str = "/messages";
/// Route name.
const NAME: &str = "teams_messages";

/// The engine, set at startup. The route replies 503 before that.
pub(crate) type EngineCell = Arc<OnceLock<Arc<Engine>>>;

fn handler<S>(cell: &EngineCell) -> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    let cell = Arc::clone(cell);
    post(move |headers: HeaderMap, body: Body| {
        let cell = Arc::clone(&cell);
        async move { dispatch(&cell, headers, body).await }
    })
}

/// Builds a router. The path is relative to the base path.
pub(crate) fn router<S>(cell: &EngineCell) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new().route(MESSAGES, handler(cell))
}

/// Builds the autumn route under `base`. autumn needs one or more `Route` to
/// boot, and lists them in `autumn routes`.
///
/// A `Route` path is `&'static str`. The plugin leaks one short string, once
/// per app build.
pub(crate) fn autumn_routes(cell: &EngineCell, base: &str) -> Vec<Route> {
    let full: &'static str = Box::leak(format!("{base}{MESSAGES}").into_boxed_str());
    vec![Route {
        method: Method::POST,
        path: full,
        handler: handler::<AppState>(cell),
        name: NAME,
        api_doc: ApiDoc {
            method: "POST",
            path: full,
            operation_id: NAME,
            // Not an app API. The Bot Framework JWT is the access control.
            hidden: true,
            public: true,
            success_status: 200,
            module_path: module_path!(),
            source_file: file!(),
            source_line: line!(),
            ..ApiDoc::default()
        },
        api_version: None,
        sunset_opt_out: false,
        repository: None,
        idempotency: autumn_web::RouteIdempotency::default(),
        timeout: autumn_web::RouteTimeout::default(),
        seo: autumn_web::seo::SeoRouteDefaults::default(),
        criticality: autumn_web::Criticality::default(),
    }]
}

async fn dispatch(cell: &EngineCell, headers: HeaderMap, body: Body) -> Response {
    let Some(engine) = cell.get() else {
        return status(StatusCode::SERVICE_UNAVAILABLE);
    };
    // Shutdown started: run no new handler. A 503 lets the Connector retry.
    // This check comes before the dedup, so a retry is not dropped.
    if engine.runner.tracker.is_closed() {
        engine.metrics.request("shutting_down");
        return status(StatusCode::SERVICE_UNAVAILABLE);
    }
    Arc::clone(engine).messages(&headers, body).await
}

/// A status with no body.
pub(crate) fn status(code: StatusCode) -> Response {
    let mut r = Response::new(Body::empty());
    *r.status_mut() = code;
    r
}

/// A status with a JSON body.
pub(crate) fn json(code: StatusCode, value: &serde_json::Value) -> Response {
    let mut r = Response::new(Body::from(value.to_string()));
    *r.status_mut() = code;
    r.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt as _;

    #[test]
    fn autumn_routes_use_the_default_criticality() {
        let cell: EngineCell = Arc::new(OnceLock::new());
        let routes = autumn_routes(&cell, "/hooks");
        assert_eq!(routes.len(), 1);
        for route in &routes {
            assert_eq!(route.criticality, autumn_web::Criticality::Default);
            assert!(route.api_doc.hidden);
            assert!(route.api_doc.public);
        }
    }

    #[tokio::test]
    async fn unstarted_route_replies_503() {
        let cell: EngineCell = Arc::new(OnceLock::new());
        let app: Router = router(&cell);
        let req = autumn_web::reexports::http::Request::post(MESSAGES)
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
