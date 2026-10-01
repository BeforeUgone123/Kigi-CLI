//! Named external-agent providers (`--provider <NAME>` / config `provider`).
//!
//! A provider names the agent process the TUI connects to. Built-in presets
//! cover the common cases; `[agent_providers.<name>] command = "..."` tables
//! in config.toml extend the registry without a rebuild.

/// Preset registry: provider name → agent command.
const PROVIDERS: &[(&str, &str)] = &[
    // The kigi agent itself (same as no --provider; explicit for symmetry
    // with config files that switch providers).
    ("kigi", "kigi acp"),
    // Local Devin agent, driven over ACP stdio (`devin acp`).
    ("local-devin", "devin acp"),
    ("devin", "devin acp"),
];

/// Look up the agent command for a provider name. Built-in presets first,
/// then `[agent_providers.<name>]` entries from config.toml.
pub fn resolve_provider(name: &str, config: &toml::Value) -> Option<String> {
    if let Some(cmd) = PROVIDERS.iter().find(|(n, _)| *n == name) {
        return Some(cmd.1.to_string());
    }
    config
        .get("agent_providers")
        .and_then(|t| t.get(name))
        .and_then(|t| t.get("command"))
        .and_then(|c| c.as_str())
        .map(str::to_string)
}

/// The configured default provider: config.toml `provider = "<name>"`.
/// Returns `Some` only when the value is a non-empty string.
pub fn config_default_provider(config: &toml::Value) -> Option<String> {
    config
        .get("provider")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

/// `[agent_providers.<name>]` tables whose `command` is missing or not a
/// string — surfaced as warnings so a typo isn't silently ignored.
pub fn warn_invalid_agent_providers(config: &toml::Value) {
    let Some(table) = config.get("agent_providers").and_then(|t| t.as_table()) else {
        return;
    };
    for (name, entry) in table {
        let ok = entry.get("command").and_then(|c| c.as_str()).is_some();
        if !ok {
            tracing::warn!("[agent_providers.{name}] has no `command`; it is ignored");
        }
    }
}
