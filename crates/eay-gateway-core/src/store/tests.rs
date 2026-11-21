use super::*;
use serde_json::json;

/// A fresh store in a temp dir; keep the dir alive for the test's duration.
fn store() -> (GatewayStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    (GatewayStore::new(dir.path()).unwrap(), dir)
}

fn account_enabled(store: &GatewayStore, id: &str) -> Option<bool> {
    store.list_accounts().into_iter().find(|a| a.id == id).map(|a| a.enabled)
}

mod teams {
    use super::*;

    #[test]
    fn a_team_saved_under_its_name_keeps_that_name_and_sessions_follow_the_new_id() {
        let dir = tempfile::tempdir().unwrap();
        {
            let conn = rusqlite::Connection::open(dir.path().join("gateway.db")).unwrap();
            conn.execute_batch(
                "CREATE TABLE teams (
                    name TEXT PRIMARY KEY, lead TEXT NOT NULL, members TEXT NOT NULL, updated_at REAL NOT NULL
                );
                INSERT INTO teams VALUES ('app', 'lead', '[\"lead\"]', 1);
                CREATE TABLE sessions (
                    id TEXT PRIMARY KEY, messages TEXT NOT NULL DEFAULT '[]', updated_at REAL NOT NULL, team TEXT
                );
                INSERT INTO sessions VALUES ('local:s1', '[]', 1, 'app');",
            )
            .unwrap();
        }
        let store = GatewayStore::new(dir.path()).unwrap();
        let teams = store.list_teams();
        assert_eq!(teams.len(), 1);
        assert_eq!(teams[0].name, "app");
        assert!(teams[0].id.starts_with("tm_"), "{}", teams[0].id);
        assert_eq!(store.session_team("local:s1").as_deref(), Some(teams[0].id.as_str()));
    }
}

mod accounts {
    use super::*;

    fn with_account() -> (GatewayStore, tempfile::TempDir, String) {
        let (s, d) = store();
        let id = s.save_account("codex", &json!({ "access_token": "tok" }), Some("Main"), Some("a@b.com")).unwrap();
        (s, d, id)
    }

    #[test]
    fn new_accounts_start_enabled() {
        let (s, _d, id) = with_account();
        assert_eq!(account_enabled(&s, &id), Some(true));
    }

    #[test]
    fn toggling_without_a_value_flips_enabled() {
        let (s, _d, id) = with_account();
        s.set_account_enabled("codex", &id, None).unwrap();
        assert_eq!(account_enabled(&s, &id), Some(false));
    }

    #[test]
    fn an_explicit_value_re_enables() {
        let (s, _d, id) = with_account();
        s.set_account_enabled("codex", &id, Some(false)).unwrap();
        s.set_account_enabled("codex", &id, Some(true)).unwrap();
        assert_eq!(account_enabled(&s, &id), Some(true));
    }

    #[test]
    fn credentials_are_decrypted_on_read() {
        let (s, _d, id) = with_account();
        let acc = s.list_accounts().into_iter().find(|a| a.id == id).unwrap();
        assert_eq!(acc.cred["access_token"], "tok");
    }

    #[test]
    fn reorder_changes_the_listing_order() {
        let (s, _d, a) = with_account();
        let b = s.save_account("codex", &json!({ "access_token": "t2" }), Some("Second"), None).unwrap();
        s.reorder_accounts("codex", &[b.clone(), a]).unwrap();
        assert_eq!(s.list_accounts()[0].id, b);
    }
}

mod providers {
    use super::*;

    #[test]
    fn disabling_a_provider_keeps_its_key() {
        let (s, _d) = store();
        s.save_provider_key("openai", "sk-test-123").unwrap();
        s.set_provider_enabled("openai", Some(false)).unwrap();
        assert_eq!(s.get_provider_key("openai").as_deref(), Some("sk-test-123"));
    }

    #[test]
    fn disabling_a_provider_reports_it_disabled() {
        let (s, _d) = store();
        s.save_provider_key("openai", "sk").unwrap();
        s.set_provider_enabled("openai", Some(false)).unwrap();
        assert!(!s.is_provider_enabled("openai"));
    }

    #[test]
    fn adding_the_same_custom_model_twice_keeps_one() {
        let (s, _d) = store();
        s.add_provider_model("openai", "gpt-custom-1").unwrap();
        assert_eq!(s.add_provider_model("openai", "gpt-custom-1").unwrap(), vec!["gpt-custom-1".to_string()]);
    }

