//! Provider model lists and key checks: verify a key, list a provider's models, keep lists fresh.

use crate::engine::GatewayEngine;
use crate::providers::{find_oauth_by_id, find_provider_by_id};
use serde_json::{json, Value};

/// Checks the key and returns (latency, model ids if the provider listed them).
pub async fn verify_upstream_provider(
    client: &reqwest::Client,
    provider_id: &str,
    api_key: &str,
    base_url: &str,
) -> Result<(i64, Vec<String>), String> {
    let t0 = std::time::Instant::now();
    let trimmed_key = api_key.trim();
    let trimmed_base = base_url.trim_end_matches('/');

    let res = if provider_id == "commandcode" {
        client
            .get(format!("{trimmed_base}/alpha/whoami?limits=1"))
            .bearer_auth(trimmed_key)
            .timeout(std::time::Duration::from_secs(8))
            .send()
            .await
    } else if provider_id == "ollama" {
        let url = format!("{}/api/tags", trimmed_base);
        client.get(&url).timeout(std::time::Duration::from_secs(5)).send().await
    } else if provider_id == "anthropic" {
        let url =
            if trimmed_base.ends_with("/v1") { format!("{}/messages", trimmed_base) } else { format!("{}/v1/messages", trimmed_base) };
        client
            .post(&url)
            .header("x-api-key", trimmed_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&json!({
                "model": "claude-3-5-haiku-20241022",
                "max_tokens": 1,
                "messages": [{"role": "user", "content": "ping"}]
            }))
            .timeout(std::time::Duration::from_secs(8))
            .send()
            .await
    } else {
        models_request(client, provider_id, trimmed_key, trimmed_base).send().await
    };

    match res {
        Ok(resp) => {
            let latency_ms = t0.elapsed().as_millis() as i64;
            let status = resp.status();
            if status.is_success() {
                let body: Value = resp.json().await.unwrap_or(Value::Null);
                Ok((latency_ms, parse_model_ids(&body)))
            } else if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
                let err_text = resp.text().await.unwrap_or_default();
                let detail = serde_json::from_str::<Value>(&err_text)
                    .ok()
                    .and_then(|v| v.get("error").and_then(|e| e.get("message")).and_then(|m| m.as_str()).map(String::from))
                    .unwrap_or_else(|| format!("HTTP {} - invalid credentials", status));
                Err(format!("Authentication failed: {}", detail))
            } else {
                let err_text = resp.text().await.unwrap_or_default();
                let detail = serde_json::from_str::<Value>(&err_text)
                    .ok()
                    .and_then(|v| v.get("error").and_then(|e| e.get("message")).and_then(|m| m.as_str()).map(String::from))
                    .unwrap_or_else(|| format!("HTTP {}", status));
                if status.as_u16() >= 400 && status.as_u16() < 500 && status != reqwest::StatusCode::NOT_FOUND {
                    Err(format!("Provider rejected key: {}", detail))
                } else {
                    Ok((latency_ms, Vec::new()))
                }
            }
        }
        Err(e) => Err(format!("Connection check failed: {}", e)),
    }
}

/// GET {base}/models with the provider's auth style.
fn models_request(client: &reqwest::Client, provider_id: &str, key: &str, base: &str) -> reqwest::RequestBuilder {
    let mut req = client.get(format!("{base}/models")).timeout(std::time::Duration::from_secs(10));
    if provider_id == "xiaomi-mimo" {
        req = req.header("X-Mimo-Source", "mimocode-cli");
    }
    if provider_id == "azure-openai" {
        req = req.header("api-key", key);
    } else if !key.is_empty() {
        req = req.bearer_auth(key);
    }
    req
}

