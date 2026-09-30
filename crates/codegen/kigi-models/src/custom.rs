//! User-declared providers: a name, a base URL, and a wire.

use crate::{ListingDialect, PlatformChatCompat, PlatformKeyHeader, PlatformWireApi};

const MAX_NAME_LEN: usize = 32;

/// Wire dialect of a custom provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomApi {
    /// Chat Completions with a Bearer key.
    OpenAi,
    /// Messages with an `x-api-key` header.
    Anthropic,
}

impl CustomApi {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "openai" => Some(Self::OpenAi),
            "anthropic" => Some(Self::Anthropic),
            _ => None,
        }
    }

    /// Login picker wording.
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI compatible",
            Self::Anthropic => "Anthropic compatible",
        }
    }

    pub fn wire_api(self) -> PlatformWireApi {
        match self {
            Self::OpenAi => PlatformWireApi::ChatCompletions,
            Self::Anthropic => PlatformWireApi::Messages,
        }
    }

    pub fn listing(self) -> ListingDialect {
        match self {
            Self::OpenAi => ListingDialect::OpenAi,
            Self::Anthropic => ListingDialect::Anthropic,
        }
    }

    pub fn key_header(self) -> PlatformKeyHeader {
        match self {
            Self::OpenAi => PlatformKeyHeader::Bearer,
            Self::Anthropic => PlatformKeyHeader::XApiKey,
        }
    }

    pub fn chat_compat(self) -> PlatformChatCompat {
        PlatformChatCompat::Passthrough
    }
}

/// Why a custom provider definition was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomProviderError {
    Name(String),
    BaseUrl(String),
}

impl std::fmt::Display for CustomProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Name(reason) => write!(f, "provider name: {reason}"),
            Self::BaseUrl(reason) => write!(f, "provider base_url: {reason}"),
        }
    }
}

impl std::error::Error for CustomProviderError {}

/// A validated provider definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomProvider {
    pub name: String,
    pub api: CustomApi,
    /// Without a trailing slash.
    pub base_url: String,
}

impl CustomProvider {
    pub fn new(name: &str, api: CustomApi, base_url: &str) -> Result<Self, CustomProviderError> {
        validate_name(name)?;
        Ok(Self {
            name: name.to_owned(),
            api,
            base_url: normalize_base_url(base_url)?,
        })
    }

    /// The listing endpoint, with the dialect's query.
    pub fn models_url(&self) -> String {
        match self.api {
            CustomApi::OpenAi => format!("{}/models", self.base_url),
            // Anthropic paginates (default 20); 1000 is the documented max.
            CustomApi::Anthropic => format!("{}/models?limit=1000", self.base_url),
        }
    }

    /// Catalog key `{name}/{model_id}`.
    pub fn managed_model_key(&self, model_id: &str) -> String {
        format!("{}/{model_id}", self.name)
    }
}

/// Names become auth.json scopes and catalog key prefixes.
pub fn validate_name(name: &str) -> Result<(), CustomProviderError> {
    let err = |reason: &str| Err(CustomProviderError::Name(reason.to_owned()));
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return err("must be 1 to 32 characters");
    }
    let mut chars = name.chars();
    if !chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        return err("must start with a lowercase letter or digit");
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
        return err("may hold only lowercase letters, digits, '-' and '_'");
    }
    if crate::PlatformId::parse(name).is_some() {
        return err("is a built-in platform id");
    }
    Ok(())
}

/// Accepts `http(s)://host[:port][/path]`; the URL is logged, so no secrets.
pub fn normalize_base_url(raw: &str) -> Result<String, CustomProviderError> {
    let err = |reason: &str| Err(CustomProviderError::BaseUrl(reason.to_owned()));
    let url = raw.trim().trim_end_matches('/');
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return err("must not contain whitespace");
    }
    let rest = ["https://", "http://"].iter().find_map(|scheme| {
        url.get(..scheme.len())
            .filter(|head| head.eq_ignore_ascii_case(scheme))
            .map(|_| &url[scheme.len()..])
    });
    let Some(rest) = rest else {
        return err("must start with http:// or https://");
    };
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty() {
        return err("has no host");
    }
    if authority.contains('@') {
        return err("must not carry credentials");
    }
    if url.contains('?') || url.contains('#') {
        return err("must not carry a query or fragment");
    }
    Ok(url.to_owned())
}

/// A manually added model id: no whitespace, no controls, bounded length.
pub fn validate_model_id(id: &str) -> Result<(), CustomProviderError> {
    let err = |reason: &str| Err(CustomProviderError::Name(reason.to_owned()));
    if id.is_empty() || id.len() > 128 {
        return err("model id must be 1 to 128 characters");
    }
    if id.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return err("model id must not contain whitespace");
    }
    Ok(())
}

