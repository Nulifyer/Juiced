use crate::ApiError;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Openai,
    Anthropic,
    Ollama,
    Mcp,
    Automatic1111,
    Realtime,
    Wyoming,
}

pub const HTTP_PROTOCOLS: [Protocol; 6] = [
    Protocol::Openai,
    Protocol::Anthropic,
    Protocol::Ollama,
    Protocol::Mcp,
    Protocol::Automatic1111,
    Protocol::Realtime,
];

pub const OPENAI_ROUTES: [&str; 8] = [
    "/models",
    "/chat/completions",
    "/completions",
    "/responses",
    "/embeddings",
    "/audio/transcriptions",
    "/audio/speech",
    "/images/generations",
];

const NATIVE_PREFIXES: [&str; 4] = ["/api/v0", "/api/v1", "/v0", "/v1"];

impl Protocol {
    pub fn id(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Anthropic => "anthropic",
            Self::Ollama => "ollama",
            Self::Mcp => "mcp",
            Self::Automatic1111 => "automatic1111",
            Self::Realtime => "realtime",
            Self::Wyoming => "wyoming",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Openai => "OpenAI",
            Self::Anthropic => "Anthropic",
            Self::Ollama => "Ollama",
            Self::Mcp => "MCP",
            Self::Automatic1111 => "AUTOMATIC1111",
            Self::Realtime => "Realtime",
            Self::Wyoming => "Wyoming",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        HTTP_PROTOCOLS
            .into_iter()
            .chain([Self::Wyoming])
            .find(|protocol| protocol.id() == id)
    }

    fn index(self) -> usize {
        self as usize
    }

    /// Native management routes share the `/api/v0`, `/api/v1` and `/v1` prefixes with client
    /// protocols, so only the exact client paths below belong to a protocol.
    pub fn classify(path: &str) -> Option<Self> {
        if path == "/v1/messages" || path == "/v1/messages/count_tokens" {
            return Some(Self::Anthropic);
        }
        if path == "/mcp" {
            return Some(Self::Mcp);
        }
        if path.starts_with("/sdapi/") {
            return Some(Self::Automatic1111);
        }
        if let Some(native) = NATIVE_PREFIXES.iter().find_map(|prefix| {
            path.strip_prefix(prefix)
                .filter(|rest| rest.starts_with('/'))
        }) {
            if native == "/realtime" {
                return Some(Self::Realtime);
            }
            return OPENAI_ROUTES.contains(&native).then_some(Self::Openai);
        }
        path.starts_with("/api/").then_some(Self::Ollama)
    }

    /// Endpoint inventory paths are unprefixed for native routes.
    pub fn of_endpoint(path: &str, native: bool) -> Option<Self> {
        if native {
            Self::classify(&format!("/v1{path}"))
        } else {
            Self::classify(path)
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub disabled: BTreeSet<Protocol>,
}

impl Settings {
    pub fn is_default(&self) -> bool {
        self.disabled.is_empty()
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.disabled.contains(&Protocol::Wyoming) {
            return Err("Wyoming is turned on and off by its own settings");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct LastRequest {
    pub at_unix_ms: u64,
    pub status: u16,
}

pub struct Gate {
    disabled: [AtomicBool; 6],
    last: Mutex<[Option<LastRequest>; 7]>,
}

impl Gate {
    pub fn new(settings: &Settings) -> Self {
        let gate = Self {
            disabled: Default::default(),
            last: Mutex::new([None; 7]),
        };
        gate.apply(settings);
        gate
    }

    pub fn apply(&self, settings: &Settings) {
        for protocol in HTTP_PROTOCOLS {
            self.disabled[protocol.index()]
                .store(settings.disabled.contains(&protocol), Ordering::Release);
        }
    }

    pub fn enabled(&self, protocol: Protocol) -> bool {
        protocol == Protocol::Wyoming || !self.disabled[protocol.index()].load(Ordering::Acquire)
    }

    pub fn check(&self, protocol: Protocol) -> Result<(), ApiError> {
        if self.enabled(protocol) {
            return Ok(());
        }
        Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "protocol_disabled",
            format!(
                "The {} API is turned off on this server. An operator can turn it on in Connect.",
                protocol.title()
            ),
        ))
    }

    pub fn observe(&self, protocol: Protocol, status: u16) {
        self.last.lock().unwrap_or_else(|error| error.into_inner())[protocol.index()] =
            Some(LastRequest {
                at_unix_ms: crate::api_keys::now_ms(),
                status,
            });
    }

    pub fn last(&self, protocol: Protocol) -> Option<LastRequest> {
        self.last.lock().unwrap_or_else(|error| error.into_inner())[protocol.index()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_keeps_native_management_routes_ungated() {
        for (path, expected) in [
            ("/v1/chat/completions", Some(Protocol::Openai)),
            ("/api/v1/models", Some(Protocol::Openai)),
            ("/v0/audio/speech", Some(Protocol::Openai)),
            ("/v1/models/fixture.one", None),
            ("/v1/juiced/protocols", None),
            ("/api/v1/health", None),
            ("/v1/load", None),
            ("/v1/messages", Some(Protocol::Anthropic)),
            ("/v1/messages/count_tokens", Some(Protocol::Anthropic)),
            ("/api/tags", Some(Protocol::Ollama)),
            ("/api/chat", Some(Protocol::Ollama)),
            ("/mcp", Some(Protocol::Mcp)),
            ("/sdapi/v1/txt2img", Some(Protocol::Automatic1111)),
            ("/v1/realtime", Some(Protocol::Realtime)),
            ("/index.html", None),
        ] {
            assert_eq!(Protocol::classify(path), expected, "{path}");
        }
        assert_eq!(
            Protocol::of_endpoint("/chat/completions", true),
            Some(Protocol::Openai)
        );
        assert_eq!(
            Protocol::of_endpoint("/api/tags", false),
            Some(Protocol::Ollama)
        );
    }

    #[test]
    fn settings_default_to_every_protocol_enabled() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert!(settings.is_default());
        assert_eq!(serde_json::to_string(&settings).unwrap(), "{}");
        let gate = Gate::new(&serde_json::from_str(r#"{"disabled":["ollama"]}"#).unwrap());
        assert!(!gate.enabled(Protocol::Ollama));
        assert!(gate.enabled(Protocol::Openai));
        assert_eq!(
            gate.check(Protocol::Ollama).unwrap_err().code,
            "protocol_disabled"
        );
        assert!(serde_json::from_str::<Settings>(r#"{"disabled":["gopher"]}"#).is_err());
        let wyoming: Settings = serde_json::from_str(r#"{"disabled":["wyoming"]}"#).unwrap();
        assert!(wyoming.validate().is_err());
    }
}
