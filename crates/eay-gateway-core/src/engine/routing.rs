use super::GatewayEngine;
use crate::providers::{
    find_oauth_by_alias, find_oauth_by_id, find_provider_by_alias, find_provider_by_id, parse_model_string, Target, OAUTH_PROVIDERS,
    PROVIDERS,
};
use crate::store::{AccountRecord, GatewaySettings, ProviderRecord};
use crate::translate::MediaRequirements;
use serde_json::Value;

impl GatewayEngine {
    /// Expands a requested model into ordered provider/model targets.
    /// `auto` = every connected provider by priority, `<alias>/auto` = one provider,
    /// `auto/<combo>` = a user-defined mix (entries may themselves be `<alias>/auto`).
    pub fn resolve_targets(&self, requested: &str) -> Vec<Target> {
        let s = self.store.get_gateway_settings();
        let req = requested.trim();
        let mut out = Vec::new();
        if let Some(name) = req.strip_prefix("auto/") {
            match self.store.list_combos().get(name) {
                Some(entries) => entries.iter().for_each(|e| extend_unique(&mut out, self.resolve_one(e, &s))),
                None => extend_unique(&mut out, self.provider_auto(name, &s)), // "auto/cx" == "cx/auto"
            }
            return out;
        }
        extend_unique(&mut out, self.resolve_one(req, &s));
        out
    }

    fn resolve_one(&self, req: &str, s: &GatewaySettings) -> Vec<Target> {
        let req = req.trim();
        if req.is_empty() || req == "auto" {
            return self.auto_provider_order(s).iter().flat_map(|id| self.provider_auto(id, s)).collect();
        }
        if let Some(prefix) = req.strip_suffix("/auto") {
            return self.provider_auto(prefix, s);
        }
        let Ok(t) = parse_model_string(req) else { return Vec::new() };
        let mut out = vec![t.clone()];
        // A subscription model falls back to that provider's other plan-appropriate models.
        if t.kind == "oauth" {
            extend_unique(&mut out, self.provider_auto(&t.provider, s));
        }
        out
    }

    fn provider_auto(&self, prefix: &str, s: &GatewaySettings) -> Vec<Target> {
        let target = |provider: &str, model: &str, kind: &str| Target { provider: provider.into(), model: model.into(), kind: kind.into() };
        if let Some(op) = find_oauth_by_alias(prefix).or_else(|| find_oauth_by_id(prefix)) {
            let accounts: Vec<AccountRecord> =
                self.store.list_accounts().into_iter().filter(|a| a.provider == op.id && a.enabled).collect();
            let all_free = accounts.is_empty() || accounts.iter().all(|a| a.is_free());
            let paid: &[&str] = if all_free { &[] } else { op.preferred_auto };
            let mut out = Vec::new();
            extend_unique(&mut out, paid.iter().chain(op.preferred_auto_free).map(|m| target(op.id, m, "oauth")));
            return out;
        }
        if let Some(p) = find_provider_by_alias(prefix).or_else(|| find_provider_by_id(prefix)) {
            // The user's own models first: adding one is the signal they want it used.
            let mut models = self.store.get_provider_custom_models(p.id);
            for m in p.seed_models {
                if !models.iter().any(|x| x == m) {
                    models.push(m.to_string());
                }
            }
            return models.iter().take(s.auto_models_per_provider).map(|m| target(p.id, m, "apikey")).collect();
        }
        Vec::new()
    }

    /// Providers `auto` uses, in order: the saved priority first, then any other connected
    /// subscription providers, then connected API-key providers.
    pub fn auto_provider_order(&self, s: &GatewaySettings) -> Vec<String> {
        let accounts = self.store.list_accounts();
        let records: std::collections::HashMap<String, ProviderRecord> =
            self.store.list_providers().into_iter().map(|r| (r.id.clone(), r)).collect();
        let usable = |id: &str| -> bool {
            if !self.store.is_provider_enabled(id) {
                return false;
            }
            if find_oauth_by_id(id).is_some() {
                return accounts.iter().any(|a| a.provider == id && a.enabled);
            }
            let extra_key = accounts.iter().any(|a| a.provider == id && a.enabled);
            match (find_provider_by_id(id), records.get(id)) {
                (Some(p), Some(r)) => r.has_key || !p.needs_key || extra_key,
                (Some(_), None) => extra_key,
                _ => false,
            }
        };
        let mut order: Vec<String> = Vec::new();
        let candidates = s
            .provider_priority
            .iter()
            .cloned()
            .chain(OAUTH_PROVIDERS.iter().map(|o| o.id.to_string()))
            .chain(PROVIDERS.iter().map(|p| p.id.to_string()));
        for id in candidates {
            if !order.contains(&id) && usable(&id) {
                order.push(id);
            }
        }
        order
    }

    /// Model ids worth showing in pickers for the auto modes.
    pub fn auto_model_ids(&self) -> Vec<String> {
        let s = self.store.get_gateway_settings();
        let mut ids = vec!["auto".to_string()];
        let mut combos: Vec<String> = self.store.list_combos().into_keys().collect();
        combos.sort();
        ids.extend(combos.into_iter().map(|c| format!("auto/{c}")));
        for id in self.auto_provider_order(&s) {
            let alias = find_oauth_by_id(&id).map(|o| o.alias).or_else(|| find_provider_by_id(&id).map(|p| p.alias)).unwrap_or(&id);
            ids.push(format!("{alias}/auto"));
        }
        ids
    }

