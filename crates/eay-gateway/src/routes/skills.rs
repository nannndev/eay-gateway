use crate::error::GatewayError;
use axum::{
    extract::{Path, State},
    Json,
};
use eay_gateway_core::engine::GatewayEngine;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct SaveSkillPayload {
    name: Option<String>,
    #[serde(default)]
    description: String,
    prompt: String,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_true() -> bool {
    true
}

pub async fn handle_list_skills(State(engine): State<GatewayEngine>) -> Json<Value> {
    Json(json!({ "skills": engine.store.list_skills() }))
}

pub async fn handle_save_skill(
    State(engine): State<GatewayEngine>,
    Json(payload): Json<SaveSkillPayload>,
) -> Result<Json<Value>, GatewayError> {
    let name = payload.name.as_deref().unwrap_or("");
    let skill = engine.store.save_skill(name, &payload.description, &payload.prompt, payload.enabled).map_err(GatewayError::BadRequest)?;
    Ok(Json(json!({ "skill": skill })))
}

pub async fn handle_put_skill(
    State(engine): State<GatewayEngine>,
    Path(name): Path<String>,
    Json(payload): Json<SaveSkillPayload>,
) -> Result<Json<Value>, GatewayError> {
    let skill = engine.store.save_skill(&name, &payload.description, &payload.prompt, payload.enabled).map_err(GatewayError::BadRequest)?;
    Ok(Json(json!({ "skill": skill })))
}

pub async fn handle_toggle_skill(State(engine): State<GatewayEngine>, Path(name): Path<String>) -> Result<Json<Value>, GatewayError> {
    let skill = engine.store.toggle_skill(&name).map_err(GatewayError::NotFound)?;
    Ok(Json(json!({ "skill": skill })))
}

pub async fn handle_delete_skill(State(engine): State<GatewayEngine>, Path(name): Path<String>) -> Json<Value> {
    let deleted = engine.store.delete_skill(&name).unwrap_or(false);
    Json(json!({ "status": if deleted { "deleted" } else { "not_found" } }))
}