    #[test]
    fn removing_a_custom_model_drops_it() {
        let (s, _d) = store();
        s.add_provider_model("openai", "gpt-custom-1").unwrap();
        assert!(s.remove_provider_model("openai", "gpt-custom-1").unwrap().is_empty());
    }

    #[test]
    fn disconnecting_clears_the_key() {
        let (s, _d) = store();
        s.save_provider_key("openai", "sk").unwrap();
        s.disconnect_provider("openai").unwrap();
        assert_eq!(s.get_provider_key("openai"), None);
    }
}

mod synced_models {
    use super::*;

    #[test]
    fn display_falls_back_to_seeds_before_any_sync() {
        let (s, _d) = store();
        assert_eq!(s.display_models("openai", &["seed-a"]), vec!["seed-a"]);
    }

    #[test]
    fn display_uses_the_synced_list_instead_of_seeds() {
        let (s, _d) = store();
        s.set_synced_models("openai", &["live-1".into()]).unwrap();
        assert_eq!(s.display_models("openai", &["seed-a"]), vec!["live-1"]);
    }

    #[test]
    fn display_lists_custom_models_first() {
        let (s, _d) = store();
        s.set_synced_models("openai", &["live-1".into()]).unwrap();
        s.add_provider_model("openai", "mine").unwrap();
        assert_eq!(s.display_models("openai", &[]), vec!["mine", "live-1"]);
    }

    #[test]
    fn sync_records_when_it_happened() {
        let (s, _d) = store();
        s.set_synced_models("openai", &["live-1".into()]).unwrap();
        assert!(s.get_synced_models("openai").1.is_some());
    }

    #[test]
    fn disconnecting_forgets_the_synced_list() {
        let (s, _d) = store();
        s.save_provider_key("openai", "sk").unwrap();
        s.set_synced_models("openai", &["live-1".into()]).unwrap();
        s.disconnect_provider("openai").unwrap();
        assert!(s.get_synced_models("openai").0.is_empty());
    }
}

mod virtual_keys {
    use super::*;

    #[test]
    fn a_created_key_is_found_by_its_secret() {
        let (s, _d) = store();
        let (rec, secret) = s.create_virtual_key("ci", None, None).unwrap();
        assert_eq!(s.find_virtual_key(&secret).map(|k| k.id), Some(rec.id));
    }

    #[test]
    fn a_wrong_secret_finds_nothing() {
        let (s, _d) = store();
        s.create_virtual_key("ci", None, None).unwrap();
        assert!(s.find_virtual_key("gw-sk-wrong").is_none());
    }

    #[test]
    fn secrets_carry_the_gateway_prefix() {
        let (s, _d) = store();
        let (_, secret) = s.create_virtual_key("ci", None, None).unwrap();
        assert!(secret.starts_with(KEY_PREFIX), "{secret}");
    }

    #[test]
    fn touching_a_key_counts_requests_and_tokens() {
        let (s, _d) = store();
        let (rec, secret) = s.create_virtual_key("ci", None, None).unwrap();
        s.touch_virtual_key(&rec.id, 10, 5);
        let k = s.find_virtual_key(&secret).unwrap();
        assert_eq!((k.total_requests, k.total_input_tokens, k.total_output_tokens), (1, 10, 5));
    }

    #[test]
    fn toggling_disables_a_key() {
        let (s, _d) = store();
        let (rec, _) = s.create_virtual_key("ci", None, None).unwrap();
        assert!(!s.toggle_virtual_key(&rec.id).unwrap().enabled);
    }

    #[test]
    fn recent_requests_are_counted_per_key() {
        let (s, _d) = store();
        let (rec, _) = s.create_virtual_key("ci", None, None).unwrap();
        s.record_usage(&UsageEntry {
            key_id: Some(&rec.id),
            key_name: Some("ci"),
            provider: Some("openai"),
            model: Some("gpt"),
            input_tokens: 1,
            output_tokens: 1,
            cached_tokens: 0,
            duration_ms: 1,
            status: "success",
            status_code: 200,
            error: None,
            streaming: false,
            agent: None,
            team: None,
            project: None,
            source: None,
        });
        assert_eq!(s.key_requests_since(&rec.id, now() - 60.0), 1);
    }
}

mod cooldowns {
    use super::*;

    #[test]
    fn a_cooled_key_reports_until_when() {
        let (s, _d) = store();
        s.cool_down("acc-1", 60);
        assert!(s.cooling_until("acc-1").is_some_and(|t| t > now()));
    }

