use anyhow::{Context, Result, anyhow};
use kigi_models::custom::CustomProvider;
use toml::Value as TomlValue;
use toml::map::Map as TomlMap;

/// Writes `[platforms.<name>]` to config.toml and the key to auth.json.
pub async fn save_custom_provider(provider: &CustomProvider, api_key: &str) -> Result<()> {
    let provider = provider.clone();
    let api_key = api_key.to_owned();
    let _guard = crate::util::config::SAVE_LOCK.lock().await;
    tokio::task::spawn_blocking(move || {
        save_custom_provider_in(
            &crate::util::config::user_config_path(),
            &crate::util::kigi_home::kigi_home(),
            &provider,
            &api_key,
        )
    })
    .await
    .map_err(|e| anyhow!("custom provider save task: {e}"))?
}

/// Config first; a keyless definition is inert.
pub(crate) fn save_custom_provider_in(
    config_path: &std::path::Path,
    kigi_home: &std::path::Path,
    provider: &CustomProvider,
    api_key: &str,
) -> Result<()> {
    let api_key = api_key.trim();
    anyhow::ensure!(!api_key.is_empty(), "API key must not be empty");
    let raw = crate::util::config::read_to_string_or_empty(config_path)
        .with_context(|| format!("reading {}", config_path.display()))?;
    let mut root: TomlValue = toml::from_str(&raw).map_err(|e| {
        anyhow!(
            "refusing to overwrite unparseable {}: {e}; fix the syntax error first",
            config_path.display()
        )
    })?;
    upsert_definition(&mut root, provider)?;
    let dest = crate::util::config::config_write_dest(config_path)?;
    crate::util::config::atomic_write_string(&dest, &toml::to_string_pretty(&root)?)
        .with_context(|| format!("writing {}", dest.display()))?;
    crate::auth::store_scoped_api_key(kigi_home, &provider.name, api_key)
        .with_context(|| format!("saving the key for {} to auth.json", provider.name))?;
    tracing::info!(
        provider = %provider.name,
        api = provider.api.as_str(),
        base_url = %provider.base_url,
        "custom provider saved"
    );
    Ok(())
}

fn upsert_definition(root: &mut TomlValue, provider: &CustomProvider) -> Result<()> {
    let top = root
        .as_table_mut()
        .ok_or_else(|| anyhow!("config root is not a table"))?;
    let platforms = top
        .entry("platforms")
        .or_insert_with(|| TomlValue::Table(TomlMap::new()))
        .as_table_mut()
        .ok_or_else(|| anyhow!("[platforms] is not a table"))?;
    let entry = platforms
        .entry(provider.name.as_str())
        .or_insert_with(|| TomlValue::Table(TomlMap::new()))
        .as_table_mut()
        .ok_or_else(|| anyhow!("[platforms.{}] is not a table", provider.name))?;
    entry.insert("api".into(), provider.api.as_str().into());
    entry.insert("base_url".into(), provider.base_url.as_str().into());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kigi_models::custom::CustomApi;

    fn provider(name: &str, api: CustomApi, url: &str) -> CustomProvider {
        CustomProvider::new(name, api, url).unwrap()
    }

    #[test]
    fn save_writes_definition_and_key_and_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(
            &config,
            "[ui]\ntheme = \"kiginight\"\n\n[platforms.moonshot-cn]\napi_key = \"sk-cn\"\n\n[platforms.proxy]\napi_key = \"hand-written\"\n",
        )
        .unwrap();
        let p = provider("proxy", CustomApi::OpenAi, "https://h.example/v1/");

        save_custom_provider_in(&config, dir.path(), &p, "  sk-new  ").unwrap();

        let saved: TomlValue = toml::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(saved["ui"]["theme"].as_str(), Some("kiginight"));
        assert_eq!(
            saved["platforms"]["moonshot-cn"]["api_key"].as_str(),
            Some("sk-cn")
        );
        let entry = &saved["platforms"]["proxy"];
        assert_eq!(entry["api"].as_str(), Some("openai"));
        assert_eq!(entry["base_url"].as_str(), Some("https://h.example/v1"));
        assert_eq!(entry["api_key"].as_str(), Some("hand-written"));
        let stored = crate::auth::read_auth_json(&dir.path().join("auth.json")).unwrap();
        assert_eq!(stored["proxy"].key, "sk-new");
    }

    #[test]
    fn save_creates_config_and_a_second_save_rewrites_the_definition() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        save_custom_provider_in(
            &config,
            dir.path(),
            &provider("gw", CustomApi::OpenAi, "https://a.example/v1"),
            "k1",
        )
        .unwrap();
        save_custom_provider_in(
            &config,
            dir.path(),
            &provider("gw", CustomApi::Anthropic, "https://b.example/v1"),
            "k2",
        )
        .unwrap();

        let saved: TomlValue = toml::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(saved["platforms"]["gw"]["api"].as_str(), Some("anthropic"));
        assert_eq!(
            saved["platforms"]["gw"]["base_url"].as_str(),
            Some("https://b.example/v1")
        );
        let stored = crate::auth::read_auth_json(&dir.path().join("auth.json")).unwrap();
        assert_eq!(stored["gw"].key, "k2");
    }

    #[test]
    fn empty_key_and_broken_config_fail_before_any_write() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let p = provider("proxy", CustomApi::OpenAi, "https://h.example/v1");

        assert!(save_custom_provider_in(&config, dir.path(), &p, "  ").is_err());
        assert!(!config.exists());
        assert!(!dir.path().join("auth.json").exists());

        std::fs::write(&config, "[platforms\nnope").unwrap();
        let err = save_custom_provider_in(&config, dir.path(), &p, "sk").unwrap_err();
        assert!(err.to_string().contains("unparseable"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "[platforms\nnope"
        );
        assert!(!dir.path().join("auth.json").exists());
    }

    #[test]
    fn a_non_table_platforms_value_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "platforms = \"x\"\n").unwrap();
        let p = provider("proxy", CustomApi::OpenAi, "https://h.example/v1");
        let err = save_custom_provider_in(&config, dir.path(), &p, "sk").unwrap_err();
        assert!(err.to_string().contains("not a table"), "{err}");
        assert!(!dir.path().join("auth.json").exists());
    }
}
