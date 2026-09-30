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
    tracing::info!(provider = %provider.name, api = provider.api.as_str(), url = %provider.models_url(), "fetching custom provider models");
    let body = listing_body(provider, credentialed.key())?;
    let wire = parse_listing(provider, &body).map_err(|e| BackendError::RequestFailed {
        // No error content: serde quotes the offending value, a reflected key risk.
        status: 200,
        body: format!(
            "{} listing parse failed at line {} column {}",
            provider.api.as_str(),
            e.line(),
            e.column()
        ),
    })?;
    if wire.is_empty() {
        tracing::warn!(provider = %provider.name, "custom provider listed no models");
    }
    Ok(wire.into_iter().map(|w| entry(provider, w)).collect())
}

/// The listing GET with the dialect's key header; status-gated body.
fn listing_body(provider: &CustomProvider, key: &str) -> Result<String, BackendError> {
    let url = provider.models_url();
    let client = crate::http::shared_blocking_client();
    let send = |bearer_fallback: bool| {
        let request = match (provider.api, bearer_fallback) {
            (CustomApi::OpenAi, _) => client
                .get(&url)
                .header("Authorization", format!("Bearer {key}")),
            (CustomApi::Anthropic, false) => client
                .get(&url)
                .header("x-api-key", key)
                .header("anthropic-version", kigi_sampling_types::ANTHROPIC_VERSION),
            (CustomApi::Anthropic, true) => client
                .get(&url)
                .header("Authorization", format!("Bearer {key}"))
                .header("anthropic-version", kigi_sampling_types::ANTHROPIC_VERSION),
        };
        request.send()
    };
    let mut response = send(false)?;
    // Relays may gate the listing on Bearer even for the Messages wire.
    if response.status().as_u16() == 401 && provider.api == CustomApi::Anthropic {
        tracing::info!(provider = %provider.name, "listing 401 with x-api-key; retrying Bearer");
        response = send(true)?;
    }
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = response.text().unwrap_or_default();
        return Err(BackendError::RequestFailed { status, body });
    }
    Ok(response.text()?)
}

fn parse_listing(
    provider: &CustomProvider,
    body: &str,
) -> Result<Vec<WireModel>, serde_json::Error> {
    match provider.api {
        CustomApi::OpenAi => kigi_models::parse_openai_listing(body),
        CustomApi::Anthropic => kigi_models::parse_anthropic_listing(body),
    }
}

/// The login-step listing result; `provider` carries the adopted base URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginFetch {
    pub provider: CustomProvider,
    pub model_ids: Vec<String>,
}

/// Login listing fetch; probes `{base}/v1` once on a 404 or unparsable body and adopts it.
pub fn fetch_listing_for_login(provider: &CustomProvider, key: &str) -> Result<LoginFetch, String> {
    match list_wire(provider, key) {
        Ok(model_ids) => Ok(LoginFetch {
            provider: provider.clone(),
            model_ids,
        }),
        Err(first) => {
            if !first.retryable() || has_version_tail(&provider.base_url) {
                return Err(first.into_message());
            }
            let probed = CustomProvider::new(
                &provider.name,
                provider.api,
                &format!("{}/v1", provider.base_url),
            )
            .map_err(|e| e.to_string())?;
            tracing::info!(
                provider = %provider.name,
                base_url = %probed.base_url,
                "custom provider listing retrying with a /v1 base"
            );
            match list_wire(&probed, key) {
                Ok(model_ids) => Ok(LoginFetch {
                    provider: probed,
                    model_ids,
                }),
                Err(second) => Err(format!(
                    "{}; {} also failed: {}",
                    first.into_message(),
                    probed.models_url(),
                    second.into_message()
                )),
            }
        }
    }
}

/// A listing failure; `retryable` marks wrong-base-URL shapes only.
enum ListFail {
    Retryable(String),
    Fatal(String),
}

impl ListFail {
    fn retryable(&self) -> bool {
        matches!(self, Self::Retryable(_))
    }

    fn into_message(self) -> String {
        match self {
            Self::Retryable(m) | Self::Fatal(m) => m,
        }
    }
}

fn list_wire(provider: &CustomProvider, key: &str) -> Result<Vec<String>, ListFail> {
    let url = provider.models_url();
    let body = listing_body(provider, key).map_err(|e| match &e {
        BackendError::RequestFailed { status, .. } => {
            // No body: a reflected error could echo the key.
            let msg = format!("{url} answered HTTP {status}");
            if *status == 404 {
                ListFail::Retryable(msg)
            } else {
                ListFail::Fatal(msg)
            }
        }
        BackendError::Network(_) => ListFail::Fatal(format!("Couldn't reach {url}: {e}")),
        BackendError::Auth(_) => ListFail::Fatal(e.to_string()),
    })?;
    let wire = parse_listing(provider, &body).map_err(|e| {
        ListFail::Retryable(format!(
            "{url} did not serve a {} model listing (invalid JSON at line {} column {})",
            provider.api.as_str(),
            e.line(),
            e.column()
        ))
    })?;
    Ok(wire.into_iter().map(|w| w.id).collect())
}

