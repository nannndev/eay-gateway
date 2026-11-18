use crate::error::GatewayError;
use axum::{
    extract::{Path, State},
    Json,
};
use eay_gateway_core::engine::{build_gateway_providers, GatewayEngine};
use eay_gateway_core::models::{sync_provider_models, verify_upstream_provider};
use eay_gateway_core::providers::{find_oauth_by_id, find_provider_by_id};
use eay_gateway_core::store::GatewayStore;
use serde::Deserialize;
use serde_json::{json, Value};

pub async fn handle_list_providers(State(engine): State<GatewayEngine>) -> Json<Value> {
    Json(json!({
        "providers": build_gateway_providers(&engine.store)
    }))
}

#[derive(Deserialize)]
pub struct ConnectPayload {
    api_key: String,
    base_url: Option<String>,
}

pub async fn handle_sync_provider_models(State(engine): State<GatewayEngine>, Path(id): Path<String>) -> Result<Json<Value>, GatewayError> {
    sync_provider_models(&engine, &id).await.map_err(GatewayError::BadGateway)?;
    Ok(Json(provider_models_response(&engine.store, &id)))
}

/// Connects a provider with the key its own CLI/desktop app already saved locally.
pub async fn handle_import_local_key(State(engine): State<GatewayEngine>, Path(id): Path<String>) -> Result<Json<Value>, GatewayError> {
    let home = std::path::PathBuf::from(std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_default());
    let appdata = std::env::var("APPDATA").map(std::path::PathBuf::from).unwrap_or_else(|_| home.clone());
    // (candidate files, JSON pointer to the key, optional pointer to a base URL, login hint)
    if id == "kiro" || id == "codex" {
        let imported = if id == "kiro" {
            eay_gateway_core::oauth::import_kiro_from_ide(&engine.store)
        } else {
            eay_gateway_core::oauth::import_codex_from_cli(&engine.store)
        };
        let acc_id = imported.map_err(GatewayError::NotFound)?;
        return Ok(Json(json!({ "status": "imported", "id": acc_id })));
    }
    let (files, key_ptr, base_ptr, hint) = match id.as_str() {
        "commandcode" => (vec![home.join(".commandcode/auth.json")], "/apiKey", None, "run `cmd login` in Command Code"),
        "xiaomi-mimo" => (
            vec![
                home.join("Library/Application Support/mimocode/auth.json"),
                home.join(".local/share/mimocode/auth.json"),
                appdata.join("Xiaomi MiMo/auth.json"),
            ],
            "/xiaomi/key",
            Some("/xiaomi/metadata/base_url"),
            "sign in to MiMo Desktop",
        ),
        _ => return Err(GatewayError::BadRequest(format!("{id} has no local login to import"))),
    };
    let saved = files
        .iter()
        .find_map(|f| std::fs::read_to_string(f).ok().and_then(|raw| serde_json::from_str::<Value>(&raw).ok()))
        .ok_or_else(|| GatewayError::NotFound(format!("No saved login found; {hint} first.")))?;
    let key = saved
        .pointer(key_ptr)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .ok_or_else(|| GatewayError::BadRequest(format!("The saved login has no API key; {hint} again.")))?
        .to_string();
    let base_url = base_ptr.and_then(|p| saved.pointer(p)).and_then(|v| v.as_str()).map(String::from);
    handle_connect_provider(State(engine), Path(id), Json(ConnectPayload { api_key: key, base_url })).await
}

