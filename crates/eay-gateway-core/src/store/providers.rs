use super::{now, query_rows, GatewayStore, ProviderRecord};
use rusqlite::params;

fn map_provider_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProviderRecord> {
    let id: String = row.get(0)?;
    let enabled: bool = row.get(1)?;
    let api_key: Option<String> = row.get(2)?;
    let base_url: Option<String> = row.get(3)?;
    let default_model: Option<String> = row.get(4)?;
    let models_json: Option<String> = row.get(5)?;
    let verified_at: Option<f64> = row.get(6)?;
    let latency_ms: Option<i64> = row.get(7)?;
    let error: Option<String> = row.get(8)?;
    let models = models_json.and_then(|j| serde_json::from_str::<Vec<String>>(&j).ok()).unwrap_or_default();

    Ok(ProviderRecord {
        id,
        enabled,
        base_url,
        default_model,
        models,
        verified_at,
        latency_ms,
        error,
        has_key: api_key.is_some_and(|k| !k.is_empty()),
    })
}

impl GatewayStore {
    // ---- Providers (API Keys) ---------------------------------------
    pub fn list_providers(&self) -> Vec<ProviderRecord> {
        let conn = self.conn.lock();
        query_rows(
            &conn,
            "SELECT id, enabled, api_key, base_url, default_model, models, verified_at, latency_ms, error FROM providers",
            [],
            map_provider_row,
        )
    }

