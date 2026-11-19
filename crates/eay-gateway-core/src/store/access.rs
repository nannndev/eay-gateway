use super::{
    hash_key, now, query_rows, AgentProfile, GatewaySettings, GatewayStore, McpServerRecord, SkillRecord, TeamRecord, VirtualKeyRecord,
    KEY_PREFIX,
};
use rusqlite::params;
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;

impl GatewayStore {
    // ---- Combos -----------------------------------------------------
    pub fn list_combos(&self) -> HashMap<String, Vec<String>> {
        let conn = self.conn.lock();
        query_rows(&conn, "SELECT name, targets FROM combos", [], |row| {
            let name: String = row.get(0)?;
            let targets_json: String = row.get(1)?;
            let targets: Vec<String> = serde_json::from_str(&targets_json).unwrap_or_default();
            Ok((name, targets))
        })
        .into_iter()
        .collect()
    }

    pub fn save_combo(&self, name: &str, targets: &[String]) -> Result<(), String> {
        let targets_json = serde_json::to_string(targets).map_err(|e| e.to_string())?;
        let conn = self.conn.lock();
        conn.execute("INSERT OR REPLACE INTO combos (name, targets) VALUES (?1, ?2)", params![name, targets_json])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn delete_combo(&self, name: &str) -> Result<bool, String> {
        let conn = self.conn.lock();
        let count = conn.execute("DELETE FROM combos WHERE name = ?1", params![name]).map_err(|e| e.to_string())?;
        Ok(count > 0)
    }

    // ---- Virtual Keys ----------------------------------------------
    pub fn list_virtual_keys(&self) -> Vec<VirtualKeyRecord> {
        let conn = self.conn.lock();
        query_rows(
            &conn,
            "SELECT id, name, key_prefix, enabled, allowed_providers, rate_limit_per_min, created_at, last_used_at, total_requests, total_input_tokens, total_output_tokens, default_skills, max_tokens_budget, max_cost_usd, max_concurrent, tool_policy FROM keys ORDER BY created_at DESC",
            [],
            |r| {
                let allowed_providers_json: Option<String> = r.get(4)?;
                let allowed_providers = allowed_providers_json.and_then(|j| serde_json::from_str(&j).ok());
                let default_skills_json: Option<String> = r.get(11)?;
                let default_skills = default_skills_json.and_then(|j| serde_json::from_str(&j).ok());
                Ok(VirtualKeyRecord {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    key_prefix: r.get(2)?,
                    enabled: r.get::<_, i64>(3)? != 0,
                    allowed_providers,
                    rate_limit_per_min: r.get(5)?,
                    max_tokens_budget: r.get(12)?,
                    max_cost_usd: r.get(13)?,
                    default_skills,
                    max_concurrent: r.get(14)?,
                    tool_policy: r.get::<_, Option<String>>(15)?.and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default(),
                    created_at: r.get(6)?,
                    last_used_at: r.get(7)?,
                    total_requests: r.get(8)?,
                    total_input_tokens: r.get(9)?,
                    total_output_tokens: r.get(10)?,
                })
            },
        )
    }

    pub fn create_virtual_key(
        &self,
        name: &str,
        allowed_providers: Option<Vec<String>>,
        rate_limit_per_min: Option<i64>,
    ) -> Result<(VirtualKeyRecord, String), String> {
        let id = Uuid::new_v4().to_string();
        let random_hex: String = (0..32).map(|_| format!("{:x}", rand::random::<u8>() % 16)).collect();
        let secret = format!("{}{}", KEY_PREFIX, random_hex);
        let key_hash = hash_key(&secret);
        let key_prefix = format!("{}...", &secret[..std::cmp::min(12, secret.len())]);
        let created_at = now();
        let allowed_json = allowed_providers.as_ref().and_then(|p| serde_json::to_string(p).ok());

        let conn = self.conn.lock();
        conn.execute(
            r#"
            INSERT INTO keys (id, name, key_hash, key_prefix, enabled, allowed_providers, rate_limit_per_min, created_at)
            VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?7)
            "#,
            params![id, name, key_hash, key_prefix, allowed_json, rate_limit_per_min, created_at],
        )
        .map_err(|e| e.to_string())?;

        let record = VirtualKeyRecord {
            id,
            name: name.to_string(),
            key_prefix,
            enabled: true,
            allowed_providers,
            rate_limit_per_min,
            max_tokens_budget: None,
            max_cost_usd: None,
            default_skills: None,
            max_concurrent: None,
            tool_policy: Default::default(),
            created_at,
            last_used_at: None,
            total_requests: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
        };
        Ok((record, secret))
    }

