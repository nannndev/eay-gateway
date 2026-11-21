use super::{now, query_rows, BattleRecord, BenchmarkRunRecord, EloRatingRecord, GatewayStore};
use rusqlite::params;
use serde_json::Value;

const INITIAL_ELO: f64 = 1200.0;

fn k_factor(matches: i64) -> f64 {
    if matches < 10 {
        40.0
    } else if matches < 30 {
        32.0
    } else {
        24.0
    }
}

impl GatewayStore {
    // ---- Battles ---------------------------------------------------
    pub fn list_battles(&self, limit: i64) -> Vec<BattleRecord> {
        let conn = self.conn.lock();
        query_rows(
            &conn,
            "SELECT id, mode, category, data, created_at FROM battles ORDER BY created_at DESC LIMIT ?1",
            params![limit.clamp(1, 500)],
            |r| {
                let raw: String = r.get(3)?;
                Ok(BattleRecord {
                    id: r.get(0)?,
                    mode: r.get(1)?,
                    category: r.get(2)?,
                    data: serde_json::from_str(&raw).unwrap_or(Value::Null),
                    created_at: r.get(4)?,
                })
            },
        )
    }

    pub fn save_battle(&self, id: Option<&str>, mode: &str, category: Option<&str>, data: &Value) -> Result<BattleRecord, String> {
        let mode = mode.trim();
        if mode.is_empty() {
            return Err("Battle mode is required".into());
        }
        let id = id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(|| format!("btl_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]));
        let category = category.map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        let data_json = serde_json::to_string(data).map_err(|e| e.to_string())?;
        let created_at = now();
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO battles (id, mode, category, data, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, mode, category, data_json, created_at],
        )
        .map_err(|e| e.to_string())?;
        Ok(BattleRecord { id, mode: mode.to_string(), category, data: data.clone(), created_at })
    }

    pub fn delete_battle(&self, id: &str) -> Result<bool, String> {
        let conn = self.conn.lock();
        let rows = conn.execute("DELETE FROM battles WHERE id = ?1", params![id]).map_err(|e| e.to_string())?;
        Ok(rows > 0)
    }

    // ---- ELO Ratings -----------------------------------------------
    pub fn list_elo_ratings(&self) -> Vec<EloRatingRecord> {
        let conn = self.conn.lock();
        query_rows(
            &conn,
            "SELECT fighter, rating, wins, losses, ties, matches, updated_at FROM elo_ratings ORDER BY rating DESC, wins DESC, fighter ASC",
            [],
            |r| {
                Ok(EloRatingRecord {
                    fighter: r.get(0)?,
                    rating: r.get(1)?,
                    wins: r.get(2)?,
                    losses: r.get(3)?,
                    ties: r.get(4)?,
                    matches: r.get(5)?,
                    updated_at: r.get(6)?,
                })
            },
        )
    }

    /// Updates two fighters' ELO ratings based on outcome: `"a"`, `"b"`, `"tie"`, or `"both_bad"`.
    pub fn record_elo_match(&self, fighter_a: &str, fighter_b: &str, outcome: &str) -> Result<(EloRatingRecord, EloRatingRecord), String> {
        let a_name = fighter_a.trim();
        let b_name = fighter_b.trim();
        if a_name.is_empty() || b_name.is_empty() {
            return Err("Both fighter_a and fighter_b are required".into());
        }
        if a_name == b_name {
            return Err("Fighters must be distinct to update ELO ratings".into());
        }
        let outcome = outcome.trim().to_lowercase();
        if !matches!(outcome.as_str(), "a" | "b" | "tie" | "both_bad") {
            return Err("outcome must be one of: a, b, tie, both_bad".into());
        }

        let conn = self.conn.lock();
        let load = |name: &str| -> EloRatingRecord {
            conn.query_row(
                "SELECT fighter, rating, wins, losses, ties, matches, updated_at FROM elo_ratings WHERE fighter = ?1",
                params![name],
                |r| {
                    Ok(EloRatingRecord {
                        fighter: r.get(0)?,
                        rating: r.get(1)?,
                        wins: r.get(2)?,
                        losses: r.get(3)?,
                        ties: r.get(4)?,
                        matches: r.get(5)?,
                        updated_at: r.get(6)?,
                    })
                },
            )
            .unwrap_or_else(|_| EloRatingRecord {
                fighter: name.to_string(),
                rating: INITIAL_ELO,
                wins: 0,
                losses: 0,
                ties: 0,
                matches: 0,
                updated_at: now(),
            })
        };

        let mut a = load(a_name);
        let mut b = load(b_name);
        let ts = now();

        if outcome == "both_bad" {
            a.losses += 1;
            b.losses += 1;
            a.matches += 1;
            b.matches += 1;
        } else {
            let exp_a = 1.0 / (1.0 + 10_f64.powf((b.rating - a.rating) / 400.0));
            let exp_b = 1.0 - exp_a;
            let (score_a, score_b) = match outcome.as_str() {
                "a" => {
                    a.wins += 1;
                    b.losses += 1;
                    (1.0, 0.0)
                }
                "b" => {
                    a.losses += 1;
                    b.wins += 1;
                    (0.0, 1.0)
                }
                _ => {
                    a.ties += 1;
                    b.ties += 1;
                    (0.5, 0.5)
                }
            };
            a.rating = (a.rating + k_factor(a.matches) * (score_a - exp_a)).round();
            b.rating = (b.rating + k_factor(b.matches) * (score_b - exp_b)).round();
            a.matches += 1;
            b.matches += 1;
        }
        a.updated_at = ts;
        b.updated_at = ts;

        for rec in [&a, &b] {
            conn.execute(
                "INSERT OR REPLACE INTO elo_ratings (fighter, rating, wins, losses, ties, matches, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![rec.fighter, rec.rating, rec.wins, rec.losses, rec.ties, rec.matches, rec.updated_at],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok((a, b))
    }

    pub fn reset_elo_ratings(&self) -> Result<(), String> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM elo_ratings", []).map_err(|e| e.to_string())?;
        Ok(())
    }

    // ---- Benchmarks ------------------------------------------------
    pub fn list_benchmarks(&self, limit: i64) -> Vec<BenchmarkRunRecord> {
        let conn = self.conn.lock();
        query_rows(
            &conn,
            "SELECT id, suite_id, name, data, created_at FROM benchmarks ORDER BY created_at DESC LIMIT ?1",
            params![limit.clamp(1, 200)],
            |r| {
                let raw: String = r.get(3)?;
                Ok(BenchmarkRunRecord {
                    id: r.get(0)?,
                    suite_id: r.get(1)?,
                    name: r.get(2)?,
                    data: serde_json::from_str(&raw).unwrap_or(Value::Null),
                    created_at: r.get(4)?,
                })
            },
        )
    }

    pub fn save_benchmark(&self, id: Option<&str>, suite_id: &str, name: &str, data: &Value) -> Result<BenchmarkRunRecord, String> {
        let suite_id = suite_id.trim();
        let name = name.trim();
        if suite_id.is_empty() || name.is_empty() {
            return Err("suite_id and name are required".into());
        }
        let id = id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(|| format!("bnch_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]));
        let data_json = serde_json::to_string(data).map_err(|e| e.to_string())?;
        let created_at = now();
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO benchmarks (id, suite_id, name, data, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, suite_id, name, data_json, created_at],
        )
        .map_err(|e| e.to_string())?;
        Ok(BenchmarkRunRecord { id, suite_id: suite_id.to_string(), name: name.to_string(), data: data.clone(), created_at })
    }

    pub fn delete_benchmark(&self, id: &str) -> Result<bool, String> {
        let conn = self.conn.lock();
        let rows = conn.execute("DELETE FROM benchmarks WHERE id = ?1", params![id]).map_err(|e| e.to_string())?;
        Ok(rows > 0)
    }
}
