//! Listing fetch and key stamping for custom providers.

use indexmap::IndexMap;
use kigi_models::WireModel;
use kigi_models::custom::{CustomApi, CustomProvider};

use super::CredentialedProvider;
use crate::agent::config::{ModelEntry, ModelEntryConfig};
use crate::agent::models_fetch::{BackendError, wire_model_to_entry};

/// `GET {base}/models` for one provider; every model is keyed `{name}/{id}`.
pub(crate) fn fetch_models_blocking(
    credentialed: &CredentialedProvider,
) -> Result<Vec<ModelEntryConfig>, BackendError> {
    let provider = &credentialed.provider;
    let url = provider.models_url();
    tracing::info!(provider = %provider.name, api = provider.api.as_str(), %url, "fetching custom provider models");
    let client = crate::http::shared_blocking_client();
    let request = match provider.api {
        CustomApi::OpenAi => client
            .get(&url)
            .header("Authorization", format!("Bearer {}", credentialed.key())),
        CustomApi::Anthropic => client
            .get(&url)
            .header("x-api-key", credentialed.key())
            .header("anthropic-version", kigi_sampling_types::ANTHROPIC_VERSION),
    };
    let response = request.send()?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = response.text().unwrap_or_default();
        return Err(BackendError::RequestFailed { status, body });
    }
    let body = response.text()?;
    let listing = match provider.api {
        CustomApi::OpenAi => kigi_models::parse_openai_listing(&body),
        CustomApi::Anthropic => kigi_models::parse_anthropic_listing(&body),
    };
    let wire = listing.map_err(|e| BackendError::RequestFailed {
        status: 200,
        body: format!("{} listing parse failed: {e}", provider.api.as_str()),
    })?;
    if wire.is_empty() {
        tracing::warn!(provider = %provider.name, "custom provider listed no models");
    }
    Ok(wire.into_iter().map(|w| entry(provider, w)).collect())
}

fn entry(provider: &CustomProvider, wire: WireModel) -> ModelEntryConfig {
    let mut entry = wire_model_to_entry(
        provider.managed_model_key(&wire.id),
        provider.api.wire_api(),
        provider.api.key_header(),
        None,
        true,
        wire,
        &provider.base_url,
    );
    entry.description = Some(provider.name.clone());
    entry
}

/// Drops fetched entries of a provider that is gone or keyless.
pub(crate) fn drop_orphans(
    prefetched: IndexMap<String, ModelEntry>,
    keyed: &[CredentialedProvider],
) -> IndexMap<String, ModelEntry> {
    prefetched
        .into_iter()
        .filter(|(key, entry)| {
            let id = entry.info.id.as_deref().unwrap_or(key.as_str());
            let Some((prefix, _)) = id.split_once('/') else {
                return true;
            };
            let known = kigi_models::PlatformId::parse(prefix).is_some()
                || keyed.iter().any(|k| k.provider.name == prefix);
            if !known {
                tracing::warn!(model_key = %key, "dropping a fetched entry of an unknown or keyless provider");
            }
            known
        })
        .collect()
}