    #[test]
    fn clearing_ends_the_cooldown() {
        let (s, _d) = store();
        s.cool_down("acc-1", 60);
        s.clear_cooldown("acc-1");
        assert!(s.cooling_until("acc-1").is_none());
    }

    #[test]
    fn a_zero_second_cooldown_is_already_over() {
        let (s, _d) = store();
        s.cool_down("acc-1", 0);
        assert!(s.cooling_until("acc-1").is_none());
    }
}

mod settings {
    use super::*;

    #[test]
    fn defaults_to_priority_routing() {
        let (s, _d) = store();
        assert_eq!(s.get_gateway_settings().routing_strategy, "priority");
    }

    #[test]
    fn maps_the_legacy_ordered_strategy_to_priority() {
        let (s, _d) = store();
        s.set_setting("routing_strategy", "ordered").unwrap();
        assert_eq!(s.get_gateway_settings().routing_strategy, "priority");
    }

    #[test]
    fn reads_the_saved_provider_priority() {
        let (s, _d) = store();
        s.set_setting("provider_priority", r#"["deepseek","openai"]"#).unwrap();
        assert_eq!(s.get_gateway_settings().provider_priority, vec!["deepseek", "openai"]);
    }

    #[test]
    fn defaults_to_two_auto_models_per_provider() {
        let (s, _d) = store();
        assert_eq!(s.get_gateway_settings().auto_models_per_provider, 2);
    }
}

mod analytics {
    use super::*;

    fn log(s: &GatewayStore, provider: &str, tokens: i64, status_code: i64) {
        s.record_usage(&UsageEntry {
            key_id: None,
            key_name: None,
            provider: Some(provider),
            model: Some("m"),
            input_tokens: tokens,
            output_tokens: 0,
            cached_tokens: 0,
            duration_ms: 1,
            status: if status_code < 400 { "success" } else { "error" },
            status_code,
            error: None,
            streaming: false,
            agent: None,
            team: None,
            project: None,
            source: None,
        });
    }

    #[test]
    fn todays_tokens_land_in_the_last_day_slot() {
        let (s, _d) = store();
        log(&s, "codex", 7, 200);
        assert_eq!(s.daily_tokens_by_provider(7, 0).get("codex").map(|d| d[6]), Some(7));
    }

    #[test]
    fn daily_tokens_cover_the_requested_number_of_days() {
        let (s, _d) = store();
        log(&s, "codex", 7, 200);
        assert_eq!(s.daily_tokens_by_provider(14, 0).get("codex").map(Vec::len), Some(14));
    }

    #[test]
    fn analytics_counts_requests_per_day() {
        let (s, _d) = store();
        log(&s, "codex", 1, 200);
        log(&s, "codex", 1, 200);
        assert_eq!(s.analytics(Some(7), 0)["days"][0]["requests"], 2);
    }

    #[test]
    fn analytics_marks_failed_requests() {
        let (s, _d) = store();
        log(&s, "codex", 1, 429);
        assert_eq!(s.analytics(None, 0)["recent"][0]["ok"], false);
    }

    #[test]
    fn analytics_lists_models_for_the_whole_range() {
        let (s, _d) = store();
        log(&s, "codex", 5, 200);
        assert_eq!(s.analytics(None, 0)["models"][0]["provider"], "codex");
    }

    #[test]
    fn analytics_reports_estimated_cost_for_priced_models() {
        let (s, _d) = store();
        s.record_usage(&UsageEntry {
            key_id: None,
            key_name: None,
            provider: Some("claude"),
            model: Some("claude-sonnet-5-5"),
            input_tokens: 1_000_000,
            output_tokens: 0,
            cached_tokens: 0,
            duration_ms: 1,
            status: "success",
            status_code: 200,
            error: None,
            streaming: false,
            agent: None,
            team: None,
            project: None,
            source: None,
        });
        assert_eq!(s.analytics(Some(1), 0)["days"][0]["cost"], 2.0);
    }

    #[test]
    fn analytics_sums_cached_tokens_per_day() {
        let (s, _d) = store();
        s.record_usage(&UsageEntry {
            key_id: None,
            key_name: None,
            provider: Some("claude"),
            model: Some("m"),
            input_tokens: 10,
            output_tokens: 0,
            cached_tokens: 6,
            duration_ms: 1,
            status: "success",
            status_code: 200,
            error: None,
            streaming: false,
            agent: None,
            team: None,
            project: None,
            source: None,
        });
        assert_eq!(s.analytics(Some(1), 0)["days"][0]["cached"], 6);
    }

