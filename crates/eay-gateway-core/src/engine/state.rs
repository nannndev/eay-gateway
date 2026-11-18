use crate::providers::{find_oauth_by_id, OAUTH_PROVIDERS, PROVIDERS};
use crate::store::{GatewayStore, ProviderRecord};
use serde_json::{json, Value};

pub fn get_oauth_accounts_state(store: &GatewayStore) -> Value {
    let accounts = store.list_accounts();
    let client_accounts: Vec<Value> = accounts
        .into_iter()
        // Extra API keys are stored as accounts but belong to their provider, not this list.
        .filter(|a| find_oauth_by_id(&a.provider).is_some())
        .map(|a| {
            let expires_at = a.cred.get("expires_at").and_then(|v| v.as_f64());
            let plan = a.display_plan();
            let is_free = a.is_free();
            json!({
                "id": a.id,
                "provider": a.provider,
                "label": a.label,
                "email": a.email,
                "enabled": a.enabled,
                "connected_at": a.connected_at,
                "refreshed_at": a.refreshed_at,
                "expires_at": expires_at,
                "last_error": a.last_error,
                "plan": plan,
                "is_free": is_free,
                "cooling_until": store.cooling_until(&a.id),
            })
        })
        .collect();

    let mut models_map = serde_json::Map::new();
    let mut aliases_map = serde_json::Map::new();
    let mut enabled_map = serde_json::Map::new();
    for op in OAUTH_PROVIDERS {
        models_map.insert(op.id.to_string(), json!(store.display_models(op.id, op.seed_models)));
        aliases_map.insert(op.id.to_string(), json!(op.alias));
        enabled_map.insert(op.id.to_string(), json!(store.is_provider_enabled(op.id)));
    }

    let settings = store.get_gateway_settings();

    json!({
        "accounts": client_accounts,
        "models": models_map,
        "aliases": aliases_map,
        "provider_enabled": enabled_map,
        "providers": OAUTH_PROVIDERS,
        "settings": settings,
    })
}

pub fn build_gateway_providers(store: &GatewayStore) -> Vec<Value> {
    let stored_records = store.list_providers();
    let record_map: std::collections::HashMap<String, ProviderRecord> = stored_records.into_iter().map(|r| (r.id.clone(), r)).collect();

    let accounts = store.list_accounts();
    let mut out = Vec::new();
    for p in PROVIDERS {
        let rec = record_map.get(p.id);
        let enabled = rec.map(|r| r.enabled).unwrap_or(true);
        let extra_keys: Vec<Value> = accounts
            .iter()
            .filter(|a| a.provider == p.id)
            .map(|a| {
                json!({
                    "id": a.id, "label": a.label, "enabled": a.enabled, "last_error": a.last_error,
                    "cooling_until": store.cooling_until(&a.id),
                })
            })
            .collect();
        let connected = rec.map(|r| r.has_key).unwrap_or(false) || !p.needs_key || !extra_keys.is_empty();
        let base_url = rec.and_then(|r| r.base_url.clone());
        let verified_at = rec.and_then(|r| r.verified_at);
        let latency_ms = rec.and_then(|r| r.latency_ms);
        let error = rec.and_then(|r| r.error.clone());
        let custom_models: Vec<String> = rec.map(|r| r.models.clone()).unwrap_or_default();
        let models = store.display_models(p.id, p.seed_models);
        let (synced, synced_at) = store.get_synced_models(p.id);

        out.push(json!({
            "id": p.id,
            "alias": p.alias,
            "name": p.name,
            "family": p.family,
            "default_base_url": p.base_url,
            "needs_key": p.needs_key,
            "enabled": enabled,
            "connected": connected,
            "extra_keys": extra_keys,
            "base_url": base_url,
            "api_key": if connected && p.needs_key { Some("••••••••") } else { None },
            "models": models,
            "custom_models": custom_models,
            "seed_models": p.seed_models,
            "synced_count": synced.len(),
            "models_synced_at": synced_at,
            "verified_at": verified_at,
            "latency_ms": latency_ms,
            "cooling": store.cooling_until(&format!("apikey:{}", p.id)).is_some(),
            "error": error,
        }));
    }
    out
}
