use super::{now, query_rows, GatewayStore, UsageEntry};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::HashMap;

impl GatewayStore {
    // ---- Usage & Stats ---------------------------------------------
    pub fn list_usage(&self, limit: i64) -> Vec<Value> {
        let conn = self.conn.lock();
        query_rows(&conn, "SELECT id, ts, key_id, key_name, provider, model, input_tokens, output_tokens, duration_ms, status, status_code, error, streaming, agent, team FROM usage ORDER BY ts DESC LIMIT ?1", params![limit], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "ts": r.get::<_, f64>(1)?,
                    "key_id": r.get::<_, Option<String>>(2)?,
                    "key_name": r.get::<_, Option<String>>(3)?,
                    "provider": r.get::<_, Option<String>>(4)?,
                    "model": r.get::<_, Option<String>>(5)?,
                    "input_tokens": r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                    "output_tokens": r.get::<_, Option<i64>>(7)?.unwrap_or(0),
                    "duration_ms": r.get::<_, Option<i64>>(8)?.unwrap_or(0),
                    "status": r.get::<_, Option<String>>(9)?.unwrap_or_else(|| "success".to_string()),
                    "status_code": r.get::<_, Option<i64>>(10)?.unwrap_or(200),
                    "error": r.get::<_, Option<String>>(11)?,
                    "streaming": r.get::<_, Option<i64>>(12)?.unwrap_or(0) != 0,
                    "agent": r.get::<_, Option<String>>(13)?,
                    "team": r.get::<_, Option<String>>(14)?,
                }))
            })
    }

    pub fn clear_usage(&self) -> Result<(), String> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM usage", []).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn record_usage(&self, u: &UsageEntry<'_>) {
        let conn = self.conn.lock();
        let id = format!("usg_{}", uuid::Uuid::new_v4().simple());
        let cost = u.model.map_or(0.0, |m| crate::pricing::cost(m, u.input_tokens, u.output_tokens, u.cached_tokens));
        let agent = clip_usage_label(u.agent);
        let team = clip_usage_label(u.team);
        let project = clip_usage_label(u.project);
        let _ = conn.execute(
            "INSERT INTO usage (id, ts, key_id, key_name, provider, model, input_tokens, output_tokens, duration_ms, status, status_code, error, streaming, cached_tokens, cost, agent, team, project) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params![
                id, now(), u.key_id, u.key_name, u.provider, u.model,
                u.input_tokens, u.output_tokens, u.duration_ms, u.status, u.status_code, u.error,
                i64::from(u.streaming), u.cached_tokens, cost, agent, team, project
            ],
        );
    }

    /// Tokens per provider per local day for the last `days` days (oldest first, today last).
    /// `utc_offset_secs` shifts unix time to the caller's local day.
    pub fn daily_tokens_by_provider(&self, days: u32, utc_offset_secs: i64) -> HashMap<String, Vec<i64>> {
        let conn = self.conn.lock();
        let today = (now() as i64 + utc_offset_secs).div_euclid(86_400);
        let first = today - i64::from(days.max(1)) + 1;
        let rows = query_rows(
            &conn,
            "SELECT provider, CAST((ts + ?1) / 86400 AS INTEGER) AS d, SUM(COALESCE(input_tokens,0) + COALESCE(output_tokens,0))
             FROM usage WHERE provider IS NOT NULL AND (ts + ?1) >= ?2 * 86400 GROUP BY provider, d",
            params![utc_offset_secs, first],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)),
        );
        let mut out: HashMap<String, Vec<i64>> = HashMap::new();
        for (provider, day, tokens) in rows {
            let idx = day - first;
            if (0..i64::from(days.max(1))).contains(&idx) {
                out.entry(provider).or_insert_with(|| vec![0; days.max(1) as usize])[idx as usize] += tokens;
            }
        }
        out
    }

    /// Request log rollup for charts: per-day totals, latest requests and top models.
    /// `days` None covers all history; days are local per `utc_offset_secs`.
    pub fn analytics(&self, days: Option<u32>, utc_offset_secs: i64) -> Value {
        let conn = self.conn.lock();
        let since = days.map_or(0.0, |d| {
            let today = (now() as i64 + utc_offset_secs).div_euclid(86_400);
            ((today - i64::from(d.max(1)) + 1) * 86_400 - utc_offset_secs) as f64
        });
        let days_json = query_rows(
            &conn,
            "SELECT CAST((ts + ?1) / 86400 AS INTEGER) AS d, COUNT(*), SUM(COALESCE(input_tokens,0)), SUM(COALESCE(output_tokens,0)),
                    SUM(COALESCE(cached_tokens,0)), SUM(COALESCE(cost,0))
             FROM usage WHERE ts >= ?2 GROUP BY d ORDER BY d",
            params![utc_offset_secs, since],
            |r| {
                Ok(json!({
                    "date": chrono::DateTime::from_timestamp(r.get::<_, i64>(0)? * 86_400, 0).map(|t| t.format("%Y-%m-%d").to_string()).unwrap_or_default(),
                    "requests": r.get::<_, i64>(1)?, "input": r.get::<_, i64>(2)?, "output": r.get::<_, i64>(3)?,
                    "cached": r.get::<_, i64>(4)?, "cost": r.get::<_, f64>(5)?,
                }))
            },
        );
        let recent = query_rows(
            &conn,
            "SELECT ts, COALESCE(model,'auto'), COALESCE(provider,''), COALESCE(input_tokens,0), COALESCE(output_tokens,0), COALESCE(status_code,200),
                    COALESCE(cost,0)
             FROM usage ORDER BY ts DESC LIMIT 20",
            [],
            |r| {
                Ok(json!({
                    "ts": r.get::<_, f64>(0)?, "model": r.get::<_, String>(1)?, "provider": r.get::<_, String>(2)?,
                    "input": r.get::<_, i64>(3)?, "output": r.get::<_, i64>(4)?, "ok": r.get::<_, i64>(5)? < 400,
                    "cost": r.get::<_, f64>(6)?,
                }))
            },
        );
        let today_start = ((now() as i64 + utc_offset_secs).div_euclid(86_400) * 86_400 - utc_offset_secs) as f64;
        let top = query_rows(
            &conn,
            "SELECT COALESCE(model,'auto'), COALESCE(provider,''), SUM(COALESCE(input_tokens,0) + COALESCE(output_tokens,0)) AS tok, SUM(COALESCE(cost,0))
             FROM usage WHERE ts >= ?1 GROUP BY model, provider ORDER BY tok DESC LIMIT 8",
            params![today_start],
            |r| Ok(json!({ "model": r.get::<_, String>(0)?, "provider": r.get::<_, String>(1)?, "tokens": r.get::<_, i64>(2)?, "cost": r.get::<_, f64>(3)? })),
        );
        let models = query_rows(
            &conn,
            "SELECT COALESCE(model,'auto'), COALESCE(provider,''), SUM(COALESCE(input_tokens,0) + COALESCE(output_tokens,0)) AS tok, SUM(COALESCE(cost,0))
             FROM usage WHERE ts >= ?1 GROUP BY model, provider ORDER BY tok DESC LIMIT 12",
            params![since],
            |r| Ok(json!({ "model": r.get::<_, String>(0)?, "provider": r.get::<_, String>(1)?, "tokens": r.get::<_, i64>(2)?, "cost": r.get::<_, f64>(3)? })),
        );
        // `top_models` covers today only; `models` covers the requested range.
        // `agents` and `teams` skip rows logged before those columns existed.
        let agent_rows = query_rows(
            &conn,
            "SELECT agent, COALESCE(provider,''), COALESCE(model,'auto'), COUNT(*),
                    SUM(COALESCE(input_tokens,0) + COALESCE(output_tokens,0)), SUM(COALESCE(cost,0))
             FROM usage WHERE ts >= ?1 AND agent IS NOT NULL AND agent != ''
             GROUP BY agent, provider, model",
            params![since],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, f64>(5)?,
                ))
            },
        );
        let team_rows = query_rows(
            &conn,
            "SELECT team, COALESCE(agent,''), COALESCE(provider,''), COALESCE(model,'auto'), COUNT(*),
                    SUM(COALESCE(input_tokens,0) + COALESCE(output_tokens,0)), SUM(COALESCE(cost,0))
             FROM usage WHERE ts >= ?1 AND team IS NOT NULL AND team != ''
             GROUP BY team, agent, provider, model",
            params![since],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, f64>(6)?,
                ))
            },
        );
        let agents = fold_agents(agent_rows);
        let teams = fold_teams(&conn, team_rows);
        json!({ "days": days_json, "recent": recent, "top_models": top, "models": models, "agents": agents, "teams": teams })
    }

    /// One project's usage since `since` (unix seconds, 0 = all time): totals, plus per model,
    /// per agent and per team, each with requests, tokens and estimated cost.
    pub fn project_usage(&self, project: &str, since: f64) -> Value {
        let conn = self.conn.lock();
        let totals = conn
            .query_row(
                "SELECT COUNT(*), SUM(COALESCE(input_tokens,0)), SUM(COALESCE(output_tokens,0)), SUM(COALESCE(cached_tokens,0)),
                        SUM(COALESCE(cost,0)), SUM(CASE WHEN COALESCE(status_code,200) >= 400 THEN 1 ELSE 0 END)
                 FROM usage WHERE project = ?1 AND ts >= ?2",
                params![project, since],
                |r| {
                    Ok(json!({
                        "requests": r.get::<_, i64>(0)?, "input_tokens": r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                        "output_tokens": r.get::<_, Option<i64>>(2)?.unwrap_or(0), "cached_tokens": r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                        "cost": r.get::<_, Option<f64>>(4)?.unwrap_or(0.0), "errors": r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    }))
                },
            )
            .unwrap_or_else(|_| json!({}));
        let by = |col: &str| {
            query_rows(
                &conn,
                &format!(
                    "SELECT COALESCE({col},''), COUNT(*), SUM(COALESCE(input_tokens,0) + COALESCE(output_tokens,0)) AS tok, SUM(COALESCE(cost,0))
                     FROM usage WHERE project = ?1 AND ts >= ?2 GROUP BY {col} ORDER BY tok DESC LIMIT 50"
                ),
                params![project, since],
                |r| Ok(json!({ "name": r.get::<_, String>(0)?, "requests": r.get::<_, i64>(1)?, "tokens": r.get::<_, i64>(2)?, "cost": r.get::<_, f64>(3)? })),
            )
        };
        json!({ "totals": totals, "models": by("model"), "agents": by("agent"), "teams": by("team") })
    }

    pub fn get_stats(&self) -> Value {
        let conn = self.conn.lock();
        let mut stats = json!({
            "requests": 0,
            "input_tokens": 0,
            "output_tokens": 0,
            "errors": 0,
            "active_keys": 0,
            "connected_providers": 0,
        });

        let _ = conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0), COALESCE(SUM(CASE WHEN status = 'error' OR status_code >= 400 THEN 1 ELSE 0 END), 0) FROM usage",
            [],
            |r| {
                stats["requests"] = json!(r.get::<_, i64>(0)?);
                stats["input_tokens"] = json!(r.get::<_, i64>(1)?);
                stats["output_tokens"] = json!(r.get::<_, i64>(2)?);
                stats["errors"] = json!(r.get::<_, i64>(3)?);
                Ok(())
            },
        );

        let active_keys: i64 = conn.query_row("SELECT COUNT(*) FROM keys WHERE enabled = 1", [], |r| r.get(0)).unwrap_or(0);
        stats["active_keys"] = json!(active_keys);

        let mut connected_count: i64 = conn
            .query_row("SELECT COUNT(DISTINCT id) FROM providers WHERE api_key IS NOT NULL AND api_key != '' AND enabled = 1", [], |r| {
                r.get(0)
            })
            .unwrap_or(0);
        let accounts_count: i64 =
            conn.query_row("SELECT COUNT(DISTINCT provider) FROM accounts WHERE enabled = 1", [], |r| r.get(0)).unwrap_or(0);
        connected_count += accounts_count;
        stats["connected_providers"] = json!(connected_count);

        stats
    }
}