/// True when the base URL's last path segment is a version like `v1`.
fn has_version_tail(base_url: &str) -> bool {
    let Some(tail) = base_url.rsplit('/').next() else {
        return false;
    };
    let digits = tail.strip_prefix('v').unwrap_or("");
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

/// A selected id the listing never served becomes an entry with wire defaults.
pub(crate) fn manual_entry(provider: &CustomProvider, id: &str) -> ModelEntryConfig {
    entry(provider, WireModel::bare(id.to_owned()))
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

    async fn login_fetch_on_thread(
        provider: CustomProvider,
        key: &str,
    ) -> Result<LoginFetch, String> {
        let key = key.to_owned();
        tokio::task::spawn_blocking(move || fetch_listing_for_login(&provider, &key))
            .await
            .unwrap()
    }

    #[test]
    fn version_tail_detection() {
        assert!(has_version_tail("https://h.example/v1"));
        assert!(has_version_tail("https://h.example/api/v2"));
        assert!(!has_version_tail("https://h.example"));
        assert!(!has_version_tail("https://h.example/coding"));
        assert!(!has_version_tail("https://h.example/v"));
        assert!(!has_version_tail("https://h.example/v1x"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn login_fetch_returns_the_served_ids() {
        let server = MockServer::start().await;
        Mock::given(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"id": "m1"}, {"id": "m2"}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let base = format!("{}/v1", server.uri());
        let provider = CustomProvider::new("proxy", CustomApi::OpenAi, &base).unwrap();

        let fetch = login_fetch_on_thread(provider, "sk-k").await.unwrap();

        assert_eq!(fetch.provider.base_url, base);
        assert_eq!(fetch.model_ids, ["m1", "m2"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn login_fetch_probes_v1_on_a_404_and_adopts_the_base() {
        let server = MockServer::start().await;
        Mock::given(path("/coding/models"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/coding/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"id": "m1"}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let base = format!("{}/coding", server.uri());
        let provider = CustomProvider::new("proxy", CustomApi::OpenAi, &base).unwrap();

        let fetch = login_fetch_on_thread(provider, "sk-k").await.unwrap();

        assert_eq!(fetch.provider.base_url, format!("{base}/v1"));
        assert_eq!(fetch.model_ids, ["m1"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn login_fetch_probes_v1_when_the_200_body_is_not_a_listing() {
        // A WAF answers every path with an HTML challenge page at HTTP 200.
        let server = MockServer::start().await;
        Mock::given(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>challenge</html>"))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"id": "m1"}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let provider = CustomProvider::new("proxy", CustomApi::OpenAi, &server.uri()).unwrap();

        let fetch = login_fetch_on_thread(provider, "sk-k").await.unwrap();

        assert_eq!(fetch.provider.base_url, format!("{}/v1", server.uri()));
        assert_eq!(fetch.model_ids, ["m1"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn login_fetch_does_not_probe_auth_failures_or_versioned_bases() {
        let server = MockServer::start().await;
        Mock::given(path("/models"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        let provider = CustomProvider::new("proxy", CustomApi::OpenAi, &server.uri()).unwrap();
        let err = login_fetch_on_thread(provider, "bad").await.unwrap_err();
        assert!(err.contains("401"), "{err}");

        let server = MockServer::start().await;
        Mock::given(path("/v2/models"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        let base = format!("{}/v2", server.uri());
        let provider = CustomProvider::new("proxy", CustomApi::OpenAi, &base).unwrap();
        let err = login_fetch_on_thread(provider, "sk-k").await.unwrap_err();
        assert!(err.contains("404") && !err.contains("also failed"), "{err}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn login_fetch_reports_both_hops_when_the_probe_fails_too() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .expect(2)
            .mount(&server)
            .await;
        let provider = CustomProvider::new("proxy", CustomApi::OpenAi, &server.uri()).unwrap();

        let err = login_fetch_on_thread(provider, "sk-k").await.unwrap_err();

        assert!(err.contains("also failed"), "{err}");
        assert!(!err.contains("sk-k"), "{err}");
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
    async fn anthropic_listing_falls_back_to_bearer_on_a_401() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("x-api-key", "sk-gw"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("authorization", "Bearer sk-gw"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"id": "c1"}], "has_more": false
            })))
            .expect(1)
            .mount(&server)
            .await;
        let base = format!("{}/v1", server.uri());

        let models = fetch_on_thread(credentialed("gw", CustomApi::Anthropic, &base, "sk-gw"))
            .await
            .unwrap();

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id.as_deref(), Some("gw/c1"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn openai_listing_never_retries_on_a_401() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        let base = format!("{}/v1", server.uri());
        let err = fetch_on_thread(credentialed("proxy", CustomApi::OpenAi, &base, "bad"))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            BackendError::RequestFailed { status: 401, .. }
        ));
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
