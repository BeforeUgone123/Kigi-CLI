//! The `[platforms.<name>] models` allowlist: keep the selected, synthesize the manual.

use indexmap::IndexMap;

use super::CredentialedProvider;
use crate::agent::config::{ModelEntry, PlatformsConfig};

/// Synthesizes selected ids the listing never served; runs before `[model.*]` overrides.
pub(crate) fn synthesize_selected(
    resolved: &mut IndexMap<String, ModelEntry>,
    platforms: &PlatformsConfig,
    keyed: &[CredentialedProvider],
) {
    for credentialed in keyed {
        let provider = &credentialed.provider;
        let Some(list) = platforms
            .entries
            .get(&provider.name)
            .and_then(|e| e.models.as_ref())
        else {
            continue;
        };
        for id in list.iter().map(|m| m.trim()) {
            if let Err(e) = kigi_models::custom::validate_model_id(id) {
                tracing::warn!(provider = %provider.name, %e, "skipping an invalid selected model id");
                continue;
            }
            let key = provider.managed_model_key(id);
            if resolved.contains_key(&key) {
                continue;
            }
            let entry = super::fetch::manual_entry(provider, id);
            resolved.insert(key, ModelEntry::from_config_entry(&entry));
        }
    }
}

/// Drops `{name}/*` entries outside the provider's selection; runs after `[model.*]` overrides.
pub(crate) fn filter_unselected(
    resolved: &mut IndexMap<String, ModelEntry>,
    platforms: &PlatformsConfig,
    keyed: &[CredentialedProvider],
) {
    for credentialed in keyed {
        let provider = &credentialed.provider;
        let Some(list) = platforms
            .entries
            .get(&provider.name)
            .and_then(|e| e.models.as_ref())
        else {
            continue;
        };
        let list: Vec<&str> = list.iter().map(|m| m.trim()).collect();
        let prefix = format!("{}/", provider.name);
        resolved.retain(|key, entry| {
            let id = entry.info.id.as_deref().unwrap_or(key.as_str());
            id.strip_prefix(&prefix)
                .is_none_or(|bare| list.contains(&bare))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kigi_models::custom::{CustomApi, CustomProvider};

    use crate::agent::models::PlatformApiKeys;

    fn provider(name: &str, base: &str) -> CustomProvider {
        CustomProvider::new(name, CustomApi::OpenAi, base).unwrap()
    }

    fn fetched_entry(provider: &CustomProvider, id: &str) -> (String, ModelEntry) {
        let cfg = super::super::fetch::manual_entry(provider, id);
        let key = cfg.id.clone().unwrap();
        (key, ModelEntry::from_config_entry(&cfg))
    }

    fn keyed(provider: CustomProvider) -> Vec<CredentialedProvider> {
        vec![CredentialedProvider::for_test(provider, "sk-x")]
    }

    #[test]
    fn selection_filters_the_listing_and_synthesizes_the_manual() {
        let proxy = provider("proxy", "https://p.example/v1");
        let mut resolved = IndexMap::new();
        for id in ["m1", "m2"] {
            let (key, entry) = fetched_entry(&proxy, id);
            resolved.insert(key, entry);
        }
        let platforms: PlatformsConfig = toml::from_str(
            "[proxy]\nbase_url = \"https://p.example/v1\"\napi = \"openai\"\nmodels = [\"m2\", \"manual-1\"]\n",
        )
        .unwrap();

        synthesize_selected(&mut resolved, &platforms, &keyed(proxy.clone()));
        filter_unselected(&mut resolved, &platforms, &keyed(proxy));

        let ids: Vec<_> = resolved.keys().map(String::as_str).collect();
        assert_eq!(ids, ["proxy/m2", "proxy/manual-1"]);
        assert_eq!(resolved["proxy/manual-1"].info.model, "manual-1");
    }

    #[test]
    fn a_provider_without_the_models_key_keeps_its_whole_listing() {
        let proxy = provider("proxy", "https://p.example/v1");
        let mut resolved = IndexMap::new();
        let (key, entry) = fetched_entry(&proxy, "m1");
        resolved.insert(key, entry);
        let platforms: PlatformsConfig =
            toml::from_str("[proxy]\nbase_url = \"https://p.example/v1\"\napi = \"openai\"\n")
                .unwrap();

        synthesize_selected(&mut resolved, &platforms, &keyed(proxy.clone()));
        filter_unselected(&mut resolved, &platforms, &keyed(proxy));

        assert!(resolved.contains_key("proxy/m1"));
    }

    #[test]
    fn an_empty_selection_drops_every_fetched_entry() {
        let proxy = provider("proxy", "https://p.example/v1");
        let mut resolved = IndexMap::new();
        let (key, entry) = fetched_entry(&proxy, "m1");
        resolved.insert(key, entry);
        let platforms: PlatformsConfig = toml::from_str(
            "[proxy]\nbase_url = \"https://p.example/v1\"\napi = \"openai\"\nmodels = []\n",
        )
        .unwrap();

        synthesize_selected(&mut resolved, &platforms, &keyed(proxy.clone()));
        filter_unselected(&mut resolved, &platforms, &keyed(proxy));

        assert!(resolved.is_empty());
    }

    #[test]
    fn invalid_selected_ids_are_skipped_with_a_warning() {
        let proxy = provider("proxy", "https://p.example/v1");
        let mut resolved = IndexMap::new();
        let platforms: PlatformsConfig = toml::from_str(
            "[proxy]\nbase_url = \"https://p.example/v1\"\napi = \"openai\"\nmodels = [\"ok\", \"has space\"]\n",
        )
        .unwrap();

        synthesize_selected(&mut resolved, &platforms, &keyed(proxy.clone()));
        filter_unselected(&mut resolved, &platforms, &keyed(proxy));

        assert!(resolved.contains_key("proxy/ok"));
        assert_eq!(resolved.len(), 1);
    }

    #[test]
    fn selection_applies_through_resolve_model_list_and_gets_the_key() {
        let raw: toml::Value = toml::from_str(
            "[platforms.proxy]\nbase_url = \"https://p.example/v1\"\napi = \"openai\"\nmodels = [\"manual-1\"]\n",
        )
        .unwrap();
        let cfg = crate::agent::config::Config::new_from_toml_cfg(&raw).unwrap();
        let proxy = provider("proxy", "https://p.example/v1");
        let keys =
            PlatformApiKeys::test_custom(vec![CredentialedProvider::for_test(proxy, "sk-proxy")]);

        let resolved = crate::agent::config::resolve_model_list(&cfg, None, &keys);

        let entry = resolved
            .get("proxy/manual-1")
            .expect("a manual model survives without any fetched catalog");
        assert_eq!(entry.api_key.as_deref(), Some("sk-proxy"));
        assert!(entry.has_own_credentials());
        assert_eq!(entry.info.context_window.get(), 256_000);
    }

    #[test]
    fn a_model_override_composes_with_the_synthesized_entry() {
        let raw: toml::Value = toml::from_str(
            "[platforms.proxy]\nbase_url = \"https://p.example/v1\"\napi = \"openai\"\nmodels = [\"manual-1\"]\n\n[model.\"proxy/manual-1\"]\ncontext_window = 1000000\n",
        )
        .unwrap();
        let cfg = crate::agent::config::Config::new_from_toml_cfg(&raw).unwrap();
        let proxy = provider("proxy", "https://p.example/v1");
        let keys =
            PlatformApiKeys::test_custom(vec![CredentialedProvider::for_test(proxy, "sk-proxy")]);

        let resolved = crate::agent::config::resolve_model_list(&cfg, None, &keys);

        let entry = &resolved["proxy/manual-1"];
        assert_eq!(entry.info.context_window.get(), 1_000_000);
        assert_eq!(entry.info.base_url, "https://p.example/v1");
        assert_eq!(entry.api_key.as_deref(), Some("sk-proxy"));
    }

    #[test]
    fn the_selection_is_authoritative_in_the_provider_namespace() {
        // A context-only override cannot resurrect a deselected model.
        let raw: toml::Value = toml::from_str(
            "[platforms.proxy]\nbase_url = \"https://p.example/v1\"\napi = \"openai\"\nmodels = [\"m2\"]\n\n[model.\"proxy/m1\"]\ncontext_window = 123456\n",
        )
        .unwrap();
        let cfg = crate::agent::config::Config::new_from_toml_cfg(&raw).unwrap();
        let proxy = provider("proxy", "https://p.example/v1");
        let (key, entry) = fetched_entry(&proxy, "m1");
        let prefetched = IndexMap::from([(key, entry)]);
        let keys =
            PlatformApiKeys::test_custom(vec![CredentialedProvider::for_test(proxy, "sk-proxy")]);

        let resolved = crate::agent::config::resolve_model_list(&cfg, Some(prefetched), &keys);

        assert!(!resolved.contains_key("proxy/m1"));
        assert!(resolved.contains_key("proxy/m2"));
    }

    #[test]
    fn provider_owned_entries_keep_their_declared_wire_against_donors() {
        let raw: toml::Value = toml::from_str(
            "[platforms.proxy]\nbase_url = \"https://p.example/v1\"\napi = \"openai\"\nmodels = [\"shared\"]\n\n[model.donorx]\nmodel = \"shared\"\napi_backend = \"messages\"\ncontext_window = 500000\n",
        )
        .unwrap();
        let cfg = crate::agent::config::Config::new_from_toml_cfg(&raw).unwrap();
        let proxy = provider("proxy", "https://p.example/v1");
        let keys =
            PlatformApiKeys::test_custom(vec![CredentialedProvider::for_test(proxy, "sk-proxy")]);

        let resolved = crate::agent::config::resolve_model_list(&cfg, None, &keys);

        let entry = &resolved["proxy/shared"];
        assert_eq!(
            entry.info.api_backend,
            crate::sampling::ApiBackend::ChatCompletions
        );
    }
}