/// Model ids from an OpenAI-style `{data:[{id}]}`, a `{models:[{id|name}]}` list, or Ollama tags.
pub fn parse_model_ids(body: &Value) -> Vec<String> {
    // Antigravity keys models by id: {models: {<id>: {isInternal, ...}}}.
    if let Some(map) = body["models"].as_object() {
        // `-disabled` and editor-only `tab_*` entries are listed but refuse chat requests.
        let mut out: Vec<String> = map
            .iter()
            .filter(|(id, m)| m["isInternal"] != true && !id.ends_with("-disabled") && !id.starts_with("tab_"))
            .map(|(id, _)| id.clone())
            .collect();
        out.sort();
        return out;
    }
    let list = body["data"].as_array().or(body["models"].as_array()).or(body.as_array());
    let mut out: Vec<String> = list
        .into_iter()
        .flatten()
        .filter(|m| m["hidden"] != true)
        .filter_map(|m| m["id"].as_str().or(m["name"].as_str()).or(m["model"].as_str()).or(m.as_str()))
        .map(|id| id.trim_start_matches("models/").to_string()) // Gemini prefixes ids
        .filter(|id| !id.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Re-fetches one provider's model list (API key or subscription) and stores it.
pub async fn sync_provider_models(engine: &GatewayEngine, id: &str) -> Result<Vec<String>, String> {
    let req = if let Some(p) = find_provider_by_id(id) {
        let rec = engine.store.get_provider_record(id);
        let base = rec.and_then(|r| r.base_url).filter(|s| !s.trim().is_empty()).unwrap_or_else(|| p.base_url.to_string());
        if base.is_empty() {
            return Err(format!("{} needs a base URL", p.name));
        }
        let key = engine.store.get_provider_key(id).unwrap_or_default();
        if p.needs_key && key.is_empty() {
            return Err(format!("{} is not connected", p.name));
        }
        let base = base.trim_end_matches('/');
        if id == "commandcode" {
            crate::oauth::commandcode_headers(engine.http_client.get(format!("{base}/provider/v1/models?view=client")), &key, "models")
        } else if id == "ollama" {
            engine.http_client.get(format!("{}/api/tags", base.trim_end_matches("/v1")))
        } else if p.family == "anthropic" {
            engine.http_client.get(format!("{base}/models?limit=1000")).header("x-api-key", key).header("anthropic-version", "2023-06-01")
        } else {
            models_request(&engine.http_client, id, &key, base)
        }
    } else if find_oauth_by_id(id).is_some() {
        let acc = engine
            .store
            .list_accounts()
            .into_iter()
            .find(|a| a.provider == id && a.enabled)
            .ok_or_else(|| format!("No enabled {id} account"))?;
        let cred = crate::oauth::fresh_credential(&engine.http_client, &engine.store, &acc, false).await?;
        crate::oauth::subscription_models_request(&engine.http_client, id, &cred)
            .ok_or_else(|| format!("{id} has no model list; using built-in models"))?
    } else {
        return Err(format!("Unknown provider {id}"));
    };
    let resp = req.timeout(std::time::Duration::from_secs(15)).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let models = parse_model_ids(&resp.json::<Value>().await.map_err(|e| e.to_string())?);
    if models.is_empty() {
        return Err("Provider returned no models".into());
    }
    engine.store.set_synced_models(id, &models)?;
    Ok(models)
}

/// Refreshes every connected provider whose list is older than a day.
pub async fn sync_stale_models(engine: &GatewayEngine) {
    let now = crate::store::now();
    let mut ids: Vec<String> = engine
        .store
        .list_providers()
        .into_iter()
        .filter(|r| {
            r.enabled
                && (r.has_key
                    || find_provider_by_id(&r.id).map(|p| !p.needs_key && !p.base_url.is_empty() && p.id != "ollama").unwrap_or(false))
        })
        .map(|r| r.id)
        .collect();
    for a in engine.store.list_accounts() {
        if a.enabled && !ids.contains(&a.provider) {
            ids.push(a.provider);
        }
    }
    for id in ids {
        let (_, at) = engine.store.get_synced_models(&id);
        if at.map(|t| now - t < 86_400.0).unwrap_or(false) {
            continue;
        }
        if let Err(e) = sync_provider_models(engine, &id).await {
            eprintln!("[GATEWAY MODELS] {id}: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    mod parse_model_ids {
        use crate::models::parse_model_ids;
        use serde_json::json;

        #[test]
        fn reads_an_openai_data_list_sorted_and_deduplicated() {
            assert_eq!(parse_model_ids(&json!({"data":[{"id":"b"},{"id":"a"},{"id":"a"}]})), vec!["a", "b"]);
        }

        #[test]
        fn strips_the_gemini_models_prefix() {
            assert_eq!(parse_model_ids(&json!({"models":[{"name":"models/gemini-2.5-pro"}]})), vec!["gemini-2.5-pro"]);
        }

        #[test]
        fn reads_ollama_tags() {
            assert_eq!(parse_model_ids(&json!({"models":[{"name":"llama3.2:latest","model":"llama3.2:latest"}]})), vec!["llama3.2:latest"]);
        }

        #[test]
        fn returns_nothing_for_an_error_body() {
            assert!(parse_model_ids(&json!({"error":"nope"})).is_empty());
        }

        #[test]
        fn skips_hidden_models() {
            assert_eq!(parse_model_ids(&json!({"data":[{"id":"a"},{"id":"b","hidden":true}]})), vec!["a"]);
        }

        #[test]
        fn reads_antigravity_model_maps_without_internal_models() {
            assert_eq!(
                parse_model_ids(&json!({"models":{"gemini-3.8-flash-medium":{},"tab_flash":{"isInternal":true}}})),
                vec!["gemini-3.8-flash-medium"]
            );
        }
    }
}