const USAGE_GROUP_CAP: usize = 30;
const USAGE_MODEL_CAP: usize = 8;

fn clip_usage_label(raw: Option<&str>) -> Option<String> {
    let label: String = raw?.trim().chars().take(64).collect();
    if label.is_empty() {
        None
    } else {
        Some(label)
    }
}

struct ModelUse {
    provider: String,
    model: String,
    requests: i64,
    tokens: i64,
    cost: f64,
}

fn push_model(models: &mut Vec<ModelUse>, provider: String, model: String, requests: i64, tokens: i64, cost: f64) {
    if let Some(found) = models.iter_mut().find(|m| m.provider == provider && m.model == model) {
        found.requests += requests;
        found.tokens += tokens;
        found.cost += cost;
        return;
    }
    models.push(ModelUse { provider, model, requests, tokens, cost });
}

fn model_json(models: &mut Vec<ModelUse>) -> Vec<Value> {
    models.sort_by(|a, b| b.tokens.cmp(&a.tokens).then(a.model.cmp(&b.model)).then(a.provider.cmp(&b.provider)));
    models.truncate(USAGE_MODEL_CAP);
    models
        .iter()
        .map(|m| json!({ "provider": m.provider, "model": m.model, "requests": m.requests, "tokens": m.tokens, "cost": m.cost }))
        .collect()
}