pub async fn handle_connect_provider(
    State(engine): State<GatewayEngine>,
    Path(id): Path<String>,
    Json(payload): Json<ConnectPayload>,
) -> Result<Json<Value>, GatewayError> {
    let prov = find_provider_by_id(&id);
    let default_base = prov.map(|p| p.base_url).unwrap_or("");
    let custom_base = payload.base_url.as_deref().filter(|s| !s.trim().is_empty());
    let base_url = custom_base.unwrap_or(default_base);
    let api_key = payload.api_key.trim();

    if prov.map(|p| p.needs_key).unwrap_or(true) && api_key.is_empty() {
        return Err(GatewayError::BadRequest(format!("API key is required for {}", prov.map(|p| p.name).unwrap_or(&id))));
    }

    if base_url.trim().is_empty() {
        return Err(GatewayError::BadRequest(match id.as_str() {
            "azure-openai" => "Azure needs a base URL like https://<resource>.openai.azure.com/openai/v1".to_string(),
            "cloudflare-ai" => {
                "Cloudflare needs a base URL like https://api.cloudflare.com/client/v4/accounts/<account-id>/ai/v1".to_string()
            }
            _ => format!("{} needs a base URL", prov.map(|p| p.name).unwrap_or(&id)),
        }));
    }

    let mut latency_ms: Option<i64> = None;
    let mut listed = Vec::new();
    if !base_url.is_empty() {
        let (ms, models) = verify_upstream_provider(&engine.http_client, &id, api_key, base_url).await.map_err(GatewayError::BadRequest)?;
        latency_ms = Some(ms);
        listed = models;
    }

    engine.store.connect_provider(&id, api_key, custom_base).map_err(GatewayError::BadRequest)?;
    let _ = engine.store.set_provider_verification(&id, latency_ms, None);
    // The key check already listed the models; Anthropic/Ollama need their own call.
    if !listed.is_empty() {
        let _ = engine.store.set_synced_models(&id, &listed);
    } else {
        let _ = sync_provider_models(&engine, &id).await;
    }

    let providers = build_gateway_providers(&engine.store);
    if let Some(p) = providers.into_iter().find(|p| p["id"] == id) {
        Ok(Json(p))
    } else {
        Ok(Json(json!({ "id": id, "connected": true, "latency_ms": latency_ms })))
    }
}

#[derive(Deserialize)]
pub struct ExtraKeyPayload {
    api_key: String,
    label: Option<String>,
}

/// Adds another key for an API-key provider. Extra keys are stored as accounts, so they get the
/// same rotation, cooldown, pause and reorder as subscription accounts.
pub async fn handle_add_provider_key(
    State(engine): State<GatewayEngine>,
    Path(id): Path<String>,
    Json(payload): Json<ExtraKeyPayload>,
) -> Result<Json<Value>, GatewayError> {
    let prov = find_provider_by_id(&id).ok_or_else(|| GatewayError::NotFound(format!("Unknown provider {id}")))?;
    let key = payload.api_key.trim();
    if key.is_empty() {
        return Err(GatewayError::BadRequest("API key is required".into()));
    }
    let base = engine.store.get_provider_record(&id).and_then(|r| r.base_url).filter(|b| !b.trim().is_empty());
    let base = base.as_deref().unwrap_or(prov.base_url);
    if base.is_empty() {
        return Err(GatewayError::BadRequest(format!("Connect {} with a base URL first", prov.name)));
    }
    verify_upstream_provider(&engine.http_client, &id, key, base).await.map_err(GatewayError::BadRequest)?;
    // Label shows which key this is without exposing it.
    let label = payload.label.filter(|l| !l.trim().is_empty()).unwrap_or_else(|| format!("Key …{}", &key[key.len().saturating_sub(4)..]));
    let acc_id = engine.store.save_account(&id, &json!({ "api_key": key }), Some(&label), None).map_err(GatewayError::Internal)?;
    Ok(Json(json!({ "status": "added", "id": acc_id })))
}

pub async fn handle_disconnect_provider(State(engine): State<GatewayEngine>, Path(id): Path<String>) -> Json<Value> {
    let _ = engine.store.disconnect_provider(&id);
    let providers = build_gateway_providers(&engine.store);
    providers.into_iter().find(|p| p["id"] == id).map(Json).unwrap_or_else(|| Json(json!({ "id": id, "connected": false })))
}

#[derive(Deserialize, Default)]
pub struct TogglePayload {
    enabled: Option<bool>,
}

