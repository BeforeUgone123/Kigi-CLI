//! `[platforms.<name>]` tables that declare custom providers.

use kigi_models::custom::{CustomApi, CustomProvider};

use crate::agent::config::{PlatformCredentialConfig, PlatformsConfig};

mod fetch;
mod save;

pub(crate) use fetch::{drop_orphans, fetch_models_blocking, stamp_credentials};
pub use save::save_custom_provider;

/// Tables setting `base_url` or `api`, valid or not.
fn declarations(platforms: &PlatformsConfig) -> Vec<(&str, Result<CustomProvider, String>)> {
    platforms
        .entries
        .iter()
        .filter(|(_, entry)| entry.base_url.is_some() || entry.api.is_some())
        .map(|(name, entry)| (name.as_str(), parse_declaration(name, entry)))
        .collect()
}

fn parse_declaration(
    name: &str,
    entry: &PlatformCredentialConfig,
) -> Result<CustomProvider, String> {
    let api = entry
        .api
        .as_deref()
        .ok_or("api is required (\"openai\" or \"anthropic\")")?;
    let api = CustomApi::parse(api)
        .ok_or_else(|| format!("api {api:?} is not \"openai\" or \"anthropic\""))?;
    let base_url = entry.base_url.as_deref().ok_or("base_url is required")?;
    CustomProvider::new(name, api, base_url).map_err(|e| e.to_string())
}

/// The valid declarations.
pub(crate) fn providers(platforms: &PlatformsConfig) -> Vec<CustomProvider> {
    declarations(platforms)
        .into_iter()
        .filter_map(|(_, parsed)| parsed.ok())
        .collect()
}

/// Base URLs of the valid declarations.
pub(crate) fn bases(platforms: &PlatformsConfig) -> Vec<String> {
    providers(platforms)
        .into_iter()
        .map(|p| p.base_url)
        .collect()
}

/// True when the table declares a provider, valid or not.
pub(crate) fn is_declared(entry: &PlatformCredentialConfig) -> bool {
    entry.base_url.is_some() || entry.api.is_some()
}

/// Warns once per refused declaration; the table stays inert.
pub(crate) fn warn_invalid_declarations(platforms: &PlatformsConfig) {
    for (name, parsed) in declarations(platforms) {
        if let Err(reason) = parsed {
            tracing::warn!(platform = %name, %reason, "[platforms.{name}] is not a valid custom provider; ignored");
        }
    }
}

/// A declared provider with a key; `Debug` hides the key.
#[derive(Clone)]
pub(crate) struct CredentialedProvider {
    pub(crate) provider: CustomProvider,
    key: String,
}

impl CredentialedProvider {
    pub(crate) fn key(&self) -> &str {
        &self.key
    }

    #[cfg(test)]
    pub(crate) fn for_test(provider: CustomProvider, key: &str) -> Self {
        Self {
            provider,
            key: key.to_owned(),
        }
    }
}

impl std::fmt::Debug for CredentialedProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialedProvider")
            .field("provider", &self.provider)
            .field("key", &"<set>")
            .finish()
    }
}

/// Key order: auth.json scope, then the table's `api_key`.
pub(crate) fn credentialed(
    platforms: &PlatformsConfig,
    stored: impl Fn(&str) -> Option<String>,
) -> Vec<CredentialedProvider> {
    providers(platforms)
        .into_iter()
        .filter_map(|provider| {
            let key = stored(&provider.name)
                .filter(|k| !k.trim().is_empty())
                .or_else(|| {
                    platforms
                        .entries
                        .get(&provider.name)
                        .and_then(|e| e.api_key.clone())
                        .filter(|k| !k.trim().is_empty())
                })?;
            Some(CredentialedProvider { provider, key })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn platforms(toml_src: &str) -> PlatformsConfig {
        toml::from_str(toml_src).expect("platforms table parses")
    }

    #[test]
    fn declarations_read_api_and_base_url() {
        let cfg = platforms(
            r#"
            [proxy]
            base_url = "https://h.example/v1/"
            api = "openai"
            [gw]
            base_url = "https://g.example/v1"
            api = "Anthropic"
            "#,
        );
        let found = providers(&cfg);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].name, "proxy");
        assert_eq!(found[0].api, CustomApi::OpenAi);
        assert_eq!(found[0].base_url, "https://h.example/v1");
        assert_eq!(found[1].api, CustomApi::Anthropic);
    }

    #[test]
    fn invalid_declarations_are_refused_with_a_reason() {
        let cfg = platforms(
            r#"
            [no-api]
            base_url = "https://h.example/v1"
            [no-url]
            api = "openai"
            [bad-api]
            base_url = "https://h.example/v1"
            api = "responses"
            [bad-url]
            base_url = "h.example"
            api = "openai"
            [openai]
            base_url = "https://h.example/v1"
            api = "openai"
            [Upper]
            base_url = "https://h.example/v1"
            api = "openai"
            "#,
        );
        assert!(providers(&cfg).is_empty());
        let reasons: Vec<String> = declarations(&cfg)
            .into_iter()
            .map(|(_, parsed)| parsed.expect_err("every declaration is invalid"))
            .collect();
        assert_eq!(reasons.len(), 6);
        assert!(reasons[0].contains("api is required"));
        assert!(reasons[1].contains("base_url is required"));
        assert!(reasons[2].contains("responses"));
        assert!(reasons[3].contains("base_url"));
        assert!(reasons[4].contains("built-in platform id"));
        assert!(reasons[5].contains("provider name"));
    }

    #[test]
    fn a_table_with_only_a_key_is_not_a_declaration() {
        let cfg = platforms("[moonshot-cn]\napi_key = \"sk-x\"\n");
        assert!(declarations(&cfg).is_empty());
        assert!(!is_declared(&cfg.entries["moonshot-cn"]));
    }

    #[test]
    fn key_prefers_auth_json_over_config_and_blank_is_unset() {
        let cfg = platforms(
            r#"
            [proxy]
            base_url = "https://h.example/v1"
            api = "openai"
            api_key = "from-config"
            [blank]
            base_url = "https://b.example/v1"
            api = "openai"
            api_key = "  "
            [keyless]
            base_url = "https://k.example/v1"
            api = "openai"
            "#,
        );
        let stored = |name: &str| (name == "proxy").then(|| "from-auth-json".to_owned());
        let found = credentialed(&cfg, stored);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].provider.name, "proxy");
        assert_eq!(found[0].key(), "from-auth-json");

        let found = credentialed(&cfg, |_| None);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key(), "from-config");
    }

    #[test]
    fn debug_never_prints_the_key() {
        let cfg = platforms(
            "[proxy]\nbase_url = \"https://h.example/v1\"\napi = \"openai\"\napi_key = \"sk-secret\"\n",
        );
        let found = credentialed(&cfg, |_| None);
        assert!(!format!("{found:?}").contains("sk-secret"));
    }
}
