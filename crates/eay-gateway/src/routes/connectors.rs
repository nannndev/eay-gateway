use crate::error::GatewayError;
use axum::{
    extract::{Path, State},
    Json,
};
use eay_gateway_core::engine::GatewayEngine;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct SaveMcpPayload {
    name: Option<String>,
    #[serde(default = "default_stdio")]
    transport: String,
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: std::collections::HashMap<String, String>,
    url: Option<String>,
    #[serde(default)]
    headers: std::collections::HashMap<String, String>,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_stdio() -> String {
    "stdio".into()
}

pub async fn handle_list_mcp(State(engine): State<GatewayEngine>) -> Json<Value> {
    Json(json!({ "mcp_servers": engine.store.list_mcp_servers() }))
}

pub async fn handle_save_mcp(State(engine): State<GatewayEngine>, Json(p): Json<SaveMcpPayload>) -> Result<Json<Value>, GatewayError> {
    let name = p.name.as_deref().unwrap_or("");
    let rec = engine
        .store
        .save_mcp_server(name, &p.transport, p.command.as_deref(), &p.args, &p.env, p.url.as_deref(), &p.headers, p.enabled)
        .map_err(GatewayError::BadRequest)?;
    Ok(Json(json!({ "mcp_server": rec })))
}

pub async fn handle_toggle_mcp(State(engine): State<GatewayEngine>, Path(name): Path<String>) -> Result<Json<Value>, GatewayError> {
    let rec = engine.store.toggle_mcp_server(&name).map_err(GatewayError::NotFound)?;
    eay_gateway_core::mcp::shutdown_server(&name);
    Ok(Json(json!({ "mcp_server": rec })))
}

pub async fn handle_delete_mcp(State(engine): State<GatewayEngine>, Path(name): Path<String>) -> Json<Value> {
    let deleted = engine.store.delete_mcp_server(&name).unwrap_or(false);
    eay_gateway_core::mcp::shutdown_server(&name);
    Json(json!({ "status": if deleted { "deleted" } else { "not_found" } }))
}

/// Bulk-adds servers from a pasted Claude Desktop / Cursor / Claude Code / VS Code MCP config.
pub async fn handle_import_mcp(State(engine): State<GatewayEngine>, Json(cfg): Json<Value>) -> Result<Json<Value>, GatewayError> {
    let parsed = eay_gateway_core::mcp::parse_client_config(&cfg);
    if parsed.is_empty() {
        return Err(GatewayError::BadRequest("No MCP servers found; paste a JSON object with an \"mcpServers\" map".into()));
    }
    let (mut imported, mut errors) = (Vec::new(), Vec::new());
    for m in parsed {
        match engine.store.save_mcp_server(&m.name, &m.transport, m.command.as_deref(), &m.args, &m.env, m.url.as_deref(), &m.headers, true)
        {
            Ok(rec) => imported.push(rec.name),
            Err(e) => errors.push(json!({ "name": m.name, "error": e })),
        }
    }
    Ok(Json(json!({ "imported": imported, "errors": errors })))
}

#[derive(Deserialize)]
pub struct CallMcpPayload {
    tool: String,
    #[serde(default)]
    arguments: Value,
}

/// Runs one tool directly, so a connector can be tried without a model in the loop.
pub async fn handle_mcp_call(
    State(engine): State<GatewayEngine>,
    Path(name): Path<String>,
    Json(p): Json<CallMcpPayload>,
) -> Result<Json<Value>, GatewayError> {
    let srv = engine.store.get_mcp_server(&name).ok_or_else(|| GatewayError::NotFound("MCP server not found".into()))?;
    let args = if p.arguments.is_null() { json!({}) } else { p.arguments };
    let t = std::time::Instant::now();
    let res = eay_gateway_core::mcp::call_tool(&engine.http_client, &srv, &p.tool, args).await;
    let ms = t.elapsed().as_millis() as u64;
    Ok(Json(match res {
        Ok(out) => json!({ "ok": true, "output": out, "duration_ms": ms }),
        Err(e) => json!({ "ok": false, "output": e, "duration_ms": ms }),
    }))
}

pub async fn handle_mcp_tools(State(engine): State<GatewayEngine>, Path(name): Path<String>) -> Result<Json<Value>, GatewayError> {
    let srv = engine.store.get_mcp_server(&name).ok_or_else(|| GatewayError::NotFound("MCP server not found".into()))?;
    let tools = eay_gateway_core::mcp::list_tools_openai(&engine.http_client, &srv).await.map_err(GatewayError::BadGateway)?;
    Ok(Json(json!({ "tools": tools })))
}

fn default_true() -> bool {
    true
}
