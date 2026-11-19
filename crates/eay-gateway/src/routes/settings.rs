use crate::error::GatewayError;
use axum::{
    extract::{Path, Query, State},
    Json,
};
use eay_gateway_core::engine::GatewayEngine;
use eay_gateway_core::providers::{OAUTH_PROVIDERS, PROVIDERS};
use serde::Deserialize;
use serde_json::{json, Value};

pub async fn handle_admin_models(State(engine): State<GatewayEngine>) -> Json<Value> {
    let mut models = Vec::new();
    for op in OAUTH_PROVIDERS {
        let enabled = engine.store.is_provider_enabled(op.id);
        let custom = engine.store.get_provider_custom_models(op.id);
        for m in engine.store.display_models(op.id, op.seed_models) {
            let is_custom = custom.iter().any(|c| c == &m);
            models.push(json!({
                "id": format!("{}/{}", op.alias, m),
                "model": m,
                "owned_by": op.id,
                "enabled": enabled,
                "custom": is_custom,
            }));
        }
    }
    for p in PROVIDERS {
        let enabled = engine.store.is_provider_enabled(p.id);
        let custom = engine.store.get_provider_custom_models(p.id);
        for m in engine.store.display_models(p.id, p.seed_models) {
            let is_custom = custom.iter().any(|c| c == &m);
            models.push(json!({
                "id": format!("{}/{}", p.alias, m),
                "model": m,
                "owned_by": p.id,
                "enabled": enabled,
                "custom": is_custom,
            }));
        }
    }
    Json(json!({ "models": models }))
}

pub async fn handle_list_combos(State(engine): State<GatewayEngine>) -> Json<Value> {
    Json(json!({ "combos": engine.store.list_combos() }))
}

pub async fn handle_save_combo(State(engine): State<GatewayEngine>, Json(payload): Json<Value>) -> Result<Json<Value>, GatewayError> {
    let name = payload.get("name").and_then(|n| n.as_str()).unwrap_or("").trim();
    let targets: Vec<String> = payload.get("targets").and_then(|t| serde_json::from_value(t.clone()).ok()).unwrap_or_default();
    // Combos are addressed as `auto/<name>`, so the name must be one path segment.
    if name.is_empty() || name.contains('/') || name.contains(char::is_whitespace) {
        return Err(GatewayError::BadRequest("Combo name must be one word without '/' or spaces".into()));
    }
    if targets.iter().all(|t| t.trim().is_empty()) {
        return Err(GatewayError::BadRequest("Combo needs at least one model".into()));
    }
    engine.store.save_combo(name, &targets).map_err(GatewayError::BadRequest)?;
    Ok(Json(json!({ "combos": engine.store.list_combos() })))
}

pub async fn handle_put_combo(
    State(engine): State<GatewayEngine>,
    Path(name): Path<String>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, GatewayError> {
    let targets: Vec<String> = payload.get("targets").and_then(|t| serde_json::from_value(t.clone()).ok()).unwrap_or_default();
    engine.store.save_combo(&name, &targets).map_err(GatewayError::BadRequest)?;
    Ok(Json(json!({ "combos": engine.store.list_combos() })))
}

pub async fn handle_delete_combo(State(engine): State<GatewayEngine>, Path(name): Path<String>) -> Json<Value> {
    let _ = engine.store.delete_combo(&name);
    Json(json!({ "combos": engine.store.list_combos() }))
}

// ---- Usage, Stats & Settings ----------------------------------------

#[derive(Deserialize)]
pub struct UsageQuery {
    limit: Option<i64>,
}

pub async fn handle_list_usage(State(engine): State<GatewayEngine>, Query(query): Query<UsageQuery>) -> Json<Value> {
    let limit = query.limit.unwrap_or(100);
    Json(json!({ "usage": engine.store.list_usage(limit) }))
}

pub async fn handle_clear_usage(State(engine): State<GatewayEngine>) -> Json<Value> {
    let _ = engine.store.clear_usage();
    Json(json!({ "status": "cleared" }))
}

pub async fn handle_get_stats(State(engine): State<GatewayEngine>) -> Json<Value> {
    Json(engine.store.get_stats())
}

#[derive(Deserialize)]
pub struct AnalyticsQuery {
    /// Days of history; omit for all.
    days: Option<u32>,
    /// Caller's offset from UTC in minutes (east positive).
    tz_offset_min: Option<i64>,
}

pub async fn handle_analytics(State(engine): State<GatewayEngine>, Query(q): Query<AnalyticsQuery>) -> Json<Value> {
    Json(engine.store.analytics(q.days, q.tz_offset_min.unwrap_or(0) * 60))
}

pub async fn handle_get_settings(State(engine): State<GatewayEngine>) -> Json<Value> {
    Json(settings_response(&engine))
}

/// Settings plus the effective `auto` provider order, so the UI can show and reorder it.
fn settings_response(engine: &GatewayEngine) -> Value {
    let s = engine.store.get_gateway_settings();
    let order = engine.auto_provider_order(&s);
    json!({ "settings": s, "auto_order": order })
}

#[derive(Deserialize)]
pub struct UpdateSettingsPayload {
    routing_strategy: Option<String>,
    fallback_order: Option<String>,
    provider_priority: Option<Vec<String>>,
    auto_models_per_provider: Option<u32>,
    cooldown_secs: Option<u32>,
    gemini_oauth_client_id: Option<String>,
    gemini_oauth_client_secret: Option<String>,
    clear_gemini_secret: Option<bool>,
}

pub async fn handle_update_settings(
    State(engine): State<GatewayEngine>,
    Json(payload): Json<UpdateSettingsPayload>,
) -> Result<Json<Value>, GatewayError> {
    if let Some(strategy) = payload.routing_strategy {
        if !["priority", "round_robin", "fill_first"].contains(&strategy.as_str()) {
            return Err(GatewayError::BadRequest(format!("unknown routing_strategy: {strategy}")));
        }
        let _ = engine.store.set_setting("routing_strategy", &strategy);
    }
    if let Some(order) = payload.fallback_order {
        if !["model_first", "account_first"].contains(&order.as_str()) {
            return Err(GatewayError::BadRequest(format!("unknown fallback_order: {order}")));
        }
        let _ = engine.store.set_setting("fallback_order", &order);
    }
    if let Some(list) = payload.provider_priority {
        let _ = engine.store.set_setting("provider_priority", &json!(list).to_string());
    }
    if let Some(n) = payload.auto_models_per_provider {
        let _ = engine.store.set_setting("auto_models_per_provider", &n.clamp(1, 10).to_string());
    }
    if let Some(n) = payload.cooldown_secs {
        let _ = engine.store.set_setting("cooldown_secs", &n.min(3600).to_string());
    }
    if let Some(client_id) = payload.gemini_oauth_client_id {
        let _ = engine.store.set_setting("gemini_oauth_client_id", &client_id);
    }
    if payload.clear_gemini_secret == Some(true) {
        let _ = engine.store.set_setting("gemini_oauth_client_secret", "");
    } else if let Some(secret) = payload.gemini_oauth_client_secret {
        if !secret.trim().is_empty() {
            let _ = engine.store.set_setting("gemini_oauth_client_secret", &secret);
        }
    }
    Ok(Json(settings_response(&engine)))
}
