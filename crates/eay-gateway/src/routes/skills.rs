use crate::error::GatewayError;
use axum::{
    extract::{Path, Query, State},
    Json,
};
use eay_gateway_core::engine::GatewayEngine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize)]
struct RepoStats {
    stars: u64,
    forks: u64,
    watchers: u64,
    description: Option<String>,
    pushed_at: Option<String>,
    default_branch: Option<String>,
    github_url: String,
}

type RepoStatsCache = Mutex<HashMap<String, (Instant, Option<RepoStats>)>>;
type MarketplaceQueryCache = Mutex<HashMap<String, (Instant, Vec<Value>)>>;

fn repo_stats_cache() -> &'static RepoStatsCache {
    static CACHE: OnceLock<RepoStatsCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn marketplace_query_cache() -> &'static MarketplaceQueryCache {
    static CACHE: OnceLock<MarketplaceQueryCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

const REPO_STATS_TTL: Duration = Duration::from_secs(900);
const QUERY_CACHE_TTL: Duration = Duration::from_secs(300);

async fn fetch_repo_stats(engine: GatewayEngine, source: String) -> (String, Option<RepoStats>) {
    if let Ok(guard) = repo_stats_cache().lock() {
        if let Some((ts, cached)) = guard.get(&source) {
            if ts.elapsed() < REPO_STATS_TTL {
                return (source, cached.clone());
            }
        }
    }

    let url = format!("https://ungh.cc/repos/{source}");
    let stats = match engine.http_client.get(&url).header("User-Agent", "eay-gateway/0.6").timeout(Duration::from_secs(3)).send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<Value>().await {
            Ok(v) => v.get("repo").map(|r| RepoStats {
                stars: r.get("stars").and_then(Value::as_u64).unwrap_or(0),
                forks: r.get("forks").and_then(Value::as_u64).unwrap_or(0),
                watchers: r.get("watchers").and_then(Value::as_u64).unwrap_or(0),
                description: r.get("description").and_then(Value::as_str).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
                pushed_at: r.get("pushedAt").and_then(Value::as_str).map(String::from),
                default_branch: r.get("defaultBranch").and_then(Value::as_str).map(String::from),
                github_url: format!("https://github.com/{source}"),
            }),
            Err(_) => None,
        },
        _ => None,
    };

    if let Ok(mut guard) = repo_stats_cache().lock() {
        if guard.len() > 400 {
            guard.retain(|_, (ts, _)| ts.elapsed() < REPO_STATS_TTL);
        }
        guard.insert(source.clone(), (Instant::now(), stats.clone()));
    }

    (source, stats)
}

#[derive(Deserialize)]
pub struct SaveSkillPayload {
    name: Option<String>,
    #[serde(default)]
    description: String,
    prompt: String,
    #[serde(default = "default_true")]
    enabled: bool,
}

#[derive(Deserialize)]
pub struct MarketplaceSearchQuery {
    q: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}

#[derive(Deserialize)]
pub struct MarketplaceFetchPayload {
    source: String,
    #[serde(alias = "skillId")]
    skill_id: String,
}

fn default_true() -> bool {
    true
}

