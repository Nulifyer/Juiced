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
    Inference,
    Operator,
    TrustedLan,
}

#[derive(Clone, Debug, Serialize)]
pub struct RequestContext {
    pub principal: &'static str,
    pub role: Role,
    pub credential_epoch: Arc<str>,
}

impl RequestContext {
    pub fn operator(&self) -> bool {
        self.role != Role::Inference
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
}

impl Authorization {
    pub fn new(auth: Auth) -> Self {
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
        }
    }

    pub fn authenticate(
        &self,
        headers: &HeaderMap,
        requires_operator: bool,
    ) -> Result<RequestContext, ApiError> {
        if self.regular.is_none() && self.operator.is_none() {
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
        let credential = self
            .operator
            .iter()
            .chain(self.regular.iter())
            .find(|key| bool::from(token.as_bytes().ct_eq(key.token.as_bytes())))
            .ok_or_else(unauthorized)?;
        if requires_operator && !credential.context.operator() {
            return Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "operator_required",
                "An operator API key is required.",
            ));
        }
        Ok(credential.context.clone())
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
        assert!(auth.authenticate(&headers, false).is_ok());
        headers.insert(header::AUTHORIZATION, "Bearer wrong".parse().unwrap());
        assert!(auth.authenticate(&headers, false).is_err());
        let mut twice = HeaderMap::new();
        twice.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            "openai-insecure-api-key.client, openai-insecure-api-key.client"
                .parse()
                .unwrap(),
        );
        assert!(auth.authenticate(&twice, false).is_err());
    }

    #[test]
    fn normalized_transports_share_context_but_roles_and_rotation_do_not() {
        let auth = Authorization::new(Auth {
            api_key: Some("client".into()),
            admin_key: Some("operator".into()),
        });
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer client".parse().unwrap());
        let bearer = auth.authenticate(&headers, false).unwrap();
        headers.clear();
        headers.insert("x-api-key", "client".parse().unwrap());
        let other = auth.authenticate(&headers, false).unwrap();
        assert_eq!(bearer.credential_epoch, other.credential_epoch);
        assert_eq!(other.role, Role::Inference);
        assert!(auth.authenticate(&headers, true).is_err());
        headers.insert(header::AUTHORIZATION, "Bearer operator".parse().unwrap());
        assert!(auth.authenticate(&headers, false).is_err());
        headers.remove("x-api-key");
        let admin = auth.authenticate(&headers, true).unwrap();
        assert_ne!(admin.credential_epoch, bearer.credential_epoch);
        let rotated = Authorization::new(Auth {
            api_key: Some("rotated".into()),
            admin_key: Some("operator".into()),
        });
        headers.insert(header::AUTHORIZATION, "Bearer rotated".parse().unwrap());
        assert_ne!(
            rotated
                .authenticate(&headers, false)
                .unwrap()
                .credential_epoch,
            bearer.credential_epoch
        );
    }

    #[test]
    fn dummy_keys_cannot_partition_anonymous_authority() {
        let auth = Authorization::new(Auth::default());
        let mut headers = HeaderMap::new();
        let empty = auth.authenticate(&headers, true).unwrap();
        headers.insert("x-api-key", "untrusted-identity".parse().unwrap());
        assert_eq!(
            auth.authenticate(&headers, true).unwrap().credential_epoch,
            empty.credential_epoch
        );
    }
}