pub async fn handle_toggle_provider(
    State(engine): State<GatewayEngine>,
    Path(id): Path<String>,
    payload: Option<Json<TogglePayload>>,
) -> Result<Json<Value>, GatewayError> {
    let desired = payload.and_then(|Json(p)| p.enabled);
    let enabled = engine.store.set_provider_enabled(&id, desired).map_err(GatewayError::Internal)?;
    let providers = build_gateway_providers(&engine.store);
    if let Some(p) = providers.into_iter().find(|p| p["id"] == id) {
        Ok(Json(p))
    } else {
        Ok(Json(json!({ "id": id, "enabled": enabled })))
    }
}

pub async fn handle_save_provider_key(
    State(engine): State<GatewayEngine>,
    Path(id): Path<String>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, GatewayError> {
    let key = payload.get("key").or_else(|| payload.get("api_key")).and_then(|k| k.as_str()).unwrap_or("");
    engine.store.save_provider_key(&id, key).map_err(GatewayError::BadRequest)?;
    Ok(Json(json!({ "status": "saved" })))
}

// ---- Custom Models per Provider -------------------------------------

#[derive(Deserialize)]
pub struct ModelPayload {
    model: String,
}

#[derive(Deserialize)]
pub struct ModelsListPayload {
    models: Vec<String>,
}

fn provider_models_response(store: &GatewayStore, provider_id: &str) -> Value {
    let custom = store.get_provider_custom_models(provider_id);
    let seed: Vec<String> = find_oauth_by_id(provider_id)
        .map(|op| op.seed_models.iter().map(|s| s.to_string()).collect())
        .or_else(|| find_provider_by_id(provider_id).map(|p| p.seed_models.iter().map(|s| s.to_string()).collect()))
        .unwrap_or_default();
    let seed_refs: Vec<&str> = seed.iter().map(String::as_str).collect();
    let (synced, synced_at) = store.get_synced_models(provider_id);
    json!({
        "provider": provider_id,
        "models": store.display_models(provider_id, &seed_refs),
        "custom_models": custom,
        "seed_models": seed,
        "synced_models": synced,
        "models_synced_at": synced_at,
    })
}

pub async fn handle_get_provider_models(State(engine): State<GatewayEngine>, Path(id): Path<String>) -> Json<Value> {
    Json(provider_models_response(&engine.store, &id))
}

pub async fn handle_add_provider_model(
    State(engine): State<GatewayEngine>,
    Path(id): Path<String>,
    Json(payload): Json<ModelPayload>,
) -> Result<Json<Value>, GatewayError> {
    engine.store.add_provider_model(&id, &payload.model).map_err(GatewayError::BadRequest)?;
    Ok(Json(provider_models_response(&engine.store, &id)))
}

pub async fn handle_set_provider_models(
    State(engine): State<GatewayEngine>,
    Path(id): Path<String>,
    Json(payload): Json<ModelsListPayload>,
) -> Result<Json<Value>, GatewayError> {
    engine.store.set_provider_models(&id, &payload.models).map_err(GatewayError::BadRequest)?;
    Ok(Json(provider_models_response(&engine.store, &id)))
}

pub async fn handle_delete_provider_model(
    State(engine): State<GatewayEngine>,
    Path((id, model)): Path<(String, String)>,
) -> Result<Json<Value>, GatewayError> {
    engine.store.remove_provider_model(&id, &model).map_err(GatewayError::BadRequest)?;
    Ok(Json(provider_models_response(&engine.store, &id)))
}

pub async fn handle_delete_provider_model_body(
    State(engine): State<GatewayEngine>,
    Path(id): Path<String>,
    Json(payload): Json<ModelPayload>,
) -> Result<Json<Value>, GatewayError> {
    engine.store.remove_provider_model(&id, &payload.model).map_err(GatewayError::BadRequest)?;
    Ok(Json(provider_models_response(&engine.store, &id)))
}
