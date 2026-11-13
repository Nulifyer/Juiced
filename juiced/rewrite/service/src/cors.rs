use crate::{ApiError, logs::Level, supervisor::Manager};
use axum::{
    extract::{Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{
    collections::HashSet,
    net::IpAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub const ALLOW_METHODS: &str = "GET, POST, PUT, DELETE, OPTIONS";
pub const ALLOW_HEADERS: &str = "Content-Type, Authorization, X-Client-Session-Id, X-Account-Session-Id, mcp-protocol-version, traceparent, Mcp-Session-Id, X-Api-Key, Anthropic-Version, Anthropic-Beta, If-None-Match";
const SELF_REFRESH: Duration = Duration::from_secs(60);
const PRIVATE_NETWORK_REQUEST: &str = "access-control-request-private-network";
const PRIVATE_NETWORK_ALLOW: &str = "access-control-allow-private-network";

/// Native `allowed_origins` semantics: requests without `Origin` pass, loopback and app
/// schemes pass, a non-empty list is authoritative, and otherwise only same-origin passes.
pub struct Policy {
    allowed: String,
    self_names: Mutex<(Option<Instant>, Arc<HashSet<String>>)>,
}

impl Policy {
    /// `LEMONADE_ALLOWED_ORIGINS` takes precedence over the configured `allowed_origins`.
    pub fn resolve(configured: Option<&str>) -> Self {
        let allowed = std::env::var("LEMONADE_ALLOWED_ORIGINS")
            .ok()
            .or_else(|| configured.map(str::to_owned))
            .unwrap_or_default();
        Self::new(allowed.trim())
    }

    pub fn new(allowed: &str) -> Self {
        Self {
            allowed: allowed.to_owned(),
            self_names: Mutex::new((None, Arc::default())),
        }
    }

    pub fn allowed_origins(&self) -> &str {
        &self.allowed
    }

    pub fn allows(&self, origin: &str, headers: &HeaderMap) -> bool {
        let Some(request) = Origin::parse(origin) else {
            return false;
        };
        if request.is_loopback() {
            return true;
        }
        if !self.allowed.is_empty() {
            if self.allowed == "*" {
                return true;
            }
            return self.allowed.split(',').map(str::trim).any(|item| {
                if item.eq_ignore_ascii_case("null") {
                    return request.scheme.is_empty()
                        && request.host == "null"
                        && request.port.is_none();
                }
                Origin::parse(item)
                    .is_some_and(|pattern| !pattern.scheme.is_empty() && request.matches(&pattern))
            });
        }
        self.same_origin(&request, headers)
    }

    /// The native check accepts an `http` origin naming this host. The two additions cannot
    /// be produced by DNS rebinding: an `https` origin reaches this plain-HTTP listener only
    /// through a TLS proxy holding a certificate for that name, and an IP-literal origin
    /// involves no name resolution at all.
    fn same_origin(&self, request: &Origin, headers: &HeaderMap) -> bool {
        if !matches!(request.scheme.as_str(), "http" | "https") {
            return false;
        }
        let forwarded = headers
            .get("x-forwarded-host")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(',').next());
        let host = headers
            .get(header::HOST)
            .and_then(|value| value.to_str().ok());
        host.into_iter().chain(forwarded).any(|candidate| {
            let Some(mut target) = Origin::parse(candidate.trim()) else {
                return false;
            };
            target.scheme.clone_from(&request.scheme);
            if !request.matches(&target) {
                return false;
            }
            request.scheme == "https"
                || request
                    .host
                    .trim_matches(['[', ']'])
                    .parse::<IpAddr>()
                    .is_ok()
                || self.self_names().contains(&target.host)
        })
    }

    fn self_names(&self) -> Arc<HashSet<String>> {
        let mut cached = self
            .self_names
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if cached.0.is_none_or(|at| at.elapsed() >= SELF_REFRESH) {
            *cached = (Some(Instant::now()), Arc::new(host_names()));
        }
        cached.1.clone()
    }
}

fn host_names() -> HashSet<String> {
    let mut names: HashSet<String> = ["localhost", "127.0.0.1", "::1", "[::1]"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    if let Some(name) = hostname() {
        let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
        if !name.is_empty() {
            if !name.contains('.') {
                names.insert(format!("{name}.local"));
            }
            names.insert(name);
        }
    }
    names
}

#[cfg(unix)]
fn hostname() -> Option<String> {
    let mut buffer = [0u8; 256];
    // SAFETY: the buffer is writable for its full length and gethostname writes at most that.
    let status = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    (status == 0).then(|| {
        let end = buffer
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(buffer.len());
        String::from_utf8_lossy(&buffer[..end]).into_owned()
    })
}

#[cfg(not(unix))]
fn hostname() -> Option<String> {
    std::env::var("COMPUTERNAME").ok()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Origin {
    scheme: String,
    host: String,
    port: Option<u16>,
    any_port: bool,
}

impl Origin {
    fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() || text.contains(['@', '?', '#']) {
            return None;
        }
        if text.eq_ignore_ascii_case("null") {
            return Some(Self {
                host: "null".into(),
                ..Self::default()
            });
        }
        let (scheme, mut rest) = match text.split_once("://") {
            Some((scheme, rest)) => (scheme.to_ascii_lowercase(), rest),
            None => (String::new(), text),
        };
        let web = matches!(scheme.as_str(), "http" | "https" | "ws" | "wss");
        if let Some((authority, path)) = rest.split_once('/') {
            if !path.is_empty() && web {
                return None;
            }
            rest = authority;
        }
        if rest.is_empty() {
            return (!scheme.is_empty()).then(|| Self {
                host: scheme.clone(),
                scheme,
                ..Self::default()
            });
        }
        let mut origin = Self {
            scheme,
            ..Self::default()
        };
        let port = if let Some(bracketed) = rest.strip_prefix('[') {
            let (address, after) = bracketed.split_once(']')?;
            let address = address.split('%').next().unwrap_or_default();
            origin.host = format!("[{}]", address.to_ascii_lowercase());
            match after {
                "" => None,
                after => Some(after.strip_prefix(':')?),
            }
        } else if rest.matches(':').count() == 1 {
            let (host, port) = rest.split_once(':')?;
            origin.host = host.to_ascii_lowercase();
            Some(port)
        } else {
            origin.host = rest.to_ascii_lowercase();
            None
        };
        match port {
            Some("*") => origin.any_port = true,
            Some(port)
                if !port.is_empty()
                    && port.len() <= 5
                    && port.bytes().all(|b| b.is_ascii_digit()) =>
            {
                origin.port = Some(port.parse().ok()?);
            }
            Some(_) => return None,
            None => {}
        }
        if origin.host.len() > 1 && !origin.host.starts_with('[') && origin.host.ends_with('.') {
            origin.host.pop();
        }
        (!origin.host.is_empty()).then_some(origin)
    }

    fn effective_port(&self) -> Option<u16> {
        self.port.or(match self.scheme.as_str() {
            "http" | "ws" => Some(80),
            "https" | "wss" => Some(443),
            _ => None,
        })
    }

    fn matches(&self, pattern: &Self) -> bool {
        if self.scheme != pattern.scheme {
            return false;
        }
        let host = if pattern.host == "*" {
            true
        } else if let Some(suffix) = pattern.host.strip_prefix("*.") {
            self.host.len() > suffix.len() + 1
                && self.host.ends_with(suffix)
                && self.host.as_bytes()[self.host.len() - suffix.len() - 1] == b'.'
        } else {
            self.host == pattern.host
        };
        host && (pattern.any_port || self.effective_port() == pattern.effective_port())
    }

    fn is_loopback(&self) -> bool {
        matches!(
            self.host.as_str(),
            "localhost" | "127.0.0.1" | "[::1]" | "::1" | "tauri.localhost"
        ) || (!self.scheme.is_empty()
            && !matches!(self.scheme.as_str(), "http" | "https" | "ws" | "wss"))
    }
}

pub async fn layer(State(manager): State<Arc<Manager>>, request: Request, next: Next) -> Response {
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .map(|value| value.to_str().unwrap_or("\u{fffd}").to_owned());
    let private_network = request
        .headers()
        .get(PRIVATE_NETWORK_REQUEST)
        .is_some_and(|value| value == "true");
    let requested_headers = request
        .headers()
        .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
        .cloned();
    let preflight = request.method() == Method::OPTIONS;
    let allowed = match &origin {
        Some(origin) if !manager.cors().allows(origin, request.headers()) => {
            manager.logs().service(
                Level::Warning,
                &format!(
                    "Rejected request from unauthorized origin: {origin}. Set allowed_origins in the configuration or LEMONADE_ALLOWED_ORIGINS to allow this origin."
                ),
            );
            let mut response = ApiError::new(
                StatusCode::FORBIDDEN,
                "origin_not_allowed",
                "Origin not allowed",
            )
            .into_response();
            decorate(response.headers_mut(), None, false, None);
            return response;
        }
        Some(origin) => Some(origin.clone()),
        None => None,
    };
    let mut response = if preflight {
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(request).await
    };
    decorate(
        response.headers_mut(),
        allowed.as_deref(),
        private_network,
        requested_headers.filter(|_| preflight && allowed.is_some()),
    );
    response
}

fn decorate(
    headers: &mut HeaderMap,
    origin: Option<&str>,
    private_network: bool,
    requested: Option<HeaderValue>,
) {
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static(ALLOW_METHODS),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        allow_headers(requested.as_ref().and_then(|value| value.to_str().ok())),
    );
    let vary = headers
        .get_all(header::VARY)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if !vary
        .iter()
        .any(|value| value.eq_ignore_ascii_case("origin") || value == "*")
    {
        let mut merged = vary;
        merged.push("Origin".into());
        if let Ok(value) = HeaderValue::from_str(&merged.join(", ")) {
            headers.insert(header::VARY, value);
        }
    }
    if let Some(origin) = origin.and_then(|origin| HeaderValue::from_str(origin).ok()) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        if private_network {
            headers.insert(
                HeaderName::from_static(PRIVATE_NETWORK_ALLOW),
                HeaderValue::from_static("true"),
            );
        }
    }
}

/// Browser SDKs send their own telemetry headers; a trusted origin may send any it asks for.
fn allow_headers(requested: Option<&str>) -> HeaderValue {
    let mut names = ALLOW_HEADERS.to_owned();
    for name in requested
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
    {
        let listed = ALLOW_HEADERS
            .split(", ")
            .any(|listed| listed.eq_ignore_ascii_case(name));
        if !name.is_empty() && !listed && HeaderName::from_bytes(name.as_bytes()).is_ok() {
            names.push_str(", ");
            names.push_str(name);
        }
    }
    HeaderValue::from_str(&names).unwrap_or(HeaderValue::from_static(ALLOW_HEADERS))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, value.parse().unwrap());
        headers
    }

    #[test]
    fn default_policy_matches_the_native_layers() {
        let policy = Policy::new("");
        let lan = host("192.168.1.5:8080");
        for origin in [
            "http://localhost:5173",
            "http://127.0.0.1:9",
            "http://[::1]:3000",
            "app://x",
            "vscode-webview://abc",
            "tauri://localhost",
        ] {
            assert!(policy.allows(origin, &lan), "{origin}");
        }
        for origin in [
            "https://evil.example",
            "null",
            "http://foo.localhost:3000",
            "http://192.168.1.6:8080",
            "http://192.168.1.5:8081",
            "https://192.168.1.5:8080/path",
            "garbage",
        ] {
            assert!(!policy.allows(origin, &lan), "{origin}");
        }
        assert!(policy.allows("http://192.168.1.5:8080", &lan));
        assert!(policy.allows("http://[fd00::5]:8080", &host("[fd00::5]:8080")));
    }

    #[test]
    fn named_same_origin_needs_tls_or_a_local_name() {
        let policy = Policy::new("");
        let proxied = host("juiced.example.net");
        assert!(policy.allows("https://juiced.example.net", &proxied));
        assert!(policy.allows("https://juiced.example.net:443", &proxied));
        assert!(!policy.allows("https://juiced.example.net:8443", &proxied));
        assert!(!policy.allows("http://juiced.example.net", &proxied));
        assert!(!policy.allows("https://other.example.net", &proxied));
        let mut rewritten = host("juiced:8080");
        rewritten.insert("x-forwarded-host", "juiced.example.net".parse().unwrap());
        assert!(policy.allows("https://juiced.example.net", &rewritten));
        assert!(!policy.allows("http://rebound.example:8080", &host("rebound.example:8080")));
        let name = hostname().unwrap().to_ascii_lowercase();
        assert!(policy.allows(
            &format!("http://{name}:8080"),
            &host(&format!("{name}:8080"))
        ));
    }

    #[test]
    fn an_explicit_list_is_authoritative() {
        let policy = Policy::new("https://app.example.com, http://*.lan:*, null");
        let headers = host("juiced.example.net");
        assert!(policy.allows("https://app.example.com", &headers));
        assert!(policy.allows("https://app.example.com:443", &headers));
        assert!(!policy.allows("http://app.example.com", &headers));
        assert!(policy.allows("http://box.lan:3000", &headers));
        assert!(!policy.allows("http://lan:3000", &headers));
        assert!(policy.allows("null", &headers));
        assert!(policy.allows("http://localhost:1", &headers));
        assert!(!policy.allows("https://juiced.example.net", &headers));
        assert!(Policy::new("*").allows("https://anything.example", &headers));
    }

    #[test]
    fn decoration_merges_vary_and_lists_requested_headers_for_trusted_preflight() {
        let mut headers = HeaderMap::new();
        headers.insert(header::VARY, "Accept-Encoding".parse().unwrap());
        decorate(
            &mut headers,
            Some("http://localhost:3000"),
            true,
            Some("authorization, x-stainless-os".parse().unwrap()),
        );
        assert_eq!(headers[header::VARY], "Accept-Encoding, Origin");
        assert_eq!(
            headers[header::ACCESS_CONTROL_ALLOW_ORIGIN],
            "http://localhost:3000"
        );
        assert_eq!(headers[PRIVATE_NETWORK_ALLOW], "true");
        let allowed = headers[header::ACCESS_CONTROL_ALLOW_HEADERS]
            .to_str()
            .unwrap();
        assert!(allowed.starts_with("Content-Type, Authorization,"));
        assert!(allowed.ends_with(", x-stainless-os"));
        assert_eq!(allowed.matches("uthorization").count(), 1);
        let mut plain = HeaderMap::new();
        decorate(&mut plain, None, true, None);
        assert_eq!(plain[header::VARY], "Origin");
        assert!(!plain.contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
        assert!(!plain.contains_key(PRIVATE_NETWORK_ALLOW));
    }
}