/// Stamps each entry with its provider's key, in memory only.
pub(crate) fn stamp_credentials(
    resolved: &mut IndexMap<String, ModelEntry>,
    keyed: &[CredentialedProvider],
) {
    for (key, entry) in resolved.iter_mut() {
        let id = entry.info.id.as_deref().unwrap_or(key.as_str());
        let Some((name, _)) = id.split_once('/') else {
            continue;
        };
        let Some(owner) = keyed.iter().find(|k| k.provider.name == name) else {
            continue;
        };
        if entry.api_key.is_some() {
            continue;
        }
        if !crate::util::matches_trusted_base_url(&entry.info.base_url, &owner.provider.base_url) {
            tracing::warn!(
                model_key = %key, provider = %name,
                "entry points away from its custom provider; key not stamped"
            );
            continue;
        }
        entry.api_key = Some(owner.key().to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::models::{ModelFetchAuth, PlatformApiKeys};
    use crate::agent::models_fetch::fetch_models_blocking as fetch_all;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn credentialed(name: &str, api: CustomApi, base: &str, key: &str) -> CredentialedProvider {
        CredentialedProvider::for_test(CustomProvider::new(name, api, base).unwrap(), key)
    }

    async fn fetch_on_thread(
        c: CredentialedProvider,
    ) -> Result<Vec<ModelEntryConfig>, BackendError> {
        tokio::task::spawn_blocking(move || fetch_models_blocking(&c))
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn openai_listing_becomes_chat_completions_entries() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("authorization", "Bearer sk-proxy"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "object": "list",
                "data": [{"id": "m1", "context_length": 32000}, {"id": "m2"}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let base = format!("{}/v1/", server.uri());

        let models = fetch_on_thread(credentialed("proxy", CustomApi::OpenAi, &base, "sk-proxy"))
            .await
            .unwrap();

        let ids: Vec<_> = models.iter().map(|m| m.id.as_deref().unwrap()).collect();
        assert_eq!(ids, ["proxy/m1", "proxy/m2"]);
        assert_eq!(models[0].model, "m1");
        assert_eq!(models[0].context_window.get(), 32_000);
        assert_eq!(
            models[1].context_window.get(),
            crate::agent::models_fetch::DEFAULT_CONTEXT_WINDOW
        );
        assert_eq!(models[0].base_url, format!("{}/v1", server.uri()));
        assert_eq!(
            models[0].api_backend,
            crate::sampling::ApiBackend::ChatCompletions
        );
        assert_eq!(models[0].auth_scheme, None);
        assert_eq!(models[0].description.as_deref(), Some("proxy"));
        assert!(models[0].api_key.is_none() && models[0].env_key.is_none());
        assert!(models[0].supported_in_api);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn anthropic_listing_becomes_messages_entries() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(query_param("limit", "1000"))
            .and(header("x-api-key", "sk-gw"))
            .and(header(
                "anthropic-version",
                kigi_sampling_types::ANTHROPIC_VERSION,
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "id": "claude-x", "display_name": "Claude X",
                    "max_input_tokens": 200000, "max_tokens": 64000
                }],
                "has_more": false
            })))
            .expect(1)
            .mount(&server)
            .await;
        let base = format!("{}/v1", server.uri());

        let models = fetch_on_thread(credentialed("gw", CustomApi::Anthropic, &base, "sk-gw"))
            .await
            .unwrap();

        assert_eq!(models.len(), 1);
        let m = &models[0];
        assert_eq!(m.id.as_deref(), Some("gw/claude-x"));
        assert_eq!(m.name.as_deref(), Some("Claude X"));
        assert_eq!(m.context_window.get(), 200_000);
        assert_eq!(m.max_completion_tokens, Some(64_000));
        assert_eq!(m.api_backend, crate::sampling::ApiBackend::Messages);
        assert_eq!(m.auth_scheme, Some(kigi_sampler::AuthScheme::XApiKey));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn listing_failures_surface_status_and_parse_errors() {
        let server = MockServer::start().await;
        Mock::given(path("/v1/models"))
            .respond_with(ResponseTemplate::new(401).set_body_string("nope"))
            .mount(&server)
            .await;
        let base = format!("{}/v1", server.uri());
        let err = fetch_on_thread(credentialed("proxy", CustomApi::OpenAi, &base, "bad"))
            .await
            .unwrap_err();
        assert!(
            matches!(err, BackendError::RequestFailed { status: 401, .. }),
            "{err}"
        );

        let server = MockServer::start().await;
        Mock::given(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;
        let base = format!("{}/v1", server.uri());
        let err = fetch_on_thread(credentialed("proxy", CustomApi::OpenAi, &base, "k"))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("openai listing parse failed"),
            "{err}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn custom_providers_alone_satisfy_the_platform_fetch() {
        let server = MockServer::start().await;
        Mock::given(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"id": "m1"}]
            })))
            .mount(&server)
            .await;
        let base = format!("{}/v1", server.uri());
        let keys = PlatformApiKeys::test_custom(vec![credentialed(
            "proxy",
            CustomApi::OpenAi,
            &base,
            "sk-origin-secret",
        )]);
        let endpoints = crate::agent::config::EndpointsConfig::default();

        let origin = crate::agent::models_fetch::models_fetch_origin(
            &endpoints,
            ModelFetchAuth::Platforms,
            false,
            &Default::default(),
            &keys,
        );
        assert!(origin.contains(&format!("proxy={base}/models")), "{origin}");
        assert!(
            !origin.contains("sk"),
            "the origin must never carry a key: {origin}"
        );

        let result = tokio::task::spawn_blocking(move || {
            fetch_all(
                &endpoints,
                None,
                &Default::default(),
                ModelFetchAuth::Platforms,
                &keys,
            )
        })
        .await
        .unwrap()
        .expect("a custom provider is a complete catalog source");
        assert_eq!(result.models.len(), 1);
        assert_eq!(result.models[0].id.as_deref(), Some("proxy/m1"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failing_custom_provider_alone_fails_the_fetch() {
        let server = MockServer::start().await;
        Mock::given(path("/v1/models"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let base = format!("{}/v1", server.uri());
        let keys = PlatformApiKeys::test_custom(vec![credentialed(
            "proxy",
            CustomApi::OpenAi,
            &base,
            "sk-origin-secret",
        )]);
        let err = tokio::task::spawn_blocking(move || {
            fetch_all(
                &crate::agent::config::EndpointsConfig::default(),
                None,
                &Default::default(),
                ModelFetchAuth::Platforms,
                &keys,
            )
        })
        .await
        .unwrap()
        .err()
        .expect("500 with no other source must fail");
        assert!(
            matches!(err, BackendError::RequestFailed { status: 500, .. }),
            "{err}"
        );
    }

    fn resolved_entry(provider: &CustomProvider, id: &str, base: &str) -> (String, ModelEntry) {
        let wire: WireModel = serde_json::from_value(serde_json::json!({ "id": id })).unwrap();
        let mut cfg = entry(provider, wire);
        cfg.base_url = base.to_owned();
        let key = cfg.id.clone().unwrap();
        (key, ModelEntry::from_config_entry(&cfg))
    }

    #[test]
    fn stamp_gives_each_entry_only_its_own_providers_key_on_its_own_host() {
        let proxy =
            CustomProvider::new("proxy", CustomApi::OpenAi, "https://p.example/v1").unwrap();
        let gw = CustomProvider::new("gw", CustomApi::Anthropic, "https://g.example/v1").unwrap();
        let mut resolved = IndexMap::new();
        for (key, entry) in [
            resolved_entry(&proxy, "m1", "https://p.example/v1"),
            resolved_entry(&gw, "c1", "https://g.example/v1"),
            resolved_entry(&proxy, "moved", "https://evil.example/v1"),
        ] {
            resolved.insert(key, entry);
        }
        let (own_key, mut own) = resolved_entry(&proxy, "own", "https://p.example/v1");
        own.api_key = Some("per-model".into());
        resolved.insert(own_key, own);
        let snapshot = vec![
            CredentialedProvider::for_test(proxy, "sk-proxy"),
            CredentialedProvider::for_test(gw, "sk-gw"),
        ];

        stamp_credentials(&mut resolved, &snapshot);

        assert_eq!(resolved["proxy/m1"].api_key.as_deref(), Some("sk-proxy"));
        assert_eq!(resolved["gw/c1"].api_key.as_deref(), Some("sk-gw"));
        assert_eq!(
            resolved["proxy/moved"].api_key, None,
            "off-host entry gets nothing"
        );
        assert_eq!(resolved["proxy/own"].api_key.as_deref(), Some("per-model"));
    }

    #[test]
    fn resolve_model_list_stamps_fetched_custom_entries() {
        let raw: toml::Value = toml::from_str(
            "[platforms.proxy]\nbase_url = \"https://p.example/v1\"\napi = \"openai\"\n",
        )
        .unwrap();
        let cfg = crate::agent::config::Config::new_from_toml_cfg(&raw).unwrap();
        let proxy =
            CustomProvider::new("proxy", CustomApi::OpenAi, "https://p.example/v1").unwrap();
        let (key, entry) = resolved_entry(&proxy, "m1", "https://p.example/v1");
        let keys =
            PlatformApiKeys::test_custom(vec![CredentialedProvider::for_test(proxy, "sk-proxy")]);

        let resolved = crate::agent::config::resolve_model_list(
            &cfg,
            Some(IndexMap::from([(key, entry)])),
            &keys,
        );

        let m = resolved
            .get("proxy/m1")
            .expect("fetched entry survives resolution");
        assert_eq!(m.api_key.as_deref(), Some("sk-proxy"));
        assert!(m.has_own_credentials());
        assert!(m.visible_for_auth(false), "API-key users see the entry");
        assert_eq!(
            m.info.api_backend,
            crate::sampling::ApiBackend::ChatCompletions
        );
    }

    #[test]
    fn orphaned_custom_entries_never_enter_the_catalog() {
        let proxy =
            CustomProvider::new("proxy", CustomApi::OpenAi, "https://p.example/v1").unwrap();
        let gone = CustomProvider::new("gone", CustomApi::OpenAi, "https://g.example/v1").unwrap();
        let mut prefetched = IndexMap::new();
        for (key, entry) in [
            resolved_entry(&proxy, "m1", "https://p.example/v1"),
            resolved_entry(&gone, "m2", "https://g.example/v1"),
        ] {
            prefetched.insert(key, entry);
        }
        let registry: WireModel =
            serde_json::from_value(serde_json::json!({ "id": "kimi-k2-turbo-preview" })).unwrap();
        let registry = ModelEntry::from_config_entry(
            &crate::agent::models_fetch::platform_wire_model_to_entry(
                kigi_models::PlatformId::MoonshotCn,
                registry,
                "https://api.moonshot.cn/v1",
            ),
        );
        prefetched.insert("moonshot-cn/kimi-k2-turbo-preview".into(), registry);
        let keyed = vec![CredentialedProvider::for_test(proxy, "sk-proxy")];
        let cfg = crate::agent::config::Config::default();

        let resolved = crate::agent::config::resolve_model_list(
            &cfg,
            Some(prefetched.clone()),
            &PlatformApiKeys::test_custom(keyed.clone()),
        );
        let ids: Vec<_> = resolved.keys().map(String::as_str).collect();
        assert_eq!(ids, ["proxy/m1", "moonshot-cn/kimi-k2-turbo-preview"]);

        let unkeyed =
            crate::agent::config::resolve_model_list(&cfg, Some(prefetched), &Default::default());
        assert!(
            !unkeyed.contains_key("proxy/m1") && !unkeyed.contains_key("gone/m2"),
            "a keyless provider leaves no entries behind"
        );
    }

    #[test]
    fn custom_endpoint_mode_keeps_slashed_remote_ids() {
        let raw: toml::Value =
            toml::from_str("[endpoints]\nmodels_base_url = \"https://m.example/v1\"\n").unwrap();
        let cfg = crate::agent::config::Config::new_from_toml_cfg(&raw).unwrap();
        let other = CustomProvider::new("org", CustomApi::OpenAi, "https://o.example/v1").unwrap();
        let (key, entry) = resolved_entry(&other, "llama", "https://o.example/v1");

        let resolved = crate::agent::config::resolve_model_list(
            &cfg,
            Some(IndexMap::from([(key, entry)])),
            &Default::default(),
        );

        assert!(
            resolved.contains_key("org/llama"),
            "{:?}",
            resolved.keys().collect::<Vec<_>>()
        );
    }
}
