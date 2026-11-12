use crate::ApiError;
use axum::http::{HeaderMap, StatusCode, header};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use subtle::ConstantTimeEq;

#[derive(Clone, Default)]
pub struct Auth {
    pub api_key: Option<String>,
    pub admin_key: Option<String>,
}

impl Auth {
    pub fn from_env() -> Self {
        fn key(name: &str) -> Option<String> {
            std::env::var(name).ok().filter(|value| !value.is_empty())
        }
        Self {
            api_key: key("LEMONADE_API_KEY"),
            admin_key: key("LEMONADE_ADMIN_API_KEY"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    Read,
    Inference,
    Operator,
    TrustedLan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Authority {
    Read,
    Inference,
    Operator,
}

impl Authority {
    pub fn from_label(label: &str) -> Self {
        match label {
            "read" => Self::Read,
            "inference" => Self::Inference,
            _ => Self::Operator,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct RequestContext {
    pub principal: &'static str,
    pub role: Role,
    pub credential_epoch: Arc<str>,
}

impl RequestContext {
    pub fn operator(&self) -> bool {
        matches!(self.role, Role::Operator | Role::TrustedLan)
    }

    fn allows(&self, authority: Authority) -> bool {
        match authority {
            Authority::Read => true,
            Authority::Inference => self.role != Role::Read,
            Authority::Operator => self.operator(),
        }
    }
}

struct Credential {
    token: String,
    context: RequestContext,
}

pub struct Authorization {
    regular: Option<Credential>,
    operator: Option<Credential>,
    anonymous: RequestContext,
    keys: Arc<crate::api_keys::KeyStore>,
}

impl Authorization {
    pub fn new(auth: Auth) -> Self {
        Self::with_keys(auth, crate::api_keys::KeyStore::memory())
    }

    pub fn with_keys(auth: Auth, keys: Arc<crate::api_keys::KeyStore>) -> Self {
        let credential = |token: String, principal, role| {
            let epoch: Arc<str> = format!(
                "{:x}",
                Sha256::digest(format!("juiced-auth-v1\0{principal}\0{token}"))
            )
            .into();
            Credential {
                token,
                context: RequestContext {
                    principal,
                    role,
                    credential_epoch: epoch,
                },
            }
        };
        let regular_role = if auth.admin_key.is_some() {
            Role::Inference
        } else {
            Role::Operator
        };
        Self {
            regular: auth
                .api_key
                .map(|token| credential(token, "configured-api", regular_role)),
            operator: auth
                .admin_key
                .map(|token| credential(token, "configured-admin", Role::Operator)),
            anonymous: RequestContext {
                principal: "anonymous-trusted-lan",
                role: Role::TrustedLan,
                credential_epoch: "anonymous-trusted-lan-v1".into(),
            },
            keys,
        }
    }

    pub fn keys(&self) -> &Arc<crate::api_keys::KeyStore> {
        &self.keys
    }

    pub fn environment_operator(&self) -> bool {
        self.operator.is_some() || self.regular.is_some()
    }

    pub fn enforcing(&self) -> bool {
        self.regular.is_some() || self.operator.is_some() || self.keys.enforcing()
    }

    pub fn environment_keys(&self) -> Vec<serde_json::Value> {
        [
            ("LEMONADE_ADMIN_API_KEY", &self.operator),
            ("LEMONADE_API_KEY", &self.regular),
        ]
        .into_iter()
        .filter_map(|(name, credential)| {
            let scope = match credential.as_ref()?.context.role {
                Role::Inference => "inference",
                _ => "operator",
            };
            Some(serde_json::json!({"id": format!("env:{name}"), "name": name, "scope": scope, "source": "environment"}))
        })
        .collect()
    }

    pub fn authenticate(
        &self,
        headers: &HeaderMap,
        authority: Authority,
    ) -> Result<RequestContext, ApiError> {
        if !self.enforcing() {
            return Ok(self.anonymous.clone());
        }
        let unauthorized = || {
            ApiError::new(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "A valid API key is required.",
            )
        };
        if headers.get_all(header::AUTHORIZATION).iter().count() > 1
            || headers.get_all("x-api-key").iter().count() > 1
        {
            return Err(unauthorized());
        }
        let bearer = headers
            .get(header::AUTHORIZATION)
            .map(|value| {
                value
                    .to_str()
                    .ok()
                    .and_then(|text| text.split_once(' '))
                    .filter(|(scheme, token)| {
                        scheme.eq_ignore_ascii_case("bearer")
                            && !token.is_empty()
                            && !token.contains(char::is_whitespace)
                    })
                    .map(|(_, token)| token)
                    .ok_or_else(unauthorized)
            })
            .transpose()?;
        let api_key = headers
            .get("x-api-key")
            .map(|value| value.to_str().map_err(|_| unauthorized()))
            .transpose()?;
        if let (Some(left), Some(right)) = (bearer, api_key)
            && !bool::from(left.as_bytes().ct_eq(right.as_bytes()))
        {
            return Err(unauthorized());
        }
        let token = bearer
            .or(api_key)
            .or_else(|| websocket_key(headers))
            .ok_or_else(unauthorized)?;
        let context = match self
            .operator
            .iter()
            .chain(self.regular.iter())
            .find(|key| bool::from(token.as_bytes().ct_eq(key.token.as_bytes())))
        {
            Some(credential) => credential.context.clone(),
            None => self.issued(token)?,
        };
        if !context.allows(authority) {
            return Err(if authority == Authority::Operator {
                ApiError::new(
                    StatusCode::UNAUTHORIZED,
                    "operator_required",
                    "An operator API key is required.",
                )
            } else {
                ApiError::new(
                    StatusCode::UNAUTHORIZED,
                    "inference_required",
                    "This read-only key cannot run inference. Use an inference or operator key.",
                )
            });
        }
        Ok(context)
    }

    fn issued(&self, token: &str) -> Result<RequestContext, ApiError> {
        use crate::api_keys::{Lookup, Scope};
        if let Some(reason) = self.keys.unavailable() {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "key_store_unavailable",
                format!(
                    "The API key store {reason}. Issued keys are refused until {} is fixed or removed.",
                    crate::api_keys::FILENAME
                ),
            ));
        }
        match self.keys.lookup(token) {
            Lookup::Found(found) => Ok(RequestContext {
                principal: match found.scope {
                    Scope::Read => "issued-read",
                    Scope::Inference => "issued-inference",
                    Scope::Operator => "issued-operator",
                },
                role: match found.scope {
                    Scope::Read => Role::Read,
                    Scope::Inference => Role::Inference,
                    Scope::Operator => Role::Operator,
                },
                credential_epoch: found.epoch,
            }),
            Lookup::Expired => Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "api_key_expired",
                "This API key has expired. Ask an operator for a new key.",
            )),
            Lookup::Unknown => Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "A valid API key is required.",
            )),
        }
    }
}

/// Browsers cannot set headers on WebSocket requests, so OpenAI Realtime clients offer the
/// key as a subprotocol. It is consulted only when no credential header is present.
fn websocket_key(headers: &HeaderMap) -> Option<&str> {
    let mut keys = headers
        .get_all(header::SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|protocol| protocol.trim().strip_prefix("openai-insecure-api-key."));
    let key = keys.next()?;
    (keys.next().is_none() && !key.is_empty()).then_some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn websocket_subprotocol_keys_need_one_offer_and_no_credential_header() {
        let auth = Authorization::new(Auth {
            api_key: Some("client".into()),
            admin_key: None,
        });
        let mut headers = HeaderMap::new();
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            "realtime, openai-insecure-api-key.client".parse().unwrap(),
        );
        assert!(auth.authenticate(&headers, Authority::Inference).is_ok());
        headers.insert(header::AUTHORIZATION, "Bearer wrong".parse().unwrap());
        assert!(auth.authenticate(&headers, Authority::Inference).is_err());
        let mut twice = HeaderMap::new();
        twice.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            "openai-insecure-api-key.client, openai-insecure-api-key.client"
                .parse()
                .unwrap(),
        );
        assert!(auth.authenticate(&twice, Authority::Inference).is_err());
    }

    #[test]
    fn normalized_transports_share_context_but_roles_and_rotation_do_not() {
        let auth = Authorization::new(Auth {
            api_key: Some("client".into()),
            admin_key: Some("operator".into()),
        });
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer client".parse().unwrap());
        let bearer = auth.authenticate(&headers, Authority::Inference).unwrap();
        headers.clear();
        headers.insert("x-api-key", "client".parse().unwrap());
        let other = auth.authenticate(&headers, Authority::Inference).unwrap();
        assert_eq!(bearer.credential_epoch, other.credential_epoch);
        assert_eq!(other.role, Role::Inference);
        assert!(auth.authenticate(&headers, Authority::Operator).is_err());
        headers.insert(header::AUTHORIZATION, "Bearer operator".parse().unwrap());
        assert!(auth.authenticate(&headers, Authority::Inference).is_err());
        headers.remove("x-api-key");
        let admin = auth.authenticate(&headers, Authority::Operator).unwrap();
        assert_ne!(admin.credential_epoch, bearer.credential_epoch);
        let rotated = Authorization::new(Auth {
            api_key: Some("rotated".into()),
            admin_key: Some("operator".into()),
        });
        headers.insert(header::AUTHORIZATION, "Bearer rotated".parse().unwrap());
        assert_ne!(
            rotated
                .authenticate(&headers, Authority::Inference)
                .unwrap()
                .credential_epoch,
            bearer.credential_epoch
        );
    }

    #[tokio::test]
    async fn issued_keys_follow_their_scope_beside_environment_keys() {
        use crate::api_keys::{Create, KeyStore, Scope};
        let keys = KeyStore::memory();
        let auth = Authorization::with_keys(Auth::default(), keys.clone());
        assert_eq!(
            auth.authenticate(&HeaderMap::new(), Authority::Operator)
                .unwrap()
                .role,
            Role::TrustedLan
        );
        let issue = async |name: &str, scope| {
            keys.create(
                Create {
                    name: name.into(),
                    scope,
                    expires_in_days: None,
                },
                false,
            )
            .await
            .unwrap()
            .1
        };
        let operator = issue("console", Scope::Operator).await;
        let reader = issue("reader", Scope::Read).await;
        let bearer = |token: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::AUTHORIZATION,
                format!("Bearer {token}").parse().unwrap(),
            );
            headers
        };
        assert!(
            auth.authenticate(&HeaderMap::new(), Authority::Read)
                .is_err()
        );
        assert_eq!(
            auth.authenticate(&bearer(&reader), Authority::Read)
                .unwrap()
                .role,
            Role::Read
        );
        for (authority, code) in [
            (Authority::Inference, "inference_required"),
            (Authority::Operator, "operator_required"),
        ] {
            assert_eq!(
                auth.authenticate(&bearer(&reader), authority)
                    .unwrap_err()
                    .code,
                code
            );
        }
        let admin = auth
            .authenticate(&bearer(&operator), Authority::Operator)
            .unwrap();
        assert_eq!(admin.principal, "issued-operator");
        let mut socket = HeaderMap::new();
        socket.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            format!("realtime, openai-insecure-api-key.{operator}")
                .parse()
                .unwrap(),
        );
        assert_eq!(
            auth.authenticate(&socket, Authority::Inference)
                .unwrap()
                .credential_epoch,
            admin.credential_epoch
        );
        let environment = Authorization::with_keys(
            Auth {
                api_key: Some("client".into()),
                admin_key: None,
            },
            keys,
        );
        assert!(environment.environment_operator());
        assert_eq!(
            environment
                .authenticate(&bearer("client"), Authority::Operator)
                .unwrap()
                .principal,
            "configured-api"
        );
    }

    #[test]
    fn dummy_keys_cannot_partition_anonymous_authority() {
        let auth = Authorization::new(Auth::default());
        let mut headers = HeaderMap::new();
        let empty = auth.authenticate(&headers, Authority::Operator).unwrap();
        headers.insert("x-api-key", "untrusted-identity".parse().unwrap());
        assert_eq!(
            auth.authenticate(&headers, Authority::Operator)
                .unwrap()
                .credential_epoch,
            empty.credential_epoch
        );
    }
}