fn is_safe_repo(source: &str) -> bool {
    let mut parts = source.split('/');
    let (Some(owner), Some(repo), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let ok_seg = |s: &str| {
        !s.is_empty()
            && s.len() <= 100
            && !s.contains("..")
            && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    };
    ok_seg(owner) && ok_seg(repo)
}

fn is_safe_skill_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.contains("..")
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Extracts optional YAML frontmatter (`--- ... ---`) into (name, description, body).
fn parse_skill_frontmatter(raw: &str) -> (Option<String>, Option<String>, String) {
    let trimmed = raw.trim_start_matches('\u{feff}');
    let Some(rest) = trimmed.strip_prefix("---\n").or_else(|| trimmed.strip_prefix("---\r\n")) else {
        return (None, None, trimmed.trim().to_string());
    };
    let Some(end_idx) = rest.find("\n---") else {
        return (None, None, trimmed.trim().to_string());
    };
    let fm = &rest[..end_idx];
    let after = &rest[end_idx + 4..];
    let body = after.trim_start_matches('\r').trim_start_matches('\n').trim().to_string();

    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    for line in fm.lines() {
        let l = line.trim();
        if let Some(val) = l.strip_prefix("name:") {
            let clean = val.trim().trim_matches('"').trim_matches('\'').trim();
            if !clean.is_empty() {
                name = Some(clean.to_string());
            }
        } else if let Some(val) = l.strip_prefix("description:") {
            let clean = val.trim().trim_matches('"').trim_matches('\'').trim();
            if !clean.is_empty() {
                description = Some(clean.to_string());
            }
        }
    }
    (name, description, if body.is_empty() { trimmed.trim().to_string() } else { body })
}

pub async fn handle_marketplace_search(
    State(engine): State<GatewayEngine>,
    Query(query): Query<MarketplaceSearchQuery>,
) -> Result<Json<Value>, GatewayError> {
    let q = query.q.as_deref().unwrap_or("best").trim();
    let q = if q.is_empty() { "best" } else { q };
    let limit = query.limit.unwrap_or(16).clamp(1, 50);
    let offset = query.offset.unwrap_or(0);
    let cache_key = q.to_lowercase();

    let cached_all = marketplace_query_cache()
        .lock()
        .ok()
        .and_then(|g| g.get(&cache_key).filter(|(ts, _)| ts.elapsed() < QUERY_CACHE_TTL).map(|(_, items)| items.clone()));

    let all_skills = if let Some(items) = cached_all {
        items
    } else {
        let res = engine
            .http_client
            .get("https://skills.sh/api/search")
            .query(&[("q", q), ("limit", "100")])
            .header("User-Agent", "eay-gateway/0.6")
            .timeout(std::time::Duration::from_secs(12))
            .send()
            .await
            .map_err(|e| GatewayError::BadGateway(format!("Failed to reach skills.sh: {e}")))?;

        if !res.status().is_success() {
            return Err(GatewayError::BadGateway(format!("skills.sh returned HTTP {}", res.status().as_u16())));
        }
        let body: Value = res.json().await.map_err(|e| GatewayError::BadGateway(format!("Invalid JSON from skills.sh: {e}")))?;

        let items = body.get("skills").and_then(Value::as_array).cloned().unwrap_or_default();

        if let Ok(mut guard) = marketplace_query_cache().lock() {
            if guard.len() > 100 {
                guard.retain(|_, (ts, _)| ts.elapsed() < QUERY_CACHE_TTL);
            }
            guard.insert(cache_key, (Instant::now(), items.clone()));
        }
        items
    };

    let total = all_skills.len();
    let end = (offset + limit).min(total);
    let mut skills: Vec<Value> = if offset < total { all_skills[offset..end].to_vec() } else { Vec::new() };
    let has_more = end < total;

    // Concurrently enrich unique GitHub repositories in this page with cached star/fork/description stats
    let mut seen_repos = HashSet::new();
    let unique_repos: Vec<String> = skills
        .iter()
        .filter_map(|s| s.get("source").and_then(Value::as_str))
        .filter(|src| is_safe_repo(src) && !src.contains('.'))
        .filter(|src| seen_repos.insert((*src).to_string()))
        .take(20)
        .map(String::from)
        .collect();

    let mut join_set = tokio::task::JoinSet::new();
    for repo in unique_repos {
        let eng = engine.clone();
        join_set.spawn(async move { fetch_repo_stats(eng, repo).await });
    }
    let mut stats_map: HashMap<String, RepoStats> = HashMap::new();
    while let Some(res) = join_set.join_next().await {
        if let Ok((repo, Some(stats))) = res {
            stats_map.insert(repo, stats);
        }
    }

    for item in &mut skills {
        if let Some(obj) = item.as_object_mut() {
            if let Some(src) = obj.get("source").and_then(Value::as_str).map(String::from) {
                if let Some(stats) = stats_map.get(&src) {
                    if let Ok(val) = serde_json::to_value(stats) {
                        obj.insert("repo_stats".into(), val);
                    }
                }
            }
        }
    }

    Ok(Json(json!({
        "query": q,
        "skills": skills,
        "total": total,
        "offset": offset,
        "has_more": has_more,
    })))
}

pub async fn handle_marketplace_fetch(
    State(engine): State<GatewayEngine>,
    Json(payload): Json<MarketplaceFetchPayload>,
) -> Result<Json<Value>, GatewayError> {
    let source = payload.source.trim();
    let skill_id = payload.skill_id.trim();
    if !is_safe_repo(source) {
        return Err(GatewayError::BadRequest("Invalid GitHub repository source (expected owner/repo)".into()));
    }
    if !is_safe_skill_id(skill_id) {
        return Err(GatewayError::BadRequest("Invalid skillId".into()));
    }

    let stripped = skill_id.split_once('-').map(|(_, rest)| rest).filter(|r| !r.is_empty());
    let mut candidates =
        vec![format!("skills/{skill_id}/SKILL.md"), format!("{skill_id}/SKILL.md"), format!(".claude/skills/{skill_id}/SKILL.md")];
    if let Some(short) = stripped {
        candidates.push(format!("skills/{short}/SKILL.md"));
        candidates.push(format!("{short}/SKILL.md"));
    }
    candidates.push("SKILL.md".into());

    let mut found: Option<(String, String, String)> = None;

    'direct: for branch in ["main", "master"] {
        for path in &candidates {
            let url = format!("https://raw.githubusercontent.com/{source}/{branch}/{path}");
            if let Ok(resp) =
                engine.http_client.get(&url).header("User-Agent", "eay-gateway/0.6").timeout(std::time::Duration::from_secs(8)).send().await
            {
                if resp.status().is_success() {
                    if let Ok(text) = resp.text().await {
                        if !text.trim().is_empty() {
                            found = Some((text, url, path.clone()));
                            break 'direct;
                        }
                    }
                }
            }
        }
    }

    // Fallback: query GitHub tree API to locate SKILL.md inside nested directories
    if found.is_none() {
        let tree_url = format!("https://api.github.com/repos/{source}/git/trees/HEAD?recursive=1");
        if let Ok(resp) = engine
            .http_client
            .get(&tree_url)
            .header("User-Agent", "eay-gateway/0.6")
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
        {
            if resp.status().is_success() {
                if let Ok(tree_json) = resp.json::<Value>().await {
                    let skill_paths: Vec<String> = tree_json["tree"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|item| item["path"].as_str())
                        .filter(|p| p.ends_with("SKILL.md") || p.ends_with("skill.md"))
                        .map(String::from)
                        .collect();

                    let needle_lower = skill_id.to_lowercase();
                    let short_lower = stripped.map(|s| s.to_lowercase());
                    let best_path = skill_paths
                        .iter()
                        .find(|p| p.to_lowercase().contains(&needle_lower))
                        .or_else(|| short_lower.as_ref().and_then(|s| skill_paths.iter().find(|p| p.to_lowercase().contains(s))))
                        .or_else(|| skill_paths.first());

                    if let Some(path) = best_path {
                        let raw_url = format!("https://raw.githubusercontent.com/{source}/HEAD/{path}");
                        if let Ok(raw_resp) = engine
                            .http_client
                            .get(&raw_url)
                            .header("User-Agent", "eay-gateway/0.6")
                            .timeout(std::time::Duration::from_secs(10))
                            .send()
                            .await
                        {
                            if raw_resp.status().is_success() {
                                if let Ok(text) = raw_resp.text().await {
                                    if !text.trim().is_empty() {
                                        found = Some((text, raw_url, path.clone()));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let Some((raw, source_url, path)) = found else {
        return Err(GatewayError::NotFound(format!("Could not locate SKILL.md for '{skill_id}' in github.com/{source}")));
    };

    let (fm_name, fm_desc, body) = parse_skill_frontmatter(&raw);
    let clean_name = fm_name
        .unwrap_or_else(|| skill_id.to_string())
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect::<String>();

    Ok(Json(json!({
        "name": clean_name,
        "description": fm_desc.unwrap_or_else(|| format!("Imported from {source}")),
        "prompt": body,
        "raw": raw,
        "source_url": source_url,
        "github_url": format!("https://github.com/{source}/blob/HEAD/{path}"),
        "path": path,
    })))
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
