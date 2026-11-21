//! Projects: an app's container for keys, teams, sessions and usage.

use crate::error::GatewayError;
use axum::{
    extract::{Path, Query, State},
    Json,
};
use eay_gateway_core::engine::GatewayEngine;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct SaveProjectPayload {
    name: String,
    #[serde(default)]
    instructions: String,
    /// Anything the app wants to keep with the project (a repo path, a customer id…). A JSON object.
    #[serde(default)]
    metadata: Value,
    /// Absolute folder the project works in; its requests get file tools confined to it.
    #[serde(default)]
    root: Option<String>,
    /// Create `root` if it does not exist yet (a new project) instead of requiring an existing folder.
    #[serde(default)]
    create: bool,
}

impl SaveProjectPayload {
    fn root(&self) -> Result<Option<String>, GatewayError> {
        let root = self.root.as_deref().map(str::trim).filter(|r| !r.is_empty());
        root.map(|r| eay_gateway_core::workspace::prepare_root(r, self.create)).transpose().map_err(GatewayError::BadRequest)
    }
}

pub async fn handle_list_projects(State(engine): State<GatewayEngine>) -> Json<Value> {
    Json(json!({ "projects": engine.store.list_projects() }))
}

pub async fn handle_create_project(
    State(engine): State<GatewayEngine>,
    Json(p): Json<SaveProjectPayload>,
) -> Result<Json<Value>, GatewayError> {
    let root = p.root()?;
    let project =
        engine.store.save_project(None, &p.name, &p.instructions, &p.metadata, root.as_deref()).map_err(GatewayError::BadRequest)?;
    Ok(Json(json!({ "project": project })))
}

pub async fn handle_put_project(
    State(engine): State<GatewayEngine>,
    Path(id): Path<String>,
    Json(p): Json<SaveProjectPayload>,
) -> Result<Json<Value>, GatewayError> {
    if engine.store.get_project(&id).is_none() {
        return Err(GatewayError::NotFound(format!("Project '{id}' not found")));
    }
    let root = p.root()?;
    let project =
        engine.store.save_project(Some(&id), &p.name, &p.instructions, &p.metadata, root.as_deref()).map_err(GatewayError::BadRequest)?;
    Ok(Json(json!({ "project": project })))
}

/// The project, plus what belongs to it: keys (prefix only), teams and sessions.
pub async fn handle_get_project(State(engine): State<GatewayEngine>, Path(id): Path<String>) -> Result<Json<Value>, GatewayError> {
    let project = engine.store.get_project(&id).ok_or_else(|| GatewayError::NotFound(format!("Project '{id}' not found")))?;
    let keys: Vec<Value> = engine
        .store
        .list_virtual_keys()
        .into_iter()
        .filter(|k| k.project.as_deref() == Some(id.as_str()))
        .map(|k| json!({ "id": k.id, "name": k.name, "key_prefix": k.key_prefix, "enabled": k.enabled }))
        .collect();
    let teams: Vec<_> = engine.store.list_teams().into_iter().filter(|t| t.project.as_deref() == Some(id.as_str())).collect();
    let sessions: Vec<Value> =
        engine.store.list_sessions(1000).into_iter().filter(|s| s["project"].as_str() == Some(id.as_str())).collect();
    Ok(Json(json!({ "project": project, "keys": keys, "teams": teams, "sessions": sessions })))
}

/// Deleting a project keeps its keys, teams, sessions and usage; they just stop belonging to it.
pub async fn handle_delete_project(State(engine): State<GatewayEngine>, Path(id): Path<String>) -> Result<Json<Value>, GatewayError> {
    let deleted = engine.store.delete_project(&id).map_err(GatewayError::Internal)?;
    Ok(Json(json!({ "status": if deleted { "deleted" } else { "not_found" } })))
}

#[derive(Deserialize)]
pub struct DirsQuery {
    path: Option<String>,
}

/// `GET /gateway/fs/dirs?path=` lists subfolders so a browser UI can pick a project folder.
pub async fn handle_list_dirs(Query(q): Query<DirsQuery>) -> Result<Json<Value>, GatewayError> {
    eay_gateway_core::workspace::list_dirs(q.path.as_deref()).map(Json).map_err(GatewayError::BadRequest)
}

/// `GET /gateway/projects/:id/files?path=` lists a folder in the project, or returns a file's text.
pub async fn handle_project_files(
    State(engine): State<GatewayEngine>,
    Path(id): Path<String>,
    Query(q): Query<DirsQuery>,
) -> Result<Json<Value>, GatewayError> {
    let project = engine.store.get_project(&id).ok_or_else(|| GatewayError::NotFound(format!("Project '{id}' not found")))?;
    let root = project.root.ok_or_else(|| GatewayError::BadRequest("This project has no folder".into()))?;
    let rel = q.path.unwrap_or_default();
    tokio::task::spawn_blocking(move || eay_gateway_core::workspace::browse(&root, &rel))
        .await
        .map_err(|e| GatewayError::Internal(e.to_string()))?
        .map(Json)
        .map_err(GatewayError::BadRequest)
}

#[derive(Deserialize)]
pub struct UsageQuery {
    /// Days of history; omit for all time.
    days: Option<u32>,
}

pub async fn handle_project_usage(
    State(engine): State<GatewayEngine>,
    Path(id): Path<String>,
    Query(q): Query<UsageQuery>,
) -> Result<Json<Value>, GatewayError> {
    if engine.store.get_project(&id).is_none() {
        return Err(GatewayError::NotFound(format!("Project '{id}' not found")));
    }
    let since = q.days.map_or(0.0, |d| eay_gateway_core::store::now() - f64::from(d.max(1)) * 86_400.0);
    Ok(Json(engine.store.project_usage(&id, since)))
}

#[derive(Deserialize)]
pub struct LinkPayload {
    /// `prj_…`, or null to remove the link.
    project: Option<String>,
}

/// `PUT /gateway/keys/:id/project` and `PUT /gateway/teams/:id/project`.
pub async fn handle_link_key(
    State(engine): State<GatewayEngine>,
    Path(id): Path<String>,
    Json(p): Json<LinkPayload>,
) -> Result<Json<Value>, GatewayError> {
    link(&engine, "keys", &id, p.project.as_deref())
}

pub async fn handle_link_team(
    State(engine): State<GatewayEngine>,
    Path(id): Path<String>,
    Json(p): Json<LinkPayload>,
) -> Result<Json<Value>, GatewayError> {
    link(&engine, "teams", &id, p.project.as_deref())
}

fn link(engine: &GatewayEngine, kind: &str, id: &str, project: Option<&str>) -> Result<Json<Value>, GatewayError> {
    let project = project.map(str::trim).filter(|p| !p.is_empty());
    engine.store.set_project_link(kind, id, project).map_err(|e| {
        if e.contains("not found") || e.starts_with("No ") {
            GatewayError::NotFound(e)
        } else {
            GatewayError::BadRequest(e)
        }
    })?;
    Ok(Json(json!({ "id": id, "project": project })))
}
