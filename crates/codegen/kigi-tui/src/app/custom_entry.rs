//! Custom provider login steps: base URL, name, key.

use kigi_shell::models::custom::{
    CustomApi, CustomProvider, default_name, normalize_base_url, validate_name,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomEntryField {
    BaseUrl,
    Name,
    Key,
}

/// Which field the input box holds, for which wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CustomEntryStep {
    pub api: CustomApi,
    pub field: CustomEntryField,
}

impl CustomEntryStep {
    pub fn first(api: CustomApi) -> Self {
        Self {
            api,
            field: CustomEntryField::BaseUrl,
        }
    }

    /// Only the key is masked.
    pub fn masks_input(self) -> bool {
        self.field == CustomEntryField::Key
    }

    pub fn placeholder(self) -> &'static str {
        match self.field {
            CustomEntryField::BaseUrl => "https://host/v1",
            CustomEntryField::Name => "provider name",
            CustomEntryField::Key => "Paste your API key here...",
        }
    }
}

/// What one Enter press did.
#[derive(Debug, PartialEq, Eq)]
pub enum CustomEntryOutcome {
    /// The input stays; the draft error says why.
    Stay,
    /// Next field, with this text in the box.
    Next {
        field: CustomEntryField,
        prefill: String,
    },
    /// Every field is valid.
    Done {
        provider: CustomProvider,
        key: String,
    },
}

/// Values collected so far, and the last refusal.
#[derive(Debug, Default, Clone)]
pub struct CustomEntryDraft {
    base_url: String,
    name: String,
    pub error: Option<String>,
}

impl CustomEntryDraft {
    pub const fn new() -> Self {
        Self {
            base_url: String::new(),
            name: String::new(),
            error: None,
        }
    }

    pub fn clear(&mut self) {
        *self = Self::new();
    }

    pub fn instruction(&self, step: CustomEntryStep) -> String {
        match step.field {
            CustomEntryField::BaseUrl => format!(
                "Base URL of the {} API, for example https://host/v1",
                step.api.label()
            ),
            CustomEntryField::Name => {
                "Name for this provider. Its models appear as name/model".to_owned()
            }
            CustomEntryField::Key => format!(
                "API key for {}. A local server that ignores auth takes any value",
                self.name
            ),
        }
    }

    pub fn submit(&mut self, step: CustomEntryStep, input: &str) -> CustomEntryOutcome {
        let input = input.trim();
        if input.is_empty() {
            self.error = None;
            return CustomEntryOutcome::Stay;
        }
        self.error = None;
        match step.field {
            CustomEntryField::BaseUrl => match normalize_base_url(input) {
                Ok(url) => {
                    let prefill = default_name(&url);
                    self.base_url = url;
                    CustomEntryOutcome::Next {
                        field: CustomEntryField::Name,
                        prefill,
                    }
                }
                Err(e) => self.refuse(e),
            },
            CustomEntryField::Name => match validate_name(input) {
                Ok(()) => {
                    self.name = input.to_owned();
                    CustomEntryOutcome::Next {
                        field: CustomEntryField::Key,
                        prefill: String::new(),
                    }
                }
                Err(e) => self.refuse(e),
            },
            CustomEntryField::Key => {
                match CustomProvider::new(&self.name, step.api, &self.base_url) {
                    Ok(provider) => CustomEntryOutcome::Done {
                        provider,
                        key: input.to_owned(),
                    },
                    Err(e) => self.refuse(e),
                }
            }
        }
    }

    fn refuse(&mut self, reason: impl std::fmt::Display) -> CustomEntryOutcome {
        self.error = Some(reason.to_string());
        CustomEntryOutcome::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(field: CustomEntryField) -> CustomEntryStep {
        CustomEntryStep {
            api: CustomApi::Anthropic,
            field,
        }
    }

    #[test]
    fn three_valid_steps_build_the_provider() {
        let mut draft = CustomEntryDraft::default();
        let next = draft.submit(
            step(CustomEntryField::BaseUrl),
            " https://api.gw.example/v1/ ",
        );
        assert_eq!(
            next,
            CustomEntryOutcome::Next {
                field: CustomEntryField::Name,
                prefill: "gw-example".to_owned()
            }
        );
        let next = draft.submit(step(CustomEntryField::Name), "gw");
        assert_eq!(
            next,
            CustomEntryOutcome::Next {
                field: CustomEntryField::Key,
                prefill: String::new()
            }
        );
        assert!(
            draft
                .instruction(step(CustomEntryField::Key))
                .contains("API key for gw")
        );

        let done = draft.submit(step(CustomEntryField::Key), "  sk-1  ");
        let CustomEntryOutcome::Done { provider, key } = done else {
            panic!("expected Done, got {done:?}");
        };
        assert_eq!(provider.name, "gw");
        assert_eq!(provider.api, CustomApi::Anthropic);
        assert_eq!(provider.base_url, "https://api.gw.example/v1");
        assert_eq!(key, "sk-1");
        assert!(draft.error.is_none());
    }

    #[test]
    fn bad_input_stays_on_the_field_with_a_reason() {
        let mut draft = CustomEntryDraft::default();
        assert_eq!(
            draft.submit(step(CustomEntryField::BaseUrl), "host.example"),
            CustomEntryOutcome::Stay
        );
        assert!(draft.error.as_deref().is_some_and(|e| e.contains("http")));

        draft.submit(step(CustomEntryField::BaseUrl), "https://h.example/v1");
        assert_eq!(
            draft.submit(step(CustomEntryField::Name), "OpenAI"),
            CustomEntryOutcome::Stay
        );
        assert!(draft.error.is_some());
        assert_eq!(
            draft.submit(step(CustomEntryField::Name), "openai"),
            CustomEntryOutcome::Stay
        );
        assert!(
            draft
                .error
                .as_deref()
                .is_some_and(|e| e.contains("built-in"))
        );
    }

    #[test]
    fn empty_input_stays_and_clears_the_last_error() {
        let mut draft = CustomEntryDraft::default();
        draft.submit(step(CustomEntryField::BaseUrl), "nope");
        assert!(draft.error.is_some());
        assert_eq!(
            draft.submit(step(CustomEntryField::BaseUrl), "   "),
            CustomEntryOutcome::Stay
        );
        assert!(draft.error.is_none());
    }

    #[test]
    fn only_the_key_is_masked() {
        assert!(step(CustomEntryField::Key).masks_input());
        assert!(!step(CustomEntryField::BaseUrl).masks_input());
        assert!(!step(CustomEntryField::Name).masks_input());
    }
}