    pub fn save_provider_key(&self, provider_id: &str, raw_key: &str) -> Result<(), String> {
        let encrypted = self.cipher.encrypt(raw_key)?;
        let conn = self.conn.lock();
        conn.execute(
            r#"
            INSERT INTO providers (id, api_key, enabled) VALUES (?1, ?2, 1)
            ON CONFLICT(id) DO UPDATE SET api_key = ?2, enabled = 1
            "#,
            params![provider_id, encrypted],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_provider_key(&self, provider_id: &str) -> Option<String> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT api_key FROM providers WHERE id = ?1").ok()?;
        let enc: Option<String> = stmt.query_row(params![provider_id], |r| r.get(0)).ok()?;
        enc.and_then(|blob| self.cipher.decrypt(&blob).ok())
    }

    pub fn get_provider_record(&self, provider_id: &str) -> Option<ProviderRecord> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, enabled, api_key, base_url, default_model, models, verified_at, latency_ms, error FROM providers WHERE id = ?1",
            )
            .ok()?;
        stmt.query_row(params![provider_id], map_provider_row).ok()
    }

    pub fn set_provider_verification(&self, provider_id: &str, latency_ms: Option<i64>, error: Option<&str>) -> Result<(), String> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE providers SET latency_ms = ?1, error = ?2, verified_at = ?3 WHERE id = ?4",
            params![latency_ms, error, now(), provider_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn set_synced_models(&self, provider_id: &str, models: &[String]) -> Result<(), String> {
        let json_str = serde_json::to_string(models).map_err(|e| e.to_string())?;
        let conn = self.conn.lock();
        conn.execute(
            r#"
            INSERT INTO providers (id, enabled, synced_models, models_synced_at) VALUES (?1, 1, ?2, ?3)
            ON CONFLICT(id) DO UPDATE SET synced_models = ?2, models_synced_at = ?3
            "#,
            params![provider_id, json_str, now()],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Models last fetched from the provider (empty if never synced) and when.
    pub fn get_synced_models(&self, provider_id: &str) -> (Vec<String>, Option<f64>) {
        let conn = self.conn.lock();
        conn.query_row("SELECT synced_models, models_synced_at FROM providers WHERE id = ?1", params![provider_id], |r| {
            Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<f64>>(1)?))
        })
        .map(|(j, at)| (j.and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default(), at))
        .unwrap_or_default()
    }

    /// What pickers show: the user's custom models, then the provider's live list
    /// (or the built-in seeds when it was never synced).
    pub fn display_models(&self, provider_id: &str, seed: &[&str]) -> Vec<String> {
        let mut out = self.get_provider_custom_models(provider_id);
        let (synced, _) = self.get_synced_models(provider_id);
        let base: Vec<String> = if synced.is_empty() { seed.iter().map(|s| s.to_string()).collect() } else { synced };
        for m in base {
            if !out.contains(&m) {
                out.push(m);
            }
        }
        out
    }

    // ---- Provider Connect / Disconnect -----------------------------
    pub fn connect_provider(&self, provider_id: &str, api_key: &str, base_url: Option<&str>) -> Result<(), String> {
        let encrypted = self.cipher.encrypt(api_key)?;
        let conn = self.conn.lock();
        conn.execute(
            r#"
            INSERT INTO providers (id, api_key, base_url, enabled, verified_at)
            VALUES (?1, ?2, ?3, 1, ?4)
            ON CONFLICT(id) DO UPDATE SET api_key = ?2, base_url = coalesce(?3, base_url), enabled = 1, verified_at = ?4
            "#,
            params![provider_id, encrypted, base_url, now()],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn disconnect_provider(&self, provider_id: &str) -> Result<(), String> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE providers SET api_key = NULL, enabled = 0, synced_models = NULL, models_synced_at = NULL WHERE id = ?1",
            params![provider_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn is_provider_enabled(&self, provider_id: &str) -> bool {
        let conn = self.conn.lock();
        conn.query_row("SELECT enabled FROM providers WHERE id = ?1", params![provider_id], |r| r.get::<_, i64>(0))
            .map(|v| v != 0)
            .unwrap_or(true)
    }

    pub fn set_provider_enabled(&self, provider_id: &str, enabled: Option<bool>) -> Result<bool, String> {
        let conn = self.conn.lock();
        let current: Option<i64> = conn.query_row("SELECT enabled FROM providers WHERE id = ?1", params![provider_id], |r| r.get(0)).ok();
        let next = enabled.unwrap_or_else(|| current.map(|v| v == 0).unwrap_or(false));
        let val = if next { 1 } else { 0 };
        conn.execute(
            r#"
            INSERT INTO providers (id, enabled) VALUES (?1, ?2)
            ON CONFLICT(id) DO UPDATE SET enabled = ?2
            "#,
            params![provider_id, val],
        )
        .map_err(|e| e.to_string())?;
        Ok(next)
    }

    pub fn get_provider_custom_models(&self, provider_id: &str) -> Vec<String> {
        let conn = self.conn.lock();
        let raw: Option<String> =
            conn.query_row("SELECT models FROM providers WHERE id = ?1", params![provider_id], |r| r.get(0)).ok().flatten();
        raw.and_then(|j| serde_json::from_str::<Vec<String>>(&j).ok()).unwrap_or_default()
    }

    pub fn set_provider_models(&self, provider_id: &str, models: &[String]) -> Result<Vec<String>, String> {
        let mut cleaned = Vec::new();
        for m in models {
            let trimmed = m.trim();
            if !trimmed.is_empty() && !cleaned.iter().any(|x| x == trimmed) {
                cleaned.push(trimmed.to_string());
            }
        }
        let json_str = serde_json::to_string(&cleaned).map_err(|e| e.to_string())?;
        let conn = self.conn.lock();
        conn.execute(
            r#"
            INSERT INTO providers (id, enabled, models) VALUES (?1, 1, ?2)
            ON CONFLICT(id) DO UPDATE SET models = ?2
            "#,
            params![provider_id, json_str],
        )
        .map_err(|e| e.to_string())?;
        Ok(cleaned)
    }

    pub fn add_provider_model(&self, provider_id: &str, model: &str) -> Result<Vec<String>, String> {
        let trimmed = model.trim();
        if trimmed.is_empty() {
            return Err("Model name cannot be empty".into());
        }
        let mut current = self.get_provider_custom_models(provider_id);
        if !current.iter().any(|m| m == trimmed) {
            current.push(trimmed.to_string());
        }
        self.set_provider_models(provider_id, &current)
    }

    pub fn remove_provider_model(&self, provider_id: &str, model: &str) -> Result<Vec<String>, String> {
        let target = model.trim();
        let mut current = self.get_provider_custom_models(provider_id);
        current.retain(|m| m != target);
        self.set_provider_models(provider_id, &current)
    }
}
