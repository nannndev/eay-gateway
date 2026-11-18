mod access;
mod accounts;
mod models;
mod providers;
mod usage;

#[cfg(test)]
mod tests;

pub use models::{AccountRecord, AgentProfile, GatewaySettings, McpServerRecord, ProviderRecord, SkillRecord, UsageEntry, VirtualKeyRecord};

use crate::cipher::Cipher;
use parking_lot::Mutex;
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub const KEY_PREFIX: &str = "gw-sk-";

pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

pub fn hash_key(secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hex::encode(hasher.finalize())
}

/// Runs a read query; a broken database logs and yields no rows instead of panicking.
pub(super) fn query_rows<T, P: rusqlite::Params>(
    conn: &Connection,
    sql: &str,
    params: P,
    map: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Vec<T> {
    let run = || -> rusqlite::Result<Vec<T>> {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(params, map)?.flatten().collect();
        Ok(rows)
    };
    run().unwrap_or_else(|e| {
        tracing::warn!("[gateway db] query failed: {e}");
        Vec::new()
    })
}

#[derive(Clone)]
pub struct GatewayStore {
    pub(super) conn: Arc<Mutex<Connection>>,
    /// In-memory "skip until" unix times, keyed by account id or "apikey:<provider>".
    pub(super) cooldowns: Arc<Mutex<HashMap<String, f64>>>,
    pub cipher: Cipher,
    pub directory: PathBuf,
}

impl GatewayStore {
    pub fn new<P: AsRef<Path>>(directory: P) -> Result<Self, String> {
        let dir = directory.as_ref();
        let _ = fs::create_dir_all(dir);
        let db_path = dir.join("gateway.db");
        let conn = Connection::open(&db_path).map_err(|e| format!("Failed to open gateway.db: {}", e))?;

        let cipher = Cipher::new(dir)?;
        let store = Self { conn: Arc::new(Mutex::new(conn)), cooldowns: Default::default(), cipher, directory: dir.to_path_buf() };
        store.init_tables()?;
        Ok(store)
    }

    fn init_tables(&self) -> Result<(), String> {
        let conn = self.conn.lock();
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS keys (
              id TEXT PRIMARY KEY, name TEXT NOT NULL, key_hash TEXT NOT NULL UNIQUE,
              key_prefix TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1,
              allowed_providers TEXT, rate_limit_per_min INTEGER,
              created_at REAL NOT NULL, last_used_at REAL,
              total_requests INTEGER NOT NULL DEFAULT 0,
              total_input_tokens INTEGER NOT NULL DEFAULT 0,
              total_output_tokens INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS providers (
              id TEXT PRIMARY KEY, enabled INTEGER NOT NULL DEFAULT 1,
              api_key TEXT, base_url TEXT, default_model TEXT, models TEXT,
              verified_at REAL, latency_ms INTEGER, error TEXT
            );
            CREATE TABLE IF NOT EXISTS combos (name TEXT PRIMARY KEY, targets TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS usage (
              id TEXT PRIMARY KEY, ts REAL NOT NULL, key_id TEXT, key_name TEXT,
              provider TEXT, model TEXT, input_tokens INTEGER, output_tokens INTEGER,
              duration_ms INTEGER, status TEXT, status_code INTEGER, error TEXT, streaming INTEGER
            );
            CREATE INDEX IF NOT EXISTS usage_ts ON usage(ts);
            CREATE TABLE IF NOT EXISTS accounts (
              id TEXT PRIMARY KEY, provider TEXT NOT NULL, label TEXT, email TEXT,
              connected_at REAL NOT NULL, refreshed_at REAL, last_error TEXT,
              sort_order INTEGER NOT NULL DEFAULT 0, enabled INTEGER NOT NULL DEFAULT 1,
              secret TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS accounts_provider ON accounts(provider, sort_order);
            CREATE TABLE IF NOT EXISTS settings (
              key TEXT PRIMARY KEY, value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS skills (
              name TEXT PRIMARY KEY, description TEXT NOT NULL DEFAULT '',
              prompt TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1,
              updated_at REAL NOT NULL
            );
            CREATE TABLE IF NOT EXISTS agents (
              name TEXT PRIMARY KEY, description TEXT NOT NULL DEFAULT '',
              model TEXT NOT NULL DEFAULT 'auto', skills TEXT NOT NULL DEFAULT '[]',
              personality TEXT NOT NULL DEFAULT '', temperature REAL,
              max_tokens INTEGER, enabled INTEGER NOT NULL DEFAULT 1,
              updated_at REAL NOT NULL
            );
            CREATE TABLE IF NOT EXISTS sessions (
              id TEXT PRIMARY KEY, messages TEXT NOT NULL DEFAULT '[]',
              updated_at REAL NOT NULL
            );
            CREATE TABLE IF NOT EXISTS mcp_servers (
              name TEXT PRIMARY KEY, transport TEXT NOT NULL DEFAULT 'stdio',
              command TEXT, args TEXT NOT NULL DEFAULT '[]',
              env TEXT NOT NULL DEFAULT '{}', url TEXT,
              enabled INTEGER NOT NULL DEFAULT 1, updated_at REAL NOT NULL
            );
            "#,
        )
        .map_err(|e| format!("Failed to init gateway schema: {}", e))?;
        let _ = conn.execute("ALTER TABLE accounts ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1", []);
        let _ = conn.execute("ALTER TABLE keys ADD COLUMN default_skills TEXT", []);
        let _ = conn.execute("ALTER TABLE keys ADD COLUMN max_tokens_budget INTEGER", []);
        let _ = conn.execute("ALTER TABLE keys ADD COLUMN max_cost_usd REAL", []);
        let _ = conn.execute("ALTER TABLE mcp_servers ADD COLUMN headers TEXT NOT NULL DEFAULT '{}'", []);
        let _ = conn.execute("ALTER TABLE providers ADD COLUMN synced_models TEXT", []);
        let _ = conn.execute("ALTER TABLE providers ADD COLUMN models_synced_at REAL", []);
        let _ = conn.execute("ALTER TABLE usage ADD COLUMN cached_tokens INTEGER", []);
        let _ = conn.execute("ALTER TABLE usage ADD COLUMN cost REAL", []);
        Ok(())
    }
}
