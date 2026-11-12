//! Barista, the optional operator assistant in the console.
//!
//! Enablement, the `juiced-assistant` model role and the permission mode live in the native
//! configuration. The console runs the model loop, but every tool runs through [`call`], which
//! rechecks operator authority, enablement, permission and the settings revision each time.
//! Ordinary inference and the MCP gateway never reach these tools.

mod tools;

use crate::{ApiError, auth::RequestContext, extract::ApiJson, http::AppState};
use axum::{
    Extension, Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const ROLE: &str = "juiced-assistant";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    #[default]
    Full,
    Ask,
    ReadOnly,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub permission: Permission,
    #[serde(default)]
    pub revision: u64,
}

impl Settings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn validate(&self) -> Result<(), String> {
        if let Some(model) = &self.model
            && !crate::config::valid_model_id(model)
        {
            return Err("The Barista model must be a valid model ID".into());
        }
        if self.enabled && self.model.is_none() {
            return Err("Barista needs a model while enabled".into());
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub expected_revision: u64,
    pub enabled: bool,
    pub model: Option<String>,
    pub permission: Permission,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Call {
    name: String,
    #[serde(default = "empty_arguments")]
    arguments: Value,
    revision: u64,
    #[serde(default)]
    confirmed: bool,
}

fn empty_arguments() -> Value {
    json!({})
}

fn refused(status: StatusCode, code: &'static str, message: &str) -> ApiError {
    ApiError::new(status, code, message)
}

fn view(settings: &Settings, context: &RequestContext) -> Value {
    let tools = if settings.enabled {
        tools::listed(context, settings.permission)
    } else {
        Vec::new()
    };
    json!({
        "schema_version": 1,
        "role": ROLE,
        "enabled": settings.enabled,
        "model": settings.model,
        "permission": settings.permission,
        "revision": settings.revision,
        "qualified": false,
        "tools": tools,
    })
}

pub(crate) async fn settings_view(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
) -> impl IntoResponse {
    let settings = state.manager.barista_settings().await;
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(view(&settings, &context)),
    )
}

pub(crate) async fn settings_set(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    ApiJson(change): ApiJson<Change>,
) -> Result<Json<Value>, ApiError> {
    let manager = state.manager.clone();
    // A client disconnect must not separate the durable write from its in-memory publication.
    let settings = tokio::spawn(async move { manager.configure_barista(change).await })
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "barista_change_interrupted",
                "The Barista change was interrupted. Refresh before retrying.",
            )
        })??;
    Ok(Json(view(&settings, &context)))
}

pub(crate) async fn call(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<Call>,
) -> Result<Json<Value>, ApiError> {
    if !context.operator() {
        return Err(refused(
            StatusCode::FORBIDDEN,
            "operator_required",
            "Barista tools require operator access.",
        ));
    }
    let settings = state.manager.barista_settings().await;
    if !settings.enabled {
        return Err(refused(
            StatusCode::CONFLICT,
            "assistant_disabled",
            "Barista is off. Turn it on in the console to use its tools.",
        ));
    }
    if settings.revision != request.revision {
        return Err(refused(
            StatusCode::CONFLICT,
            "assistant_settings_changed",
            "Barista settings changed. Start a new request.",
        ));
    }
    let tool = tools::tool(&request.name).ok_or_else(|| {
        refused(
            StatusCode::BAD_REQUEST,
            "unknown_tool",
            "Barista has no tool with this name.",
        )
    })?;
    if !tool.read_only {
        match settings.permission {
            Permission::ReadOnly => {
                return Err(refused(
                    StatusCode::FORBIDDEN,
                    "assistant_read_only",
                    "Barista is read only. It cannot make changes.",
                ));
            }
            Permission::Ask if !request.confirmed => {
                return Err(refused(
                    StatusCode::FORBIDDEN,
                    "assistant_confirmation_required",
                    "The operator must approve this change first.",
                ));
            }
            Permission::Ask | Permission::Full => {}
        }
    }
    let app = state.probe.get().cloned().ok_or_else(|| {
        refused(
            StatusCode::SERVICE_UNAVAILABLE,
            "assistant_not_ready",
            "Barista tools are not ready yet. Retry.",
        )
    })?;
    match tools::call(app, &headers, &context, tool, request.arguments).await {
        Ok(result) => Ok(Json(json!({"ok": true, "result": result}))),
        Err(tools::Failure::Forbidden) => Err(refused(
            StatusCode::FORBIDDEN,
            "operator_required",
            "Barista tools require operator access.",
        )),
        Err(tools::Failure::InvalidParams(message)) => Ok(Json(json!({
            "ok": false,
            "result": {"error": {"code": "invalid_arguments", "message": message, "status": 400}},
        }))),
        Err(tools::Failure::Route(failure)) => Ok(Json(json!({"ok": false, "result": failure}))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_default_off_with_full_permission_and_round_trip() {
        let missing: Settings = serde_json::from_value(json!({})).unwrap();
        assert_eq!(missing, Settings::default());
        assert!(!missing.enabled);
        assert_eq!(missing.permission, Permission::Full);
        assert!(missing.is_default());
        let saved =
            json!({"enabled":true,"model":"folder/chat","permission":"read_only","revision":3});
        let settings: Settings = serde_json::from_value(saved.clone()).unwrap();
        assert_eq!(serde_json::to_value(&settings).unwrap(), saved);
        assert!(settings.validate().is_ok());
        assert!(serde_json::from_value::<Settings>(json!({"permission":"admin"})).is_err());
        assert!(serde_json::from_value::<Settings>(json!({"extra":true})).is_err());
        let orphan = Settings {
            enabled: true,
            model: None,
            ..Settings::default()
        };
        assert!(orphan.validate().is_err());
    }
}
