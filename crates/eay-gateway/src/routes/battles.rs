use crate::error::GatewayError;
use axum::{
    extract::{Path, Query, State},
    Json,
};
use eay_gateway_core::engine::GatewayEngine;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct ListLimitQuery {
    limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct SaveBattlePayload {
    id: Option<String>,
    #[serde(default = "default_mode")]
    mode: String,
    category: Option<String>,
    #[serde(default)]
    data: Value,
}

fn default_mode() -> String {
    "blind".into()
}

#[derive(Deserialize)]
pub struct RecordEloMatchPayload {
    fighter_a: String,
    fighter_b: String,
    outcome: String,
}

#[derive(Deserialize)]
pub struct SaveBenchmarkPayload {
    id: Option<String>,
    suite_id: String,
    name: String,
    #[serde(default)]
    data: Value,
}

pub async fn handle_list_battles(
    State(engine): State<GatewayEngine>,
    Query(q): Query<ListLimitQuery>,
) -> Json<Value> {
    Json(json!({ "battles": engine.store.list_battles(q.limit.unwrap_or(50)) }))
}

pub async fn handle_save_battle(
    State(engine): State<GatewayEngine>,
    Json(p): Json<SaveBattlePayload>,
) -> Result<Json<Value>, GatewayError> {
    let battle = engine
        .store
        .save_battle(p.id.as_deref(), &p.mode, p.category.as_deref(), &p.data)
        .map_err(GatewayError::BadRequest)?;
    Ok(Json(json!({ "battle": battle })))
}

pub async fn handle_delete_battle(
    State(engine): State<GatewayEngine>,
    Path(id): Path<String>,
) -> Json<Value> {
    let deleted = engine.store.delete_battle(&id).unwrap_or(false);
    Json(json!({ "status": if deleted { "deleted" } else { "not_found" } }))
}

pub async fn handle_list_elo(State(engine): State<GatewayEngine>) -> Json<Value> {
    Json(json!({ "ratings": engine.store.list_elo_ratings() }))
}

pub async fn handle_record_elo_match(
    State(engine): State<GatewayEngine>,
    Json(p): Json<RecordEloMatchPayload>,
) -> Result<Json<Value>, GatewayError> {
    let (a, b) = engine
        .store
        .record_elo_match(&p.fighter_a, &p.fighter_b, &p.outcome)
        .map_err(GatewayError::BadRequest)?;
    Ok(Json(json!({ "fighter_a": a, "fighter_b": b })))
}

pub async fn handle_reset_elo(State(engine): State<GatewayEngine>) -> Result<Json<Value>, GatewayError> {
    engine.store.reset_elo_ratings().map_err(GatewayError::Internal)?;
    Ok(Json(json!({ "status": "reset" })))
}

pub async fn handle_list_benchmarks(
    State(engine): State<GatewayEngine>,
    Query(q): Query<ListLimitQuery>,
) -> Json<Value> {
    Json(json!({ "benchmarks": engine.store.list_benchmarks(q.limit.unwrap_or(50)) }))
}

pub async fn handle_save_benchmark(
    State(engine): State<GatewayEngine>,
    Json(p): Json<SaveBenchmarkPayload>,
) -> Result<Json<Value>, GatewayError> {
    let benchmark = engine
        .store
        .save_benchmark(p.id.as_deref(), &p.suite_id, &p.name, &p.data)
        .map_err(GatewayError::BadRequest)?;
    Ok(Json(json!({ "benchmark": benchmark })))
}

pub async fn handle_delete_benchmark(
    State(engine): State<GatewayEngine>,
    Path(id): Path<String>,
) -> Json<Value> {
    let deleted = engine.store.delete_benchmark(&id).unwrap_or(false);
    Json(json!({ "status": if deleted { "deleted" } else { "not_found" } }))
}