/// A valid provider name derived from a host.
pub fn default_name(base_url: &str) -> String {
    let host = base_url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .and_then(|authority| authority.split(':').next())
        .unwrap_or("")
        .to_ascii_lowercase();
    let labels: Vec<&str> = host
        .split('.')
        .filter(|label| !label.is_empty() && !matches!(*label, "api" | "www"))
        .collect();
    let joined: String = labels
        .join("-")
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .take(MAX_NAME_LEN)
        .collect();
    let name = joined.trim_matches('-').to_owned();
    if validate_name(&name).is_ok() {
        name
    } else {
        "custom".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_names_come_from_the_host() {
        for (url, expected) in [
            ("https://api.together.xyz/v1", "together-xyz"),
            ("https://openrouter.ai/api/v1", "openrouter-ai"),
            ("http://localhost:11434/v1", "localhost"),
            ("http://127.0.0.1:8080/v1", "127-0-0-1"),
            ("https://api.openai.com/v1", "openai-com"),
            (
                "https://gateway.internal.example.org/anthropic",
                "gateway-internal-example-org",
            ),
            ("https://API.Example.COM/v1", "example-com"),
            ("not a url", "custom"),
            ("https://api/v1", "custom"),
            ("https://[::1]:9000/v1", "custom"),
        ] {
            assert_eq!(default_name(url), expected, "{url}");
            assert!(validate_name(&default_name(url)).is_ok(), "{url}");
        }
        assert!(default_name(&format!("https://{}.example/v1", "a".repeat(60))).len() <= 32);
    }

    #[test]
    fn api_round_trips_and_maps_to_the_wire() {
        for api in [CustomApi::OpenAi, CustomApi::Anthropic] {
            assert_eq!(CustomApi::parse(api.as_str()), Some(api));
        }
        assert_eq!(CustomApi::parse(" OpenAI "), Some(CustomApi::OpenAi));
        assert_eq!(CustomApi::parse("responses"), None);
        assert_eq!(
            CustomApi::OpenAi.wire_api(),
            PlatformWireApi::ChatCompletions
        );
        assert_eq!(CustomApi::Anthropic.wire_api(), PlatformWireApi::Messages);
        assert_eq!(CustomApi::OpenAi.key_header(), PlatformKeyHeader::Bearer);
        assert_eq!(
            CustomApi::Anthropic.key_header(),
            PlatformKeyHeader::XApiKey
        );
        assert_eq!(CustomApi::OpenAi.listing(), ListingDialect::OpenAi);
        assert_eq!(CustomApi::Anthropic.listing(), ListingDialect::Anthropic);
        for api in [CustomApi::OpenAi, CustomApi::Anthropic] {
            assert_eq!(api.chat_compat(), PlatformChatCompat::Passthrough);
        }
    }

    #[test]
    fn names_follow_the_rules() {
        for ok in ["proxy", "my-proxy_2", "0x", "a"] {
            assert!(validate_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "Proxy",
            "pRoxy",
            "-x",
            "a/b",
            "a b",
            "a:b",
            "é",
            &"x".repeat(33),
        ] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
        for id in crate::PlatformId::ALL {
            assert!(validate_name(id.as_str()).is_err(), "{}", id.as_str());
        }
    }

    #[test]
    fn base_urls_are_normalized_or_refused() {
        assert_eq!(
            normalize_base_url(" https://api.example.com/v1/ ").unwrap(),
            "https://api.example.com/v1"
        );
        assert_eq!(
            normalize_base_url("HTTP://localhost:11434/v1").unwrap(),
            "HTTP://localhost:11434/v1"
        );
        for bad in [
            "",
            "example.com/v1",
            "ftp://example.com",
            "https://",
            "https:///v1",
            "https://user:pw@example.com/v1",
            "https://example.com/v1?key=x",
            "https://example.com/v1#frag",
            "https://exa mple.com",
            "https://h\u{0}x",
            "ht€ps://example.com",
            "€https://example.com",
            "https:/€",
            "http:/€",
        ] {
            assert!(normalize_base_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn listing_urls_and_keys_follow_the_dialect() {
        let openai =
            CustomProvider::new("proxy", CustomApi::OpenAi, "https://h.example/v1/").unwrap();
        assert_eq!(openai.models_url(), "https://h.example/v1/models");
        assert_eq!(openai.managed_model_key("gpt-x"), "proxy/gpt-x");
        let anthropic =
            CustomProvider::new("claude-gw", CustomApi::Anthropic, "https://h.example/v1").unwrap();
        assert_eq!(
            anthropic.models_url(),
            "https://h.example/v1/models?limit=1000"
        );
    }

    #[test]
    fn provider_new_reports_which_field_failed() {
        assert!(matches!(
            CustomProvider::new("openai", CustomApi::OpenAi, "https://h.example/v1"),
            Err(CustomProviderError::Name(_))
        ));
        assert!(matches!(
            CustomProvider::new("proxy", CustomApi::OpenAi, "h.example"),
            Err(CustomProviderError::BaseUrl(_))
        ));
    }
}
