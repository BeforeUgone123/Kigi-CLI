//! Named external-agent providers (`--provider <NAME>`).
//!
//! A provider is a preset external-agent command so users select a backend
//! by name instead of spelling out `--external-agent "devin acp"`.

/// Preset registry: provider name → agent command.
const PROVIDERS: &[(&str, &str)] = &[
    // Local Devin agent, driven over ACP stdio (`devin acp`).
    ("local-devin", "devin acp"),
    ("devin", "devin acp"),
];

/// Look up the agent command for a provider name.
pub fn resolve_provider(name: &str) -> Option<&'static str> {
    PROVIDERS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, cmd)| *cmd)
}

/// Names accepted by `--provider`, for error messages.
pub fn known_providers() -> &'static [&'static str] {
    const NAMES: &[&str] = &["local-devin", "devin"];
    NAMES
}
