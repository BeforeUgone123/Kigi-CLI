// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

const CUSTOM_KEY: &str = "sk-custom-provider-key";
const HOUSE_KEY: &str = "test-key-for-ci";

/// Declares `[platforms.proxy]` in the pager's isolated home.
fn declare_provider(content: &ContentController, api: &str) {
    let kigi_home = content.home().join(".kigi");
    std::fs::create_dir_all(&kigi_home).expect("create .kigi");
    std::fs::write(
        kigi_home.join("config.toml"),
        format!(
            "[platforms.proxy]\nbase_url = \"{}\"\napi = \"{api}\"\napi_key = \"{CUSTOM_KEY}\"\n",
            content.url()
        ),
    )
    .expect("write config.toml");
}

fn turn_requests(
    content: &ContentController,
    path: &str,
) -> Vec<kigi_test_support::mock_server::LogEntry> {
    content
        .requests()
        .into_iter()
        .filter(|e| {
            e.path == path
                && e.body
                    .as_ref()
                    .and_then(|b| b.get("model"))
                    .and_then(|m| m.as_str())
                    == Some("custom-m1")
        })
        .collect()
}

async fn run_turn(content: &ContentController) -> PtyHarness {
    content.set_response(format!(
        "{MOCK_RESPONSE_SENTINEL} from the custom provider."
    ));
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, content, &[])
            .expect("spawn pager with content");
    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("mock response on screen");
    harness
}

/// A declared OpenAI-compatible provider supplies the catalog and the turn:
/// the request goes to `/chat/completions` for the fetched model with the
/// provider's own key. The house key, which is set in this environment on the
/// same loopback URL as the session's coding endpoint, never rides it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn custom_openai_provider_lists_models_and_uses_its_own_key() {
    let content = ContentController::start_with_models(vec![MockModel::new("custom-m1")])
        .await
        .expect("start content");
    declare_provider(&content, "openai");

    let mut harness = run_turn(&content).await;

    let turns = turn_requests(&content, "/v1/chat/completions");
    assert!(
        !turns.is_empty(),
        "no chat completion for custom-m1: {:?}",
        content.requests()
    );
    for turn in &turns {
        assert_eq!(
            turn.authorization.as_deref(),
            Some(format!("Bearer {CUSTOM_KEY}").as_str()),
            "the custom provider's own key must ride, never {HOUSE_KEY}"
        );
    }

    harness
        .inject_keys(b"/model ")
        .expect("open the /model list");
    harness
        .wait_for_text("proxy", Duration::from_secs(10))
        .expect("the fetched model is listed with its provider name");
    harness.quit().expect("clean quit");
}

/// An Anthropic-compatible provider is reached on `/messages` with
/// `x-api-key` and `anthropic-version`, and no Bearer header.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn custom_anthropic_provider_uses_messages_and_x_api_key() {
    let content = ContentController::start_with_models(vec![MockModel::new("custom-m1")])
        .await
        .expect("start content");
    declare_provider(&content, "anthropic");

    let mut harness = run_turn(&content).await;

    let turns = turn_requests(&content, "/v1/messages");
    assert!(
        !turns.is_empty(),
        "no messages request for custom-m1: {:?}",
        content.requests()
    );
    for turn in &turns {
        assert_eq!(
            turn.header("x-api-key"),
            Some(CUSTOM_KEY),
            "request headers: {:?}",
            turn.headers
        );
        assert!(turn.header("anthropic-version").is_some());
        assert_eq!(
            turn.authorization, None,
            "an x-api-key wire carries no Bearer"
        );
    }
    harness.quit().expect("clean quit");
}

/// A saved provider is a complete login: with no house key and no session, the
/// pager starts authenticated and the turn reaches the provider.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn custom_provider_alone_authenticates_at_startup() {
    let content = ContentController::start_with_models(vec![MockModel::new("custom-m1")])
        .await
        .expect("start content");
    declare_provider(&content, "openai");
    content.set_response(format!(
        "{MOCK_RESPONSE_SENTINEL} from the custom provider."
    ));
    let env: Vec<(String, String)> = content
        .env_for_pager()
        .into_iter()
        .filter(|(name, _)| name != "KIGI_API_KEY")
        .collect();
    let env_refs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::new_in_dir(&binary, DEFAULT_ROWS, DEFAULT_COLS, &[], &env_refs, None)
            .expect("spawn pager without a house key");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("the turn reaches the custom provider without a login screen");
    harness.quit().expect("clean quit");
}
