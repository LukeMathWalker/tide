use http::Method;
use route_recognizer::{Match, Params, Router as MethodRouter};
use std::collections::{HashMap, HashSet};

use crate::endpoint::{DynEndpoint, Endpoint};
use crate::utils::BoxFuture;
use crate::{Request, Response};

/// The routing table used by `Server`
///
/// Internally, we have a separate state machine per http method; indexing
/// by the method first allows the table itself to be more efficient.
#[allow(missing_debug_implementations)]
pub(crate) struct Router<State> {
    method_map: HashMap<http::Method, MethodRouter<Box<DynEndpoint<State>>>>,
    paths: HashSet<String>,
}

/// The result of routing a URL
pub(crate) struct Selection<'a, State> {
    pub(crate) endpoint: &'a DynEndpoint<State>,
    pub(crate) params: Params,
}

static HTTP_METHODS: [Method; 9] = [
    Method::GET,
    Method::POST,
    Method::PUT,
    Method::DELETE,
    Method::HEAD,
    Method::PATCH,
    Method::TRACE,
    Method::CONNECT,
    Method::OPTIONS,
];

impl<State: 'static> Router<State> {
    pub(crate) fn new() -> Router<State> {
        Router {
            method_map: HashMap::default(),
            paths: HashSet::default(),
        }
    }

    pub(crate) fn add(&mut self, path: &str, method: http::Method, ep: impl Endpoint<State>) {
        self.method_map
            .entry(method.clone())
            .or_insert_with(MethodRouter::new)
            .add(path, Box::new(move |cx| Box::pin(ep.call(cx))));
        // It is not possible (or quite cumbersome) to retrieve the set of paths
        // from `MethodRouter` - we'll keep track of them in a separate collection
        self.paths.insert(path.to_string());
    }

    /// For each path registered in the router:
    /// - if missing, add an OPTIONS handler that returns a 204,
    ///   listing the supported HTTP methods in the Allow header;
    /// - for each HTTP method that doesn't have a handler, add a default handler
    ///   that returns a 405, listing the supported HTTP methods in the Allow header.
    ///   We don't add an explicit 405 for HEAD, because we fallback on GET if missing.
    pub(crate) fn add_default_handlers(&mut self) {
        for path in self.paths.clone().iter() {
            let mut http_methods_with_handlers = self.get_http_methods_with_handlers(path);

            if !http_methods_with_handlers.contains(&Method::OPTIONS) {
                self.add_default_options_handler(path, &http_methods_with_handlers);
                http_methods_with_handlers.insert(Method::OPTIONS);
            }

            for http_method in &HTTP_METHODS {
                // If missing, HEAD falls back on GET
                if !http_methods_with_handlers.contains(&http_method)
                    && http_method != &Method::HEAD
                {
                    self.add_method_not_allowed_handler(
                        path,
                        http_method,
                        &http_methods_with_handlers,
                    )
                }
            }
        }
    }

    // Register a default `Method Not Allowed` handler: it returns a 405 with an Allow header
    // specifying the list of supported HTTP methods for `path`.
    fn add_method_not_allowed_handler(
        &mut self,
        path: &str,
        method: &http::Method,
        supported_http_methods: &HashSet<http::Method>,
    ) {
        let allow_header = supported_http_methods
            .into_iter()
            .map(|m| format!("{}", m))
            .collect::<Vec<_>>()
            .join(", ");
        self.method_map
            .entry(method.to_owned())
            .or_insert_with(MethodRouter::new)
            .add(
                path,
                Box::new(move |_| {
                    // Only way to get this to compile apparently.
                    let allow_header = allow_header.clone();
                    Box::pin(async move {
                        let response = crate::Response::new(405)
                            .set_header("Allow", allow_header.clone())
                            .body(http_service::Body::empty());
                        response
                    })
                }),
            );
    }

    // Register a default OPTIONS handler: it returns a 204 with an Allow header
    // specifying the list of supported HTTP methods for `path`.
    fn add_default_options_handler(
        &mut self,
        path: &str,
        supported_http_methods: &HashSet<http::Method>,
    ) {
        let allow_header = supported_http_methods
            .into_iter()
            .map(|m| format!("{}", m))
            .collect::<Vec<_>>()
            .join(", ");
        self.method_map
            .entry(http::Method::OPTIONS)
            .or_insert_with(MethodRouter::new)
            .add(
                path,
                Box::new(move |_| {
                    // Only way to get this to compile apparently.
                    let allow_header = allow_header.clone();
                    Box::pin(async move {
                        let response = crate::Response::new(204)
                            .set_header("Allow", allow_header.clone())
                            .body(http_service::Body::empty());
                        response
                    })
                }),
            );
    }

    // Determine for which HTTP methods there is a registered handler for `path`
    fn get_http_methods_with_handlers(&self, path: &String) -> HashSet<http::Method> {
        let mut http_methods_with_handler = HashSet::with_capacity(HTTP_METHODS.len());
        for http_method in &HTTP_METHODS {
            let method_router = self.method_map.get(http_method);
            if let Some(method_router) = method_router {
                if method_router.recognize(path).is_ok() {
                    http_methods_with_handler.insert(http_method.to_owned());
                }
            }
        }
        http_methods_with_handler
    }

    pub(crate) fn route(&self, path: &str, method: http::Method) -> Selection<'_, State> {
        if let Some(Match { handler, params }) = self
            .method_map
            .get(&method)
            .and_then(|r| r.recognize(path).ok())
        {
            Selection {
                endpoint: &**handler,
                params,
            }
        } else if method == http::Method::HEAD {
            // If it is a HTTP HEAD request then check if there is a callback in the endpoints map
            // if not then fallback to the behavior of HTTP GET else proceed as usual

            self.route(path, http::Method::GET)
        } else {
            Selection {
                endpoint: &not_found_endpoint,
                params: Params::new(),
            }
        }
    }
}

fn not_found_endpoint<State>(_cx: Request<State>) -> BoxFuture<'static, Response> {
    Box::pin(async move { Response::new(http::StatusCode::NOT_FOUND.as_u16()) })
}
