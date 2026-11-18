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
        });
        assert_eq!(s.analytics(Some(1), 0)["days"][0]["cached"], 6);
    }

    #[test]
    fn analytics_ranks_todays_models_by_tokens() {
        let (s, _d) = store();
        log(&s, "codex", 5, 200);
        assert_eq!(s.analytics(Some(1), 0)["top_models"][0]["tokens"], 5);
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
