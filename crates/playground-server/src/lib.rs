//! # playground-server — HTTP parse service (issue 038)
//!
//! A std-only parse service: `POST /parse {pack, source}` returns the
//! tokens and green tree as deterministic JSON, `GET /packs` lists the
//! registered packs. The router is a **pure function** — every routing,
//! dispatch, and serialization decision is testable without sockets;
//! the TCP layer (`serve`) is a thin accept loop with graceful
//! shutdown and bounded request sizes.
//!
//! ```no_run
//! use std::sync::atomic::AtomicBool;
//! use std::sync::Arc;
//! use playground_server::{Registry, Request, Method, route};
//!
//! let registry = Registry::reference();
//! let req = Request {
//!     method: Method::Post,
//!     path: "/parse".to_owned(),
//!     body: br#"{"pack":"json","source":"[1]"}"#.to_vec(),
//! };
//! let resp = route(&registry, 4096, &req);
//! assert_eq!(resp.status, 200);
//! assert!(resp.body.contains("\"ok\": true"), "{}", resp.body);
//! ```

pub mod http;
pub mod wire;

pub use http::{ReqError, Server, parse_request, serialize_response};

use std::sync::Arc;

use runtime_core::tree::GreenNode;

/// A registered pack: name + reference-parser entry point.
#[derive(Clone, Copy)]
pub struct PackEntry {
    /// Pack name as used in `/parse` requests.
    pub name: &'static str,
    /// The lossless reference parser.
    pub parse: fn(&str) -> Result<Arc<GreenNode>, String>,
}

/// The pack registry (fixed registration order — deterministic).
#[derive(Default)]
pub struct Registry {
    packs: Vec<PackEntry>,
}

impl Registry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a pack.
    pub fn register(&mut self, entry: PackEntry) {
        self.packs.push(entry);
    }

    /// The reference registry: every shipped pack, in language order.
    #[must_use]
    pub fn reference() -> Self {
        type ParseFn = fn(&str) -> Result<Arc<GreenNode>, String>;
        let mut registry = Self::new();
        let entries: [(&'static str, ParseFn); 7] = [
            ("json", packs_json::parse),
            ("toml", packs_toml::parse),
            ("python", packs_python::parse),
            ("typescript", packs_typescript::parse),
            ("rust", packs_rust::parse),
            ("c", packs_c::parse),
            ("sql", packs_sql::parse),
        ];
        for (name, parse) in entries {
            registry.register(PackEntry { name, parse });
        }
        registry
    }

    /// Look up a pack by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&PackEntry> {
        self.packs.iter().find(|entry| entry.name == name)
    }

    /// Registered pack names in registration order.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.packs.iter().map(|entry| entry.name).collect()
    }
}

/// HTTP methods the router distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// `GET`
    Get,
    /// `POST`
    Post,
}

/// A parsed request (routing-relevant fields only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The method.
    pub method: Method,
    /// The path (no query string support — the service is stateless).
    pub path: String,
    /// The raw body bytes.
    pub body: Vec<u8>,
}

/// A routing decision: status + deterministic JSON body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// HTTP status code.
    pub status: u16,
    /// JSON body.
    pub body: String,
}

/// Route a request against the registry (pure; bounded body size).
#[must_use]
pub fn route(registry: &Registry, max_body: usize, req: &Request) -> Response {
    match (req.method, req.path.as_str()) {
        (Method::Get, "/packs") => Response {
            status: 200,
            body: packs_json_body(&registry.names()),
        },
        (Method::Post, "/packs") | (Method::Get, "/parse") => Response {
            status: 405,
            body: "{\"error\":\"method not allowed\"}".to_owned(),
        },
        (Method::Post, "/parse") => route_parse(registry, max_body, req),
        _ => Response {
            status: 404,
            body: "{\"error\":\"not found\"}".to_owned(),
        },
    }
}

fn route_parse(registry: &Registry, max_body: usize, req: &Request) -> Response {
    if req.body.len() > max_body {
        return Response {
            status: 413,
            body: "{\"error\":\"request body too large\"}".to_owned(),
        };
    }
    let Ok(body) = std::str::from_utf8(&req.body) else {
        return Response {
            status: 400,
            body: "{\"error\":\"malformed request body\"}".to_owned(),
        };
    };
    let Ok((pack, source)) = wire::extract_pack_source(body) else {
        return Response {
            status: 400,
            body: "{\"error\":\"malformed request body\"}".to_owned(),
        };
    };
    let Some(entry) = registry.get(&pack) else {
        let quoted = wire::json_str(&pack);
        return Response {
            status: 404,
            body: format!(
                "{{\"error\":\"unknown pack: {}\"}}",
                &quoted[1..quoted.len() - 1]
            ),
        };
    };
    let tree = (entry.parse)(&source);
    Response {
        status: 200,
        body: wire::render_parse(&pack, tree),
    }
}

fn packs_json_body(names: &[&str]) -> String {
    let mut out = String::from("{\"packs\":[");
    for (index, name) in names.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&wire::json_str(name));
    }
    out.push_str("]}");
    out
}