    /// Turns targets into concrete (target, account) attempts following the routing settings.
    /// Attempts on cooling accounts/providers move to the end, so they're only a last resort.
    pub(super) fn plan_attempts(&self, targets: &[Target], s: &GatewaySettings) -> Vec<(Target, Option<AccountRecord>)> {
        let all_accounts = self.store.list_accounts();
        let mut ordered: std::collections::HashMap<String, Vec<AccountRecord>> = Default::default();
        let mut out = Vec::new();
        // Consecutive targets of one provider form a group, so combos keep their interleaving.
        for group in targets.chunk_by(|a, b| a.provider == b.provider) {
            let prov = &group[0].provider;
            let accs = ordered.entry(prov.clone()).or_insert_with(|| self.ordered_accounts(prov, &all_accounts, s)).clone();
            if group[0].kind != "oauth" {
                // API-key providers: the main key (None) first, then any extra keys, in routing order.
                let keys: Vec<Option<AccountRecord>> = std::iter::once(None).chain(accs.into_iter().map(Some)).collect();
                if s.fallback_order == "account_first" {
                    for k in &keys {
                        out.extend(group.iter().map(|t| (t.clone(), k.clone())));
                    }
                } else {
                    for t in group {
                        out.extend(keys.iter().map(|k| (t.clone(), k.clone())));
                    }
                }
                continue;
            }
            if s.fallback_order == "account_first" {
                for a in &accs {
                    out.extend(group.iter().map(|t| (t.clone(), Some(a.clone()))));
                }
            } else {
                for t in group {
                    out.extend(accs.iter().map(|a| (t.clone(), Some(a.clone()))));
                }
            }
        }
        out.sort_by_key(|(t, a)| self.store.cooling_until(&cool_key(t, a.as_ref())).is_some());
        out
    }

    fn ordered_accounts(&self, provider: &str, all: &[AccountRecord], s: &GatewaySettings) -> Vec<AccountRecord> {
        // `all` is already in the user's per-provider sort order.
        let mut accs: Vec<AccountRecord> = all.iter().filter(|a| a.provider == provider && a.enabled).cloned().collect();
        if accs.len() < 2 {
            return accs;
        }
        match s.routing_strategy.as_str() {
            "round_robin" => {
                let mut rr = self.rr.lock();
                let n = rr.entry(provider.to_string()).or_insert(0);
                let len = accs.len();
                accs.rotate_left(*n % len);
                *n = n.wrapping_add(1);
            }
            "fill_first" => {
                if let Some(id) = self.sticky.lock().get(provider) {
                    if let Some(pos) = accs.iter().position(|a| &a.id == id) {
                        let a = accs.remove(pos);
                        accs.insert(0, a);
                    }
                }
            }
            _ => {}
        }
        accs
    }
}

pub(super) fn supports_media(t: &Target, req: MediaRequirements) -> bool {
    let p = t.provider.as_str();
    let m = t.model.to_ascii_lowercase();
    let is_gemini = matches!(p, "antigravity" | "gemini-cli" | "google" | "gemini") || m.contains("gemini");
    let is_claude = matches!(p, "claude" | "anthropic" | "claude-code") || m.contains("claude");
    let is_openai_native = matches!(p, "openai" | "codex" | "azure-openai");

    if req.has_video {
        return is_gemini || (p == "openrouter" && (m.contains("gemini") || m.contains("qwen-vl") || m.contains("qvq")));
    }
    if req.has_audio {
        return is_gemini
            || (is_openai_native && (m.contains("audio") || m.contains("gpt-4o")))
            || (p == "openrouter" && (m.contains("gemini") || m.contains("audio")));
    }
    if req.has_document {
        return is_gemini || is_claude || is_openai_native || p == "openrouter";
    }
    if req.has_image {
        if matches!(p, "deepseek" | "kiro" | "cerebras") {
            return false;
        }
        return is_gemini
            || is_claude
            || is_openai_native
            || matches!(p, "copilot" | "commandcode" | "openrouter")
            || [
                "vision", "-vl", "gpt-4o", "gpt-4.1", "gpt-5", "o1", "o3", "o4", "pixtral", "llava", "qwen-vl", "llama-4",
                "llama-3.2-11b", "llama-3.2-90b",
            ]
            .iter()
            .any(|k| m.contains(k));
    }
    true
}

/// Keeps only multimodal-capable targets when `targets` has multiple candidates and at least one matches.
pub(super) fn filter_multimodal_targets(targets: &mut Vec<Target>, req: MediaRequirements) {
    if !req.any() || targets.len() <= 1 {
        return;
    }
    if targets.iter().any(|t| supports_media(t, req)) {
        targets.retain(|t| supports_media(t, req));
    }
}

fn extend_unique(out: &mut Vec<Target>, items: impl IntoIterator<Item = Target>) {
    for t in items {
        if !out.contains(&t) {
            out.push(t);
        }
    }
}

/// Cooldown bucket: a subscription account, or a whole API-key provider.
pub(super) fn cool_key(t: &Target, acc: Option<&AccountRecord>) -> String {
    acc.map(|a| a.id.clone()).unwrap_or_else(|| format!("apikey:{}", t.provider))
}

/// Seconds to bench a rate-limited / out-of-quota upstream, or None for other errors.
/// `default` 0 disables cooldown unless the upstream names its own reset time.
pub(super) fn cooldown_for(status: u16, retry_after: Option<u64>, body: &str, default: u64) -> Option<u64> {
    let b = body.to_lowercase();
    let limited = status == 429
        || ["rate limit", "rate_limit", "quota", "usage limit", "usage_limit", "too many requests"].iter().any(|k| b.contains(k));
    if !limited {
        return None;
    }
    let hinted = retry_after.or_else(|| {
        let j: Value = serde_json::from_str(body).ok()?;
        j["error"]["resets_in_seconds"].as_u64().or(j["resets_in_seconds"].as_u64())
    });
    let secs = hinted.unwrap_or(default);
    (secs > 0).then(|| secs.min(6 * 3600))
}
