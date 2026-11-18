use super::{now, query_rows, AccountRecord, GatewayStore};
use rusqlite::params;
use serde_json::{json, Value};
use uuid::Uuid;

impl GatewayStore {
    // ---- Accounts (OAuth) -------------------------------------------
    pub fn list_accounts(&self) -> Vec<AccountRecord> {
        let conn = self.conn.lock();
        let rows = query_rows(
            &conn,
            "SELECT id, provider, label, email, connected_at, refreshed_at, last_error, sort_order, secret, enabled FROM accounts ORDER BY provider, sort_order, connected_at",
            [],
            |row| {
                let id: String = row.get(0)?;
                let provider: String = row.get(1)?;
                let label: Option<String> = row.get(2)?;
                let email: Option<String> = row.get(3)?;
                let connected_at: f64 = row.get(4)?;
                let refreshed_at: Option<f64> = row.get(5)?;
                let last_error: Option<String> = row.get(6)?;
                let sort_order: i64 = row.get(7)?;
                let secret: String = row.get(8)?;
                let enabled: bool = row.get::<_, i64>(9).map(|v| v != 0).unwrap_or(true);
                Ok((id, provider, label, email, connected_at, refreshed_at, last_error, sort_order, secret, enabled))
            },
        );

        let mut out = Vec::new();
        for (id, provider, label, email, connected_at, refreshed_at, last_error, sort_order, secret, enabled) in rows {
            let cred = if let Ok(dec) = self.cipher.decrypt(&secret) {
                serde_json::from_str::<Value>(&dec).unwrap_or(json!({}))
            } else {
                json!({})
            };
            out.push(AccountRecord { id, provider, label, email, connected_at, refreshed_at, last_error, sort_order, enabled, cred });
        }
        out
    }

    pub fn save_account(&self, provider: &str, cred: &Value, label: Option<&str>, email: Option<&str>) -> Result<String, String> {
        self.update_or_save_account(provider, cred, label, email, None)
    }

    pub fn update_or_save_account(
        &self,
        provider: &str,
        cred: &Value,
        label: Option<&str>,
        email: Option<&str>,
        target_account_id: Option<&str>,
    ) -> Result<String, String> {
        let cred_str = serde_json::to_string(cred).map_err(|e| e.to_string())?;
        let encrypted = self.cipher.encrypt(&cred_str)?;
        let conn = self.conn.lock();

        if let Some(target_id) = target_account_id {
            let updated = conn
                .execute(
                    "UPDATE accounts SET secret = ?1, email = coalesce(?2, email), label = coalesce(?3, label), refreshed_at = ?4, last_error = NULL WHERE id = ?5",
                    params![encrypted, email, label, now(), target_id],
                )
                .unwrap_or(0);
            if updated > 0 {
                return Ok(target_id.to_string());
            }
        }

        if let Some(em) = email {
            let mut stmt = conn.prepare("SELECT id FROM accounts WHERE provider = ?1 AND email = ?2 LIMIT 1").map_err(|e| e.to_string())?;
            let existing_id: Option<String> = stmt.query_row(params![provider, em], |r| r.get(0)).ok();
            if let Some(eid) = existing_id {
                let _ = conn.execute(
                    "UPDATE accounts SET secret = ?1, label = coalesce(?2, label), refreshed_at = ?3, last_error = NULL WHERE id = ?4",
                    params![encrypted, label, now(), eid],
                );
                return Ok(eid);
            }
        }

        let id = format!("{}:{}", provider, &Uuid::new_v4().to_string()[..8]);
        conn.execute(
            "INSERT INTO accounts (id, provider, label, email, connected_at, secret) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, provider, label, email, now(), encrypted],
        )
        .map_err(|e| format!("Failed to insert account: {}", e))?;
        Ok(id)
    }

    pub fn record_account_error(&self, id: &str, err: Option<&str>) -> Result<(), String> {
        let conn = self.conn.lock();
        conn.execute("UPDATE accounts SET last_error = ?1 WHERE id = ?2", params![err, id]).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn remove_account(&self, provider: &str, account_id: Option<&str>) -> Result<bool, String> {
        let conn = self.conn.lock();
        let rows = if let Some(id) = account_id {
            conn.execute("DELETE FROM accounts WHERE provider = ?1 AND id = ?2", params![provider, id])
        } else {
            conn.execute("DELETE FROM accounts WHERE provider = ?1", params![provider])
        }
        .map_err(|e| e.to_string())?;
        Ok(rows > 0)
    }

    pub fn reorder_accounts(&self, provider: &str, account_ids: &[String]) -> Result<(), String> {
        let conn = self.conn.lock();
        for (i, id) in account_ids.iter().enumerate() {
            let _ = conn.execute("UPDATE accounts SET sort_order = ?1 WHERE provider = ?2 AND id = ?3", params![i as i64, provider, id]);
        }
        Ok(())
    }

    pub fn set_account_enabled(&self, provider: &str, account_id: &str, enabled: Option<bool>) -> Result<bool, String> {
        let conn = self.conn.lock();
        let current: i64 = conn
            .query_row("SELECT enabled FROM accounts WHERE provider = ?1 AND id = ?2", params![provider, account_id], |r| r.get(0))
            .map_err(|_| format!("Account '{}' for provider '{}' not found", account_id, provider))?;
        let next = enabled.unwrap_or(current == 0);
        conn.execute(
            "UPDATE accounts SET enabled = ?1 WHERE provider = ?2 AND id = ?3",
            params![if next { 1 } else { 0 }, provider, account_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(next)
    }

    // ---- Cooldowns -------------------------------------------------
    pub fn cool_down(&self, key: &str, secs: u64) {
        self.cooldowns.lock().insert(key.to_string(), now() + secs as f64);
    }

    pub fn clear_cooldown(&self, key: &str) {
        self.cooldowns.lock().remove(key);
    }

    pub fn cooling_until(&self, key: &str) -> Option<f64> {
        self.cooldowns.lock().get(key).copied().filter(|t| *t > now())
    }
}
