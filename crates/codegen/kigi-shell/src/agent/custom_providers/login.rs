//! Login for custom providers: picker rows, method ids, key validation.

use agent_client_protocol as acp;
use kigi_models::custom::CustomApi;

use super::CredentialedProvider;

const PICKER_PREFIX: &str = "custom-";
const LOGIN_PREFIX: &str = "custom:";
const ALL_APIS: [CustomApi; 2] = [CustomApi::OpenAi, CustomApi::Anthropic];

/// The picker row for one wire.
pub fn picker_method(api: CustomApi) -> acp::AuthMethod {
    acp::AuthMethod::Agent(
        acp::AuthMethodAgent::new(
            acp::AuthMethodId::new(format!("{PICKER_PREFIX}{}", api.as_str())),
            format!("Custom provider ({})", api.label()),
        )
        .description(Some("Your own base URL and API key".to_owned())),
    )
}

/// Both picker rows, in picker order.
pub fn picker_methods() -> Vec<acp::AuthMethod> {
    ALL_APIS.into_iter().map(picker_method).collect()
}

/// The wire behind a picker row id.
pub fn picker_api(id: &str) -> Option<CustomApi> {
    let suffix = id.strip_prefix(PICKER_PREFIX)?;
    ALL_APIS.into_iter().find(|api| api.as_str() == suffix)
}

/// The id `authenticate` takes once the provider is saved.
pub fn login_method_id(name: &str) -> acp::AuthMethodId {
    acp::AuthMethodId::new(format!("{LOGIN_PREFIX}{name}"))
}

/// The provider name inside an `authenticate` method id.
pub fn login_provider_name(id: &str) -> Option<&str> {
    id.strip_prefix(LOGIN_PREFIX)
}

/// Probes the listing endpoint; errors never hold the key.
pub(crate) async fn validate_key(credentialed: &CredentialedProvider) -> Result<(), String> {
    let provider = &credentialed.provider;
    let url = provider.models_url();
    let client = crate::http::shared_client();
    let send = |bearer_fallback: bool| {
        let request = match (provider.api, bearer_fallback) {
            (CustomApi::OpenAi, _) => client
                .get(&url)
                .header("Authorization", format!("Bearer {}", credentialed.key())),
            (CustomApi::Anthropic, false) => client
                .get(&url)
                .header("x-api-key", credentialed.key())
                .header("anthropic-version", kigi_sampling_types::ANTHROPIC_VERSION),
            (CustomApi::Anthropic, true) => client
                .get(&url)
                .header("Authorization", format!("Bearer {}", credentialed.key()))
                .header("anthropic-version", kigi_sampling_types::ANTHROPIC_VERSION),
        };
        request.send()
    };
    let mut response = send(false)
        .await
        .map_err(|e| format!("Couldn't reach {url}: {e}"))?;
    // Mirrors listing_body: relays may gate the listing on Bearer.
    if response.status().as_u16() == 401 && provider.api == CustomApi::Anthropic {
        tracing::info!(provider = %provider.name, "validation 401 with x-api-key; retrying Bearer");
        response = send(true)
            .await
            .map_err(|e| format!("Couldn't reach {url}: {e}"))?;
    }
    let status = response.status().as_u16();
    match status {
        200..=299 => Ok(()),
        401 | 403 => Err(format!(
            "{} rejected the API key (HTTP {status})",
            provider.name
        )),
        _ => Err(format!("{url} answered HTTP {status}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kigi_models::custom::CustomProvider;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn credentialed(api: CustomApi, base: &str, key: &str) -> CredentialedProvider {
        CredentialedProvider::for_test(CustomProvider::new("proxy", api, base).unwrap(), key)
    }

    #[test]
    fn picker_ids_round_trip_and_reject_lookalikes() {
        for api in ALL_APIS {
            let method = picker_method(api);
            assert_eq!(picker_api(method.id().0.as_ref()), Some(api));
        }
        assert_eq!(picker_api("custom-responses"), None);
        assert_eq!(picker_api("custom-OpenAI"), None);
        assert_eq!(picker_api("custom:proxy"), None);
        assert_eq!(picker_api("moonshot-cn"), None);
        assert_eq!(picker_methods().len(), 2);
    }

    #[test]
    fn login_ids_carry_the_provider_name() {
        let id = login_method_id("my-proxy");
        assert_eq!(id.0.as_ref(), "custom:my-proxy");
        assert_eq!(login_provider_name(id.0.as_ref()), Some("my-proxy"));
        assert_eq!(login_provider_name("custom-openai"), None);
        assert_eq!(login_provider_name("proxy"), None);
    }

    #[tokio::test]
    async fn validation_sends_the_dialects_key_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("authorization", "Bearer sk-openai"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;
        let base = format!("{}/v1", server.uri());
        validate_key(&credentialed(CustomApi::OpenAi, &base, "sk-openai"))
            .await
            .unwrap();

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("x-api-key", "sk-anthropic"))
            .and(header(
                "anthropic-version",
                kigi_sampling_types::ANTHROPIC_VERSION,
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;
        let base = format!("{}/v1", server.uri());
        validate_key(&credentialed(CustomApi::Anthropic, &base, "sk-anthropic"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn validation_falls_back_to_bearer_for_anthropic_relays() {
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
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;
        let base = format!("{}/v1", server.uri());
        validate_key(&credentialed(CustomApi::Anthropic, &base, "sk-gw"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn validation_names_the_failure_without_the_key() {
        let server = MockServer::start().await;
        for (status, path_suffix) in [(401, "a"), (403, "b"), (404, "c"), (500, "d")] {
            Mock::given(path(format!("/{path_suffix}/models")))
                .respond_with(ResponseTemplate::new(status))
                .mount(&server)
                .await;
        }
        let check = |suffix: &str| {
            let base = format!("{}/{suffix}", server.uri());
            async move {
                validate_key(&credentialed(CustomApi::OpenAi, &base, "sk-secret-key"))
                    .await
                    .unwrap_err()
            }
        };
        assert!(check("a").await.contains("rejected the API key (HTTP 401)"));
        assert!(check("b").await.contains("rejected the API key (HTTP 403)"));
        assert!(check("c").await.contains("answered HTTP 404"));
        assert!(check("d").await.contains("answered HTTP 500"));

        let err = validate_key(&credentialed(
            CustomApi::OpenAi,
            "http://127.0.0.1:1/v1",
            "sk-secret-key",
        ))
        .await
        .unwrap_err();
        assert!(err.contains("Couldn't reach"), "{err}");
        assert!(!err.contains("sk-secret-key"), "{err}");
    }
}
