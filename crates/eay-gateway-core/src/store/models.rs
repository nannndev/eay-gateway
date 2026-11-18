use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One row of the request log.
pub struct UsageEntry<'a> {
    pub key_id: Option<&'a str>,
    pub key_name: Option<&'a str>,
    pub provider: Option<&'a str>,
    pub model: Option<&'a str>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Part of `input_tokens` served from the provider's prompt cache.
    pub cached_tokens: i64,
    pub duration_ms: i64,
    /// "success" or "error".
    pub status: &'a str,
    pub status_code: i64,
    pub error: Option<&'a str>,
    pub streaming: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountRecord {
    pub id: String,
    pub provider: String,
    pub label: Option<String>,
    pub email: Option<String>,
    pub connected_at: f64,
    pub refreshed_at: Option<f64>,
    pub last_error: Option<String>,
    pub sort_order: i64,
    pub enabled: bool,
    pub cred: Value,
}

impl AccountRecord {
    pub fn detect_plan(&self) -> String {
        if self.provider == "codex" {
            if let Some(p) = self.cred.get("plan").and_then(|v| v.as_str()) {
                if !p.is_empty() {
                    return p.to_lowercase();
                }
            }
            if let Some(token) = self.cred.get("access_token").and_then(|t| t.as_str()) {
                let parts: Vec<&str> = token.split('.').collect();
                if parts.len() >= 2 {
                    let decoded_res = base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, parts[1])
                        .or_else(|_| base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE, parts[1]));
                    if let Ok(decoded) = decoded_res {
                        if let Ok(claims) = serde_json::from_slice::<Value>(&decoded) {
                            if let Some(plan) =
                                claims.get("https://api.openai.com/auth").and_then(|a| a.get("chatgpt_plan_type")).and_then(|p| p.as_str())
                            {
                                return plan.to_lowercase();
                            }
                        }
                    }
                }
            }
        }
        if let Some(p) = self.cred.get("plan").and_then(|v| v.as_str()) {
            return p.to_lowercase();
        }
        "free".to_string()
    }

    pub fn is_free(&self) -> bool {
        let plan = self.detect_plan();
        plan == "free" || plan == "none" || plan.is_empty()
    }

    pub fn display_plan(&self) -> String {
        match self.detect_plan().as_str() {
            "free" => "Free".to_string(),
            "plus" => "Plus".to_string(),
            "pro" => "Pro".to_string(),
            "team" => "Team".to_string(),
            "business" => "Business".to_string(),
            "enterprise" => "Enterprise".to_string(),
            other => {
                if other.is_empty() {
                    "Free".to_string()
                } else {
                    let mut c = other.chars();
                    match c.next() {
                        None => String::new(),
                        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderRecord {
    pub id: String,
    pub enabled: bool,
    pub base_url: Option<String>,
    pub default_model: Option<String>,
    pub models: Vec<String>,
    pub verified_at: Option<f64>,
    pub latency_ms: Option<i64>,
    pub error: Option<String>,
    pub has_key: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtualKeyRecord {
    pub id: String,
    pub name: String,
    pub key_prefix: String,
    pub enabled: bool,
    pub allowed_providers: Option<Vec<String>>,
    pub rate_limit_per_min: Option<i64>,
    #[serde(default)]
    pub max_tokens_budget: Option<i64>,
    #[serde(default)]
    pub max_cost_usd: Option<f64>,
    #[serde(default)]
    pub default_skills: Option<Vec<String>>,
    pub created_at: f64,
    pub last_used_at: Option<f64>,
    pub total_requests: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillRecord {
    pub name: String,
    pub description: String,
    pub prompt: String,
    pub enabled: bool,
    pub updated_at: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProfile {
    pub name: String,
    pub description: String,
    pub model: String,
    pub skills: Vec<String>,
    pub personality: String,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u64>,
    pub enabled: bool,
    pub updated_at: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewaySettings {
    /// "priority" (ordered fallback), "round_robin", or "fill_first" (sticky until it fails).
    pub routing_strategy: String,
    /// "model_first": try a model on every account before the next model; "account_first": the reverse.
    pub fallback_order: String,
    /// Provider ids in the order `auto` tries them; unlisted connected providers follow alphabetically.
    pub provider_priority: Vec<String>,
    /// How many models per API-key provider `auto` tries (its custom models first, then seeds).
    pub auto_models_per_provider: usize,
    /// Seconds an account/provider is skipped after a rate limit or quota error.
    pub cooldown_secs: u64,
    pub gemini_oauth_client_id: String,
    pub gemini_oauth_client_secret_masked: Option<String>,
    pub gemini_oauth_configured: bool,
}
