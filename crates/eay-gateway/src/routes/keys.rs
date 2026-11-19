use crate::error::GatewayError;
use axum::{
    extract::{Path, State},
    Json,
};
use eay_gateway_core::engine::GatewayEngine;
use serde::Deserialize;
use serde_json::{json, Value};

pub async fn handle_list_keys(State(engine): State<GatewayEngine>) -> Json<Value> {
    Json(json!({ "keys": engine.store.list_virtual_keys() }))
}

#[derive(Deserialize)]
pub struct CreateKeyPayload {
    name: String,
    allowed_providers: Option<Vec<String>>,
    rate_limit_per_min: Option<i64>,
    max_tokens_budget: Option<i64>,
    max_cost_usd: Option<f64>,
    default_skills: Option<Vec<String>>,
    max_concurrent: Option<i64>,
    #[serde(default)]
    tool_policy: eay_gateway_core::mcp::ToolPolicy,
}

pub async fn handle_create_key(
    State(engine): State<GatewayEngine>,
    Json(payload): Json<CreateKeyPayload>,
) -> Result<Json<Value>, GatewayError> {
    // Validate before creating anything, so a bad policy does not leave a key behind.
    payload.tool_policy.validate().map_err(GatewayError::BadRequest)?;
    let (mut record, secret) = engine
        .store
        .create_virtual_key(&payload.name, payload.allowed_providers, payload.rate_limit_per_min)
        .map_err(GatewayError::Internal)?;
    if payload.max_tokens_budget.is_some() || payload.max_cost_usd.is_some() {
        engine.store.set_virtual_key_limits(&record.id, payload.max_tokens_budget, payload.max_cost_usd).map_err(GatewayError::Internal)?;
        record.max_tokens_budget = payload.max_tokens_budget;
        record.max_cost_usd = payload.max_cost_usd;
    }
    if let Some(n) = payload.max_concurrent.filter(|n| *n > 0) {
        engine.store.set_virtual_key_concurrency(&record.id, Some(n)).map_err(GatewayError::Internal)?;
        record.max_concurrent = Some(n);
    }
    if !payload.tool_policy.is_empty() {
        engine.store.set_virtual_key_tool_policy(&record.id, &payload.tool_policy).map_err(GatewayError::Internal)?;
        record.tool_policy = payload.tool_policy.clone();
    }
    if let Some(skills) = payload.default_skills.filter(|s| !s.is_empty()) {
        engine.store.set_virtual_key_skills(&record.id, Some(skills.clone())).map_err(GatewayError::Internal)?;
        record.default_skills = Some(skills);
    }
    let mut val = serde_json::to_value(&record).map_err(|e| GatewayError::Internal(e.to_string()))?;
    val["key"] = json!(secret);
    Ok(Json(json!({ "key": val })))
}

pub async fn handle_toggle_key(State(engine): State<GatewayEngine>, Path(id): Path<String>) -> Result<Json<Value>, GatewayError> {
    let record = engine.store.toggle_virtual_key(&id).map_err(GatewayError::NotFound)?;
    Ok(Json(json!({ "key": record })))
}

pub async fn handle_delete_key(State(engine): State<GatewayEngine>, Path(id): Path<String>) -> Json<Value> {
    let _ = engine.store.delete_virtual_key(&id);
    Json(json!({ "status": "deleted" }))
}