    #[test]
    fn analytics_ranks_todays_models_by_tokens() {
        let (s, _d) = store();
        log(&s, "codex", 5, 200);
        assert_eq!(s.analytics(Some(1), 0)["top_models"][0]["tokens"], 5);
    }

    fn log_who(s: &GatewayStore, agent: Option<&str>, team: Option<&str>, provider: &str, model: &str, tokens: i64) {
        s.record_usage(&UsageEntry {
            key_id: None,
            key_name: None,
            provider: Some(provider),
            model: Some(model),
            input_tokens: tokens,
            output_tokens: 0,
            cached_tokens: 0,
            duration_ms: 1,
            status: "success",
            status_code: 200,
            error: None,
            streaming: false,
            agent,
            team,
            project: None,
            source: None,
        });
    }

    #[test]
    fn analytics_splits_agents_and_teams_and_lists_their_models() {
        let (s, _d) = store();
        let policy = crate::mcp::ToolPolicy::default();
        s.save_agent("reviewer", "", "auto", &[], "", None, None, &policy, true, &[]).unwrap();
        s.save_agent("tester", "", "auto", &[], "", None, None, &policy, true, &[]).unwrap();
        let team = s.save_team(None, "app", "reviewer", &["reviewer".into(), "tester".into()]).unwrap();
        log_who(&s, Some("reviewer"), Some(&team.id), "claude", "claude-sonnet-4.5", 30);
        log_who(&s, Some("reviewer"), Some(&team.id), "codex", "gpt-5.5", 10);
        log_who(&s, Some("tester"), Some(&team.id), "claude", "claude-sonnet-4.5", 20);
        log_who(&s, None, Some(&team.id), "openai", "gpt-4.1", 5);
        log_who(&s, Some("reviewer"), None, "claude", "claude-sonnet-4.5", 7);
        log_who(&s, None, None, "codex", "m", 1);

        let report = s.analytics(None, 0);
        let agents = report["agents"].as_array().unwrap();
        assert_eq!(agents[0]["agent"], "reviewer");
        assert_eq!(agents[0]["tokens"], 47);
        assert_eq!(agents[0]["models"].as_array().unwrap().len(), 2);
        assert_eq!(agents[0]["models"][0]["model"], "claude-sonnet-4.5");
        assert_eq!(agents.len(), 2);

        let teams = &report["teams"][0];
        assert_eq!(teams["id"], team.id);
        assert_eq!(teams["name"], "app");
        assert_eq!(teams["tokens"], 65);
        assert_eq!(teams["agents"].as_array().unwrap().len(), 2);
        assert_eq!(teams["agents"][0]["agent"], "reviewer");
        assert_eq!(teams["agents"][0]["tokens"], 40);
        assert_eq!(teams["models"].as_array().unwrap().len(), 3);
    }
}

mod stats {
    use super::*;

    fn log(s: &GatewayStore, status: &str, code: i64) {
        s.record_usage(&UsageEntry {
            key_id: None,
            key_name: None,
            provider: Some("codex"),
            model: Some("m"),
            input_tokens: 1,
            output_tokens: 1,
            cached_tokens: 0,
            duration_ms: 1,
            status,
            status_code: code,
            error: None,
            streaming: false,
            agent: None,
            team: None,
            project: None,
            source: None,
        });
    }

    #[test]
    fn successful_requests_are_not_counted_as_errors() {
        let (s, _d) = store();
        log(&s, "success", 200);
        assert_eq!(s.get_stats()["errors"], 0);
    }

    #[test]
    fn failed_requests_are_counted_as_errors() {
        let (s, _d) = store();
        log(&s, "error", 429);
        assert_eq!(s.get_stats()["errors"], 1);
    }
}

mod combos {
    use super::*;

    #[test]
    fn a_saved_combo_is_listed_with_its_targets() {
        let (s, _d) = store();
        s.save_combo("mix", &["oa/gpt-4o".into()]).unwrap();
        assert_eq!(s.list_combos().get("mix"), Some(&vec!["oa/gpt-4o".to_string()]));
    }
}

mod projects {
    use super::*;
    use serde_json::json;

