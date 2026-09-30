//! Custom provider login steps: base URL, name, key.

use kigi_shell::models::custom::{
    CustomApi, CustomProvider, default_name, normalize_base_url, validate_model_id, validate_name,
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

/// Fetch status of the model-selection screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectFetch {
    InFlight,
    Listed,
    Failed(String),
}

/// One selectable model row; `manual` marks user-typed ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectRow {
    pub id: String,
    pub selected: bool,
    pub manual: bool,
}

/// Model selection after the key step: fetched rows plus manual adds.
#[derive(Clone)]
pub struct CustomSelectState {
    /// The fetch may adopt a `/v1` base URL.
    pub provider: CustomProvider,
    key: String,
    pub rows: Vec<SelectRow>,
    pub highlight: usize,
    pub fetch: SelectFetch,
    pub error: Option<String>,
    /// Enter hit while the listing is in flight; it completes on arrival.
    pub finish_requested: bool,
}

impl CustomSelectState {
    pub fn new(provider: CustomProvider, key: String) -> Self {
        Self {
            provider,
            key,
            rows: Vec::new(),
            highlight: 0,
            fetch: SelectFetch::InFlight,
            error: None,
            finish_requested: false,
        }
    }

    pub fn take_key(self) -> String {
        self.key
    }

    pub fn apply_fetch(&mut self, fetch: kigi_shell::agent::custom_providers::LoginFetch) {
        self.provider = fetch.provider;
        for id in fetch.model_ids {
            if self.rows.iter().any(|r| r.id == id) {
                continue;
            }
            self.rows.push(SelectRow {
                id,
                selected: false,
                manual: false,
            });
        }
        self.fetch = SelectFetch::Listed;
    }

    pub fn fail_fetch(&mut self, reason: String) {
        self.fetch = SelectFetch::Failed(reason);
    }

    pub fn move_highlight(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let max = self.rows.len() - 1;
        self.highlight = self.highlight.saturating_add_signed(delta).min(max);
    }

    pub fn toggle_highlighted(&mut self) {
        if let Some(row) = self.rows.get_mut(self.highlight) {
            row.selected = !row.selected;
        }
    }

    /// Adds a typed id selected; an id already listed just gets selected.
    pub fn add_manual(&mut self, raw: &str) {
        let id = raw.trim();
        if let Err(e) = validate_model_id(id) {
            self.error = Some(e.to_string());
            return;
        }
        if let Some(row) = self.rows.iter_mut().find(|r| r.id == id) {
            row.selected = true;
        } else {
            self.rows.push(SelectRow {
                id: id.to_owned(),
                selected: true,
                manual: true,
            });
        }
        self.error = None;
    }

    /// The chosen ids, or a refusal when nothing is chosen.
    pub fn try_finish(&mut self) -> Option<Vec<String>> {
        // Wait for the listing: it may adopt a probed `/v1` base URL.
        if self.fetch == SelectFetch::InFlight {
            self.finish_requested = true;
            return None;
        }
        let ids: Vec<String> = self
            .rows
            .iter()
            .filter(|r| r.selected)
            .map(|r| r.id.clone())
            .collect();
        if ids.is_empty() {
            self.finish_requested = false;
            self.error = Some("Select at least one model, or type one and press enter".to_owned());
            return None;
        }
        Some(ids)
    }
}

impl std::fmt::Debug for CustomSelectState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CustomSelectState")
            .field("provider", &self.provider)
            .field("key", &"<set>")
            .field("rows", &self.rows)
            .field("highlight", &self.highlight)
            .field("fetch", &self.fetch)
            .field("error", &self.error)
            .field("finish_requested", &self.finish_requested)
            .finish()
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

    fn select_state() -> CustomSelectState {
        let provider =
            CustomProvider::new("gw", CustomApi::OpenAi, "https://h.example/v1").unwrap();
        CustomSelectState::new(provider, "sk-secret".to_owned())
    }

    #[test]
    fn select_state_toggles_adds_and_finishes() {
        let mut state = select_state();
        assert!(state.try_finish().is_none(), "in flight: finish waits");
        assert!(state.finish_requested && state.error.is_none());
        state.finish_requested = false;

        state.apply_fetch(kigi_shell::agent::custom_providers::LoginFetch {
            provider: state.provider.clone(),
            model_ids: vec!["m1".to_owned(), "m2".to_owned()],
        });
        assert!(state.try_finish().is_none());
        assert!(state.error.is_some(), "listed: an empty selection refuses");
        assert_eq!(state.rows.len(), 2);

        state.toggle_highlighted();
        state.move_highlight(1);
        state.toggle_highlighted();
        state.move_highlight(9);
        assert_eq!(state.highlight, 1, "highlight clamps at the last row");

        state.add_manual(" claude-opus-5-5[1m] ");
        assert_eq!(state.rows.len(), 3);
        state.add_manual("m1");
        assert_eq!(
            state.rows.len(),
            3,
            "a listed id is selected, not duplicated"
        );
        state.add_manual("has space");
        assert!(state.error.is_some());
        assert_eq!(state.rows.len(), 3);

        let ids = state.try_finish().expect("three selected");
        assert_eq!(ids, ["m1", "m2", "claude-opus-5-5[1m]"]);
    }

    #[test]
    fn select_state_apply_fetch_adopts_the_probed_base_and_dedups() {
        let mut state = select_state();
        state.add_manual("m1");
        let adopted =
            CustomProvider::new("gw", CustomApi::OpenAi, "https://h.example/coding/v1").unwrap();
        state.apply_fetch(kigi_shell::agent::custom_providers::LoginFetch {
            provider: adopted,
            model_ids: vec!["m1".to_owned(), "m2".to_owned()],
        });
        assert_eq!(state.provider.base_url, "https://h.example/coding/v1");
        assert_eq!(state.rows.len(), 2, "the manual row covers the fetched id");
        assert!(state.rows[0].manual && state.rows[0].selected);
        assert!(!state.rows[1].selected);
    }

    #[test]
    fn select_state_debug_hides_the_key() {
        let state = select_state();
        assert!(!format!("{state:?}").contains("sk-secret"));
    }
}