fn fold_agents(rows: Vec<(String, String, String, i64, i64, f64)>) -> Vec<Value> {
    let mut names: Vec<String> = Vec::new();
    let mut groups: HashMap<String, (i64, i64, f64, Vec<ModelUse>)> = HashMap::new();
    for (agent, provider, model, requests, tokens, cost) in rows {
        let entry = groups.entry(agent.clone()).or_insert_with(|| {
            names.push(agent);
            (0, 0, 0.0, Vec::new())
        });
        entry.0 += requests;
        entry.1 += tokens;
        entry.2 += cost;
        push_model(&mut entry.3, provider, model, requests, tokens, cost);
    }
    let mut ranked: Vec<_> = names
        .into_iter()
        .filter_map(|agent| {
            let (requests, tokens, cost, mut models) = groups.remove(&agent)?;
            let models = model_json(&mut models);
            Some((tokens, agent.clone(), json!({ "agent": agent, "requests": requests, "tokens": tokens, "cost": cost, "models": models })))
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    ranked.truncate(USAGE_GROUP_CAP);
    ranked.into_iter().map(|(_, _, row)| row).collect()
}

fn fold_teams(conn: &Connection, rows: Vec<(String, String, String, String, i64, i64, f64)>) -> Vec<Value> {
    struct TeamAcc {
        requests: i64,
        tokens: i64,
        cost: f64,
        agents: HashMap<String, (i64, i64, f64)>,
        models: Vec<ModelUse>,
    }
    let mut ids: Vec<String> = Vec::new();
    let mut groups: HashMap<String, TeamAcc> = HashMap::new();
    for (team, agent, provider, model, requests, tokens, cost) in rows {
        let entry = groups.entry(team.clone()).or_insert_with(|| {
            ids.push(team);
            TeamAcc { requests: 0, tokens: 0, cost: 0.0, agents: HashMap::new(), models: Vec::new() }
        });
        entry.requests += requests;
        entry.tokens += tokens;
        entry.cost += cost;
        if !agent.is_empty() {
            let speaker = entry.agents.entry(agent).or_insert((0, 0, 0.0));
            speaker.0 += requests;
            speaker.1 += tokens;
            speaker.2 += cost;
        }
        push_model(&mut entry.models, provider, model, requests, tokens, cost);
    }
    let mut ranked = Vec::new();
    for id in ids {
        let Some(team) = groups.remove(&id) else { continue };
        let name: String = conn.query_row("SELECT name FROM teams WHERE id = ?1", params![id], |r| r.get(0)).unwrap_or_else(|_| id.clone());
        ranked.push((team.tokens, id, name, team));
    }
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    ranked.truncate(USAGE_GROUP_CAP);
    ranked
        .into_iter()
        .map(|(_, id, name, mut team)| {
            let mut agents: Vec<_> = team.agents.into_iter().collect();
            agents.sort_by(|a, b| b.1 .1.cmp(&a.1 .1).then(a.0.cmp(&b.0)));
            agents.truncate(USAGE_GROUP_CAP);
            let agents: Vec<_> = agents
                .into_iter()
                .map(|(agent, (requests, tokens, cost))| json!({ "agent": agent, "requests": requests, "tokens": tokens, "cost": cost }))
                .collect();
            json!({
                "id": id,
                "name": name,
                "requests": team.requests,
                "tokens": team.tokens,
                "cost": team.cost,
                "agents": agents,
                "models": model_json(&mut team.models),
            })
        })
        .collect()
}