    #[test]
    fn create_update_validate_and_delete() {
        let (s, _d) = store();
        let p = s.save_project(None, "Shop", "Be brief.", &json!({ "repo": "/x" }), None).unwrap();
        assert!(p.id.starts_with("prj_"));
        let again = s.save_project(Some(&p.id), "Shop v2", "", &json!(null), None).unwrap();
        assert_eq!(again.id, p.id);
        assert_eq!(again.created_at, p.created_at, "editing keeps the creation time");
        assert_eq!(again.metadata, json!({}), "null metadata becomes an empty object");
        assert!(s.save_project(None, "", "", &json!({}), None).is_err());
        assert!(s.save_project(None, "x", "", &json!([1, 2]), None).is_err(), "metadata must be an object");
        assert!(s.save_project(Some("prj_missing"), "x", "", &json!({}), None).is_err());
        assert!(s.set_project_link("keys", "nope", Some(&p.id)).is_err());
        assert!(s.set_project_link("agents", "x", Some(&p.id)).is_err(), "only keys and teams link");
        assert!(s.delete_project(&p.id).unwrap());
        assert!(!s.delete_project(&p.id).unwrap());
    }

    #[test]
    fn editing_a_team_keeps_its_project() {
        let (s, _d) = store();
        s.save_agent("a", "", "auto", &[], "", None, None, &Default::default(), true, &[]).unwrap();
        let p = s.save_project(None, "P", "", &json!({}), None).unwrap();
        let t = s.save_team(None, "crew", "", &["a".into()]).unwrap();
        s.set_project_link("teams", &t.id, Some(&p.id)).unwrap();
        let edited = s.save_team(Some(&t.id), "crew2", "", &["a".into()]).unwrap();
        assert_eq!(edited.project.as_deref(), Some(p.id.as_str()));
    }
}

mod upgrade_to_projects {
    use super::*;

    #[test]
    fn a_040_database_opens_and_gains_project_columns_without_losing_data() {
        let dir = tempfile::tempdir().unwrap();
        {
            let s = GatewayStore::new(dir.path()).unwrap();
            s.save_agent("a", "", "auto", &[], "", None, None, &Default::default(), true, &[]).unwrap();
            s.save_team(None, "crew", "", &["a".into()]).unwrap();
            s.save_session_messages("local:s1", &[serde_json::json!({ "role": "user", "content": "hi" })]).unwrap();
            s.create_virtual_key("k", None, None).unwrap();
        }
        // Simulate 0.4: drop the project columns by rebuilding the tables without them.
        {
            let conn = rusqlite::Connection::open(dir.path().join("gateway.db")).unwrap();
            conn.execute_batch(
                "DROP TABLE projects;
                 CREATE TABLE t AS SELECT id, name, lead, members, updated_at FROM teams; DROP TABLE teams; ALTER TABLE t RENAME TO teams;
                 CREATE TABLE s AS SELECT id, messages, updated_at, team FROM sessions; DROP TABLE sessions; ALTER TABLE s RENAME TO sessions;",
            )
            .unwrap();
        }
        let s = GatewayStore::new(dir.path()).unwrap();
        assert_eq!(s.list_teams().len(), 1);
        assert_eq!(s.get_session_messages("local:s1").len(), 1);
        assert_eq!(s.list_virtual_keys().len(), 1);
        let p = s.save_project(None, "P", "", &serde_json::json!({}), None).unwrap();
        let team = s.list_teams()[0].id.clone();
        s.set_project_link("teams", &team, Some(&p.id)).unwrap();
        s.set_session_project("local:s1", &p.id).unwrap();
        assert_eq!(s.session_project("local:s1").as_deref(), Some(p.id.as_str()));
        assert_eq!(s.get_session_messages("local:s1").len(), 1, "binding a project keeps the chat");
    }
}

mod battles {
    use super::*;
    use serde_json::json;

    #[test]
    fn battles_elo_and_benchmarks_roundtrip() {
        let (s, _d) = store();
        let b = s.save_battle(None, "blind", Some("coding"), &json!({ "prompt": "hi" })).unwrap();
        assert!(b.id.starts_with("btl_"));
        assert_eq!(s.list_battles(10).len(), 1);
        assert!(s.delete_battle(&b.id).unwrap());

        let (a, b_rat) = s.record_elo_match("openai/gpt-4o", "anthropic/claude-sonnet", "a").unwrap();
        assert!(a.rating > 1200.0 && b_rat.rating < 1200.0);
        assert_eq!(a.wins, 1);
        assert_eq!(b_rat.losses, 1);
        assert_eq!(s.list_elo_ratings().len(), 2);
        s.reset_elo_ratings().unwrap();
        assert!(s.list_elo_ratings().is_empty());

        let bn = s.save_benchmark(None, "fullstack-v1", "Nov Run", &json!({ "pass_rate": 0.9 })).unwrap();
        assert!(bn.id.starts_with("bnch_"));
        assert_eq!(s.list_benchmarks(10).len(), 1);
        assert!(s.delete_benchmark(&bn.id).unwrap());
    }
}