    pub fn set_virtual_key_limits(&self, id: &str, max_tokens_budget: Option<i64>, max_cost_usd: Option<f64>) -> Result<(), String> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE keys SET max_tokens_budget = ?1, max_cost_usd = ?2 WHERE id = ?3",
            params![max_tokens_budget, max_cost_usd, id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn set_virtual_key_tool_policy(&self, id: &str, policy: &crate::mcp::ToolPolicy) -> Result<(), String> {
        policy.validate()?;
        let json = if policy.is_empty() { None } else { Some(serde_json::to_string(policy).map_err(|e| e.to_string())?) };
        let conn = self.conn.lock();
        conn.execute("UPDATE keys SET tool_policy = ?1 WHERE id = ?2", params![json, id]).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn set_virtual_key_concurrency(&self, id: &str, max_concurrent: Option<i64>) -> Result<(), String> {
        let conn = self.conn.lock();
        conn.execute("UPDATE keys SET max_concurrent = ?1 WHERE id = ?2", params![max_concurrent.filter(|n| *n > 0), id])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn key_total_cost(&self, id: &str) -> f64 {
        let conn = self.conn.lock();
        conn.query_row("SELECT COALESCE(SUM(cost), 0.0) FROM usage WHERE key_id = ?1", params![id], |r| r.get(0)).unwrap_or(0.0)
    }

    pub fn set_virtual_key_skills(&self, id: &str, skills: Option<Vec<String>>) -> Result<(), String> {
        let json = skills.as_ref().and_then(|s| serde_json::to_string(s).ok());
        let conn = self.conn.lock();
        conn.execute("UPDATE keys SET default_skills = ?1 WHERE id = ?2", params![json, id]).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn toggle_virtual_key(&self, id: &str) -> Result<VirtualKeyRecord, String> {
        let conn = self.conn.lock();
        conn.execute("UPDATE keys SET enabled = 1 - enabled WHERE id = ?1", params![id]).map_err(|e| e.to_string())?;
        drop(conn);
        self.list_virtual_keys().into_iter().find(|k| k.id == id).ok_or_else(|| "Key not found".to_string())
    }

    pub fn delete_virtual_key(&self, id: &str) -> Result<bool, String> {
        let conn = self.conn.lock();
        let rows = conn.execute("DELETE FROM keys WHERE id = ?1", params![id]).map_err(|e| e.to_string())?;
        Ok(rows > 0)
    }

    /// Looks up a virtual key by its plaintext secret.
    pub fn find_virtual_key(&self, secret: &str) -> Option<VirtualKeyRecord> {
        let id: String = {
            let conn = self.conn.lock();
            conn.query_row("SELECT id FROM keys WHERE key_hash = ?1", params![hash_key(secret)], |r| r.get(0)).ok()?
        };
        self.list_virtual_keys().into_iter().find(|k| k.id == id)
    }

    pub fn key_requests_since(&self, id: &str, since: f64) -> i64 {
        let conn = self.conn.lock();
        conn.query_row("SELECT COUNT(*) FROM usage WHERE key_id = ?1 AND ts >= ?2", params![id, since], |r| r.get(0)).unwrap_or(0)
    }

    pub fn touch_virtual_key(&self, id: &str, input_tokens: i64, output_tokens: i64) {
        let conn = self.conn.lock();
        let _ = conn.execute(
            "UPDATE keys SET last_used_at = ?1, total_requests = total_requests + 1, total_input_tokens = total_input_tokens + ?2, total_output_tokens = total_output_tokens + ?3 WHERE id = ?4",
            params![now(), input_tokens, output_tokens, id],
        );
    }

    // ---- Skills (Context Presets) ----------------------------------
    pub fn list_skills(&self) -> Vec<SkillRecord> {
        let conn = self.conn.lock();
        query_rows(&conn, "SELECT name, description, prompt, enabled, updated_at FROM skills ORDER BY name ASC", [], |r| {
            Ok(SkillRecord {
                name: r.get(0)?,
                description: r.get(1)?,
                prompt: r.get(2)?,
                enabled: r.get::<_, i64>(3)? != 0,
                updated_at: r.get(4)?,
            })
        })
    }

    pub fn get_skill(&self, name: &str) -> Option<SkillRecord> {
        let conn = self.conn.lock();
        conn.query_row("SELECT name, description, prompt, enabled, updated_at FROM skills WHERE name = ?1", params![name], |r| {
            Ok(SkillRecord {
                name: r.get(0)?,
                description: r.get(1)?,
                prompt: r.get(2)?,
                enabled: r.get::<_, i64>(3)? != 0,
                updated_at: r.get(4)?,
            })
        })
        .ok()
    }

    pub fn save_skill(&self, name: &str, description: &str, prompt: &str, enabled: bool) -> Result<SkillRecord, String> {
        let name = name.trim();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err("Skill name must be non-empty and use only letters, digits, '-' or '_'".into());
        }
        let prompt_trimmed = prompt.trim();
        if prompt_trimmed.is_empty() {
            return Err("Skill prompt cannot be empty".into());
        }
        let updated_at = now();
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO skills (name, description, prompt, enabled, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![name, description.trim(), prompt_trimmed, i64::from(enabled), updated_at],
        )
        .map_err(|e| e.to_string())?;
        Ok(SkillRecord {
            name: name.to_string(),
            description: description.trim().to_string(),
            prompt: prompt_trimmed.to_string(),
            enabled,
            updated_at,
        })
    }

    pub fn toggle_skill(&self, name: &str) -> Result<SkillRecord, String> {
        let conn = self.conn.lock();
        conn.execute("UPDATE skills SET enabled = 1 - enabled, updated_at = ?1 WHERE name = ?2", params![now(), name])
            .map_err(|e| e.to_string())?;
        drop(conn);
        self.get_skill(name).ok_or_else(|| "Skill not found".to_string())
    }

    pub fn delete_skill(&self, name: &str) -> Result<bool, String> {
        let conn = self.conn.lock();
        let rows = conn.execute("DELETE FROM skills WHERE name = ?1", params![name]).map_err(|e| e.to_string())?;
        Ok(rows > 0)
    }

    // ---- Agents (Personality Profiles) -----------------------------
    pub fn list_agents(&self) -> Vec<AgentProfile> {
        let conn = self.conn.lock();
        query_rows(
            &conn,
            "SELECT name, description, model, skills, personality, temperature, max_tokens, enabled, updated_at, tool_policy, mcp FROM agents ORDER BY name ASC",
            [],
            |r| {
                let skills_json: String = r.get(3)?;
                let skills: Vec<String> = serde_json::from_str(&skills_json).unwrap_or_default();
                let max_tokens: Option<i64> = r.get(6)?;
                Ok(AgentProfile {
                    name: r.get(0)?,
                    description: r.get(1)?,
                    model: r.get(2)?,
                    skills,
                    mcp: serde_json::from_str(&r.get::<_, String>(10)?).unwrap_or_default(),
                    personality: r.get(4)?,
                    temperature: r.get(5)?,
                    max_tokens: max_tokens.and_then(|v| u64::try_from(v).ok()),
                    tool_policy: r.get::<_, Option<String>>(9)?.and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default(),
                    enabled: r.get::<_, i64>(7)? != 0,
                    updated_at: r.get(8)?,
                })
            },
        )
    }

    pub fn get_agent(&self, name: &str) -> Option<AgentProfile> {
        self.list_agents().into_iter().find(|a| a.name == name)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn save_agent(
        &self,
        name: &str,
        description: &str,
        model: &str,
        skills: &[String],
        personality: &str,
        temperature: Option<f64>,
        max_tokens: Option<u64>,
        tool_policy: &crate::mcp::ToolPolicy,
        enabled: bool,
        mcp: &[String],
    ) -> Result<AgentProfile, String> {
        tool_policy.validate()?;
        let mcp = clean_mcp_names(mcp)?;
        let policy_json = if tool_policy.is_empty() { None } else { Some(serde_json::to_string(tool_policy).map_err(|e| e.to_string())?) };
        let name = name.trim();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err("Agent name must be non-empty and use only letters, digits, '-' or '_'".into());
        }
        let model = if model.trim().is_empty() { "auto" } else { model.trim() };
        let skills_json = serde_json::to_string(skills).map_err(|e| e.to_string())?;
        let mcp_json = serde_json::to_string(&mcp).map_err(|e| e.to_string())?;
        let updated_at = now();
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO agents (name, description, model, skills, personality, temperature, max_tokens, enabled, updated_at, tool_policy, mcp) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                name,
                description.trim(),
                model,
                skills_json,
                personality.trim(),
                temperature,
                max_tokens.map(|v| v as i64),
                i64::from(enabled),
                updated_at,
                policy_json,
                mcp_json,
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(AgentProfile {
            name: name.to_string(),
            description: description.trim().to_string(),
            model: model.to_string(),
            skills: skills.to_vec(),
            mcp,
            personality: personality.trim().to_string(),
            temperature,
            max_tokens,
            tool_policy: tool_policy.clone(),
            enabled,
            updated_at,
        })
    }

    pub fn delete_agent(&self, name: &str) -> Result<bool, String> {
        let conn = self.conn.lock();
        let rows = conn.execute("DELETE FROM agents WHERE name = ?1", params![name]).map_err(|e| e.to_string())?;
        Ok(rows > 0)
    }

    // ---- Teams (a roster of agents the user assembled) -------------
    pub fn list_teams(&self) -> Vec<TeamRecord> {
        let conn = self.conn.lock();
        query_rows(&conn, "SELECT id, name, lead, members, updated_at FROM teams ORDER BY name ASC, id ASC", [], |r| {
            Ok(TeamRecord {
                id: r.get(0)?,
                name: r.get(1)?,
                lead: r.get(2)?,
                members: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default(),
                updated_at: r.get(4)?,
            })
        })
    }

    pub fn get_team(&self, id: &str) -> Option<TeamRecord> {
        self.list_teams().into_iter().find(|t| t.id == id)
    }

    /// Saves a roster. `id` of `None` creates `tm_…`. An existing id is updated, so the name can
    /// change without breaking sessions. Every member must already be a saved agent. An empty
    /// `lead` uses the first member. Names may repeat; the id is what a session stores.
    pub fn save_team(&self, id: Option<&str>, name: &str, lead: &str, members: &[String]) -> Result<TeamRecord, String> {
        let name = name.trim();
        if name.is_empty() || name.len() > 64 || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err("Team name must be non-empty and use only letters, digits, '-' or '_'".into());
        }
        let mut roster = Vec::new();
        for raw in members {
            let member = raw.trim();
            if member.is_empty() || roster.iter().any(|m: &String| m == member) {
                continue;
            }
            if self.get_agent(member).is_none() {
                return Err(format!("No agent named '{member}'"));
            }
            if roster.len() == 12 {
                return Err("A team can have at most 12 agents".into());
            }
            roster.push(member.to_string());
        }
        if roster.is_empty() {
            return Err("A team needs at least one agent".into());
        }
        let lead = if lead.trim().is_empty() { roster[0].clone() } else { lead.trim().to_string() };
        if !roster.iter().any(|m| m == &lead) {
            return Err("The lead must be one of the team's agents".into());
        }
        let id = match id.map(str::trim).filter(|s| !s.is_empty()) {
            Some(existing) => {
                if self.get_team(existing).is_none() {
                    return Err(format!("Team '{existing}' not found"));
                }
                existing.to_string()
            }
            None => format!("tm_{}", &Uuid::new_v4().simple().to_string()[..12]),
        };
        let members_json = serde_json::to_string(&roster).map_err(|e| e.to_string())?;
        let updated_at = now();
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO teams (id, name, lead, members, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, name, lead, members_json, updated_at],
        )
        .map_err(|e| e.to_string())?;
        Ok(TeamRecord { id, name: name.to_string(), lead, members: roster, updated_at })
    }

    pub fn delete_team(&self, id: &str) -> Result<bool, String> {
        let conn = self.conn.lock();
        let rows = conn.execute("DELETE FROM teams WHERE id = ?1", params![id]).map_err(|e| e.to_string())?;
        Ok(rows > 0)
    }

    // ---- Sessions (Conversation Memory) ----------------------------
    pub fn get_session_messages(&self, id: &str) -> Vec<Value> {
        let conn = self.conn.lock();
        let raw: Option<String> = conn.query_row("SELECT messages FROM sessions WHERE id = ?1", params![id], |r| r.get(0)).ok();
        raw.and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default()
    }

    pub fn save_session_messages(&self, id: &str, messages: &[Value]) -> Result<(), String> {
        // Keep last 40 turns to prevent unbounded growth in SQLite.
        let trimmed = if messages.len() > 40 { &messages[messages.len() - 40..] } else { messages };
        let json = serde_json::to_string(trimmed).map_err(|e| e.to_string())?;
        let conn = self.conn.lock();
        // Keep the roster binding. INSERT OR REPLACE would otherwise reset the new `team` column.
        conn.execute(
            "INSERT OR REPLACE INTO sessions (id, messages, updated_at, team) VALUES (?1, ?2, ?3, (SELECT team FROM sessions WHERE id = ?1))",
            params![id, json, now()],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// The roster this session uses, if one was bound with `x-eay-team`.
    pub fn session_team(&self, id: &str) -> Option<String> {
        let conn = self.conn.lock();
        conn.query_row("SELECT team FROM sessions WHERE id = ?1", params![id], |r| r.get::<_, Option<String>>(0)).ok().flatten()
    }

    /// Remembers which roster a session uses. Creates the session row when the chat has not started yet.
    pub fn set_session_team(&self, id: &str, team: &str) -> Result<(), String> {
        let conn = self.conn.lock();
        let updated = conn.execute("UPDATE sessions SET team = ?2 WHERE id = ?1", params![id, team]).map_err(|e| e.to_string())?;
        if updated == 0 {
            conn.execute("INSERT INTO sessions (id, messages, updated_at, team) VALUES (?1, '[]', ?2, ?3)", params![id, now(), team])
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Sessions, newest first: storage id, message count and last update. Message bodies are not
    /// loaded, so listing stays cheap however long the conversations are.
    pub fn list_sessions(&self, limit: usize) -> Vec<Value> {
        let conn = self.conn.lock();
        query_rows(
            &conn,
            "SELECT id, json_array_length(messages), updated_at, team FROM sessions ORDER BY updated_at DESC LIMIT ?1",
            params![limit as i64],
            |r| {
                let id: String = r.get(0)?;
                // `key:<key-id>:<sid>` or `local:<sid>`; older ids have no prefix.
                let (owner, name) = match id.split_once(':') {
                    Some(("local", rest)) => ("local".to_string(), rest.to_string()),
                    Some(("key", rest)) => match rest.split_once(':') {
                        Some((k, s)) => (format!("key:{k}"), s.to_string()),
                        None => ("unknown".to_string(), rest.to_string()),
                    },
                    _ => ("legacy".to_string(), id.clone()),
                };
                Ok(serde_json::json!({
                    "id": id, "session": name, "owner": owner,
                    "messages": r.get::<_, i64>(1)?, "updated_at": r.get::<_, f64>(2)?,
                    "team": r.get::<_, Option<String>>(3)?,
                }))
            },
        )
    }

    pub fn session_updated_at(&self, id: &str) -> Option<f64> {
        let conn = self.conn.lock();
        conn.query_row("SELECT updated_at FROM sessions WHERE id = ?1", params![id], |r| r.get(0)).ok()
    }

    pub fn delete_session(&self, id: &str) -> Result<bool, String> {
        let conn = self.conn.lock();
        let rows = conn.execute("DELETE FROM sessions WHERE id = ?1", params![id]).map_err(|e| e.to_string())?;
        Ok(rows > 0)
    }

    // ---- Subagent run history ------------------------------------------
    /// Keeps the newest 500 finished runs; enough history without unbounded growth.
    pub fn save_subagent_run(&self, run: &crate::engine::SubagentRun) {
        let Ok(data) = serde_json::to_string(run) else { return };
        let conn = self.conn.lock();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO subagent_runs (id, data, updated_at) VALUES (?1, ?2, ?3)",
            params![run.id, data, run.updated_at],
        );
        let _ =
            conn.execute("DELETE FROM subagent_runs WHERE id NOT IN (SELECT id FROM subagent_runs ORDER BY updated_at DESC LIMIT 500)", []);
    }

    pub fn list_subagent_runs(&self, limit: usize) -> Vec<crate::engine::SubagentRun> {
        let conn = self.conn.lock();
        let Ok(mut stmt) = conn.prepare("SELECT data FROM subagent_runs ORDER BY updated_at DESC LIMIT ?1") else { return Vec::new() };
        stmt.query_map(params![limit as i64], |r| r.get::<_, String>(0))
            .map(|rows| rows.filter_map(Result::ok).filter_map(|d| serde_json::from_str(&d).ok()).collect())
            .unwrap_or_default()
    }

    pub fn delete_subagent_run(&self, id: &str) {
        let _ = self.conn.lock().execute("DELETE FROM subagent_runs WHERE id = ?1", params![id]);
    }

    // ---- External MCP Servers --------------------------------------
    pub fn list_mcp_servers(&self) -> Vec<McpServerRecord> {
        let conn = self.conn.lock();
        query_rows(
            &conn,
            "SELECT name, transport, command, args, env, url, enabled, updated_at, headers FROM mcp_servers ORDER BY name ASC",
            [],
            |r| {
                let args_json: String = r.get(3)?;
                let env_json: String = r.get(4)?;
                let headers_json: String = r.get(8).unwrap_or_else(|_| "{}".into());
                Ok(McpServerRecord {
                    name: r.get(0)?,
                    transport: r.get(1)?,
                    command: r.get(2)?,
                    args: serde_json::from_str(&args_json).unwrap_or_default(),
                    env: serde_json::from_str(&env_json).unwrap_or_default(),
                    url: r.get(5)?,
                    headers: serde_json::from_str(&headers_json).unwrap_or_default(),
                    enabled: r.get::<_, i64>(6)? != 0,
                    updated_at: r.get(7)?,
                })
            },
        )
    }

    pub fn get_mcp_server(&self, name: &str) -> Option<McpServerRecord> {
        self.list_mcp_servers().into_iter().find(|m| m.name == name)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn save_mcp_server(
        &self,
        name: &str,
        transport: &str,
        command: Option<&str>,
        args: &[String],
        env: &HashMap<String, String>,
        url: Option<&str>,
        headers: &HashMap<String, String>,
        enabled: bool,
    ) -> Result<McpServerRecord, String> {
        let name = name.trim();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err("MCP server name must be non-empty and use only letters, digits, '-' or '_'".into());
        }
        let transport = match transport.trim() {
            "stdio" | "http" => transport.trim(),
            other => return Err(format!("Unsupported MCP transport '{other}'; use 'stdio' or 'http'")),
        };
        if transport == "stdio" && command.map(str::trim).unwrap_or("").is_empty() {
            return Err("MCP stdio server requires 'command'".into());
        }
        if transport == "http" && url.map(str::trim).unwrap_or("").is_empty() {
            return Err("MCP http server requires 'url'".into());
        }
        let args_json = serde_json::to_string(args).map_err(|e| e.to_string())?;
        let env_json = serde_json::to_string(env).map_err(|e| e.to_string())?;
        let headers_json = serde_json::to_string(headers).map_err(|e| e.to_string())?;
        let updated_at = now();
        let cmd_clean = command.map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        let url_clean = url.map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO mcp_servers (name, transport, command, args, env, url, headers, enabled, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![name, transport, cmd_clean, args_json, env_json, url_clean, headers_json, i64::from(enabled), updated_at],
        )
        .map_err(|e| e.to_string())?;
        Ok(McpServerRecord {
            name: name.to_string(),
            transport: transport.to_string(),
            command: cmd_clean,
            args: args.to_vec(),
            env: env.clone(),
            url: url_clean,
            headers: headers.clone(),
            enabled,
            updated_at,
        })
    }

    pub fn toggle_mcp_server(&self, name: &str) -> Result<McpServerRecord, String> {
        let conn = self.conn.lock();
        conn.execute("UPDATE mcp_servers SET enabled = 1 - enabled, updated_at = ?1 WHERE name = ?2", params![now(), name])
            .map_err(|e| e.to_string())?;
        drop(conn);
        self.get_mcp_server(name).ok_or_else(|| "MCP server not found".to_string())
    }

    pub fn delete_mcp_server(&self, name: &str) -> Result<bool, String> {
        let conn = self.conn.lock();
        let rows = conn.execute("DELETE FROM mcp_servers WHERE name = ?1", params![name]).map_err(|e| e.to_string())?;
        Ok(rows > 0)
    }

    // ---- Settings --------------------------------------------------
    pub fn get_setting(&self, key: &str) -> Option<String> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT value FROM settings WHERE key = ?1").ok()?;
        stmt.query_row(params![key], |r| r.get(0)).ok()
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), String> {
        let conn = self.conn.lock();
        conn.execute("INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)", params![key, value]).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_settings(&self) -> GatewaySettings {
        self.get_gateway_settings()
    }

    pub fn get_gateway_settings(&self) -> GatewaySettings {
        let strategy = self.get_setting("routing_strategy").unwrap_or_default();
        let client_id = self.get_setting("gemini_oauth_client_id").unwrap_or_default();
        let client_secret = self.get_setting("gemini_oauth_client_secret");
        let secret_masked = client_secret.as_ref().filter(|s| !s.is_empty()).map(|s| {
            if s.len() > 4 {
                format!("••••{}", &s[s.len() - 4..])
            } else {
                "••••".to_string()
            }
        });
        let configured = !client_id.is_empty() && client_secret.is_some();
        let routing_strategy = match strategy.as_str() {
            "round_robin" | "fill_first" => strategy,
            _ => "priority".to_string(),
        };
        GatewaySettings {
            routing_strategy,
            fallback_order: if self.get_setting("fallback_order").as_deref() == Some("account_first") {
                "account_first".into()
            } else {
                "model_first".into()
            },
            provider_priority: self.get_setting("provider_priority").and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default(),
            auto_models_per_provider: self
                .get_setting("auto_models_per_provider")
                .and_then(|v| v.parse().ok())
                .filter(|n| *n > 0)
                .unwrap_or(2),
            cooldown_secs: self.get_setting("cooldown_secs").and_then(|v| v.parse().ok()).unwrap_or(60),
            gemini_oauth_client_id: client_id,
            gemini_oauth_client_secret_masked: secret_masked,
            gemini_oauth_configured: configured,
        }
    }
}

/// Names of MCP servers an agent may use. `*` means every enabled server. Duplicates are dropped.
fn clean_mcp_names(names: &[String]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for raw in names {
        let name = raw.trim();
        if name.is_empty() {
            continue;
        }
        if name != "*" && !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(format!("MCP server name '{name}' must use only letters, digits, '-' or '_'"));
        }
        if out.iter().any(|s: &String| s == name) {
            continue;
        }
        if out.len() == 20 {
            return Err("An agent can list at most 20 MCP servers".into());
        }
        out.push(name.to_string());
    }
    Ok(out)
}
