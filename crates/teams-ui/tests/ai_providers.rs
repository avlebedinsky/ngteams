//! The settings' "AI providers" box (`teams_ui::settings_view::ai_providers`) on GPUI's test context: a command-line
//! tool found shows without a settings entry and a switch writes the whole list; an OpenAI-compatible provider added,
//! tested against a mock server on the loopback, saved with its key, edited, moved, turned off and removed; the key
//! only in the key vault, never in the settings file or a `Debug` output; a key that the vault refuses; a saved key
//! removed; the AI settings' dropdowns then offer the new provider. Every key is synthetic; nothing leaves the
//! loopback.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
use teams_auth::secrets::Secret;
use teams_ui::local_ai::providers::{MAX_NAME, MemoryKeys};
use teams_ui::local_ai::{AiKeys, Backend, KeyVault, Provider, ToolChoice, Tools};
use teams_ui::settings::{Settings, SettingsModel, render};
use teams_ui::settings_view::SettingsView;
use teams_ui::settings_view::ai_providers::{AiProviders, edited};
use teams_ui::theme;

/// The synthetic key that the tests type.
const KEY: &str = "sk-synthetic-ui-key-0001";
/// What the mock lists.
const MODELS: &str = r#"{"data":[{"id":"model-a"},{"id":"model-b"},{"id":"model-c"}]}"#;

/// A mock HTTP server on the loopback that gives each request the same answer, and keeps each request's head.
struct Mock {
    base: String,
    heads: Arc<Mutex<Vec<String>>>,
}

impl Mock {
    fn new(status: u16, body: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let heads = Arc::new(Mutex::new(Vec::new()));
        let held = Arc::clone(&heads);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                answer(stream, status, body, &held);
            }
        });
        Self { base, heads }
    }

    fn heads(&self) -> Vec<String> {
        self.heads.lock().unwrap().clone()
    }
}

fn answer(mut stream: TcpStream, status: u16, body: &str, heads: &Mutex<Vec<String>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut head = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim_end().is_empty() {
            break;
        }
        head.push_str(&line);
    }
    let length = head
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(str::to_owned)
        })
        .and_then(|length| length.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let _ = (&mut reader).take(length).read_to_end(&mut Vec::new());
    heads.lock().unwrap().push(head);
    let _ = write!(
        stream,
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
}

/// A key vault that keeps nothing: each write fails.
struct RefusingKeys;

impl KeyVault for RefusingKeys {
    fn get(&self, _: &str) -> Result<Option<Secret>, String> {
        Ok(None)
    }

    fn put(&self, _: &str, _: &Secret) -> Result<(), String> {
        Err("the keyring is locked".to_owned())
    }

    fn delete(&self, _: &str) -> Result<(), String> {
        Err("the keyring is locked".to_owned())
    }
}

struct Opened {
    view: Entity<SettingsView>,
    model: Entity<SettingsModel>,
    cx: VisualTestContext,
}

impl Opened {
    fn settle(&mut self) {
        // the mock answers on a thread of its own: give it the time
        for _ in 0..5 {
            self.cx.executor().allow_parking();
            self.cx.run_until_parked();
            thread::sleep(Duration::from_millis(20));
        }
        self.cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    fn shows(&mut self, selector: &str) -> bool {
        self.settle();
        self.cx
            .debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
            .is_some()
    }

    fn click(&mut self, selector: &str) {
        self.settle();
        let bounds = self
            .cx
            .debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
            .unwrap_or_else(|| panic!("no {selector}"));
        self.cx.simulate_click(bounds.center(), Modifiers::none());
        self.settle();
    }

    /// Replaces the text of the editor's field `selector` with `text`, as the user types it.
    fn type_into(&mut self, selector: &str, text: &str) {
        self.click(selector);
        self.cx.simulate_keystrokes("ctrl-a backspace");
        if !text.is_empty() {
            self.cx.simulate_input(text);
        }
        self.settle();
    }

    fn top(&mut self, selector: &str) -> gpui::Pixels {
        self.settle();
        self.cx
            .debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
            .unwrap_or_else(|| panic!("no {selector}"))
            .top()
    }

    fn settings(&mut self) -> Settings {
        let model = self.model.clone();
        self.cx.read(|cx| model.read(cx).settings().clone())
    }

    fn providers(&mut self) -> Entity<AiProviders> {
        let view = self.view.clone();
        self.cx.update(|_, cx| view.update(cx, SettingsView::ai_providers))
    }

    fn tested(&mut self) -> Option<String> {
        let providers = self.providers();
        self.cx
            .read(|cx| providers.read(cx).tested().map(|said| said.to_string()))
    }

    fn problem(&mut self) -> Option<String> {
        let providers = self.providers();
        self.cx
            .read(|cx| providers.read(cx).problem().map(|said| said.to_string()))
    }

    fn editing(&mut self) -> Option<String> {
        let providers = self.providers();
        self.cx.read(|cx| providers.read(cx).editing().map(str::to_owned))
    }
}

/// The settings over `settings` on their General page, the AI providers' keys in `vault`, with `tools` found.
fn opened(settings: Settings, tools: &[Backend], vault: Arc<dyn KeyVault>, cx: &mut TestAppContext) -> Opened {
    cx.update(teams_ui::window::init);
    cx.update(theme::init);
    let found = tools
        .iter()
        .map(|tool| (*tool, format!("/synthetic/{}", tool.key()).into()))
        .collect();
    cx.update(|cx| {
        cx.set_global(Tools::with(found));
        cx.set_global(AiKeys::new(vault));
    });
    let mut made = None;
    let window = cx.add_window(|window, cx| {
        let model = SettingsModel::in_memory(settings);
        model.publish_ai(cx);
        let model = cx.new(|_| model);
        let view = cx.new(|cx| SettingsView::new(model.clone(), cx));
        made = Some((view.clone(), model));
        teams_ui::window::KitRoot::new(view, window, cx)
    });
    let (view, model) = made.unwrap();
    let mut opened = Opened {
        view,
        model,
        cx: VisualTestContext::from_window(window.into(), cx),
    };
    // tall enough for the whole General page
    opened.cx.simulate_resize(size(px(1400.0), px(2400.0)));
    opened.settle();
    opened
}

fn memory() -> Arc<MemoryKeys> {
    Arc::new(MemoryKeys::default())
}

#[gpui::test]
fn a_tool_found_shows_without_a_settings_entry_and_a_switch_writes_the_whole_list(cx: &mut TestAppContext) {
    let mut opened = opened(Settings::default(), &[Backend::Claude, Backend::Codex], memory(), cx);
    assert!(opened.shows("ai-provider-claude"));
    assert!(opened.shows("ai-provider-codex"));
    assert!(opened.settings().ai_providers.is_empty());
    // a command-line tool is only turned off, never removed
    assert!(!opened.shows("ai-provider-claude-remove"));
    // the box comes before the chats' translation
    assert!(opened.top("ai-providers") < opened.top("chat-translator"));

    opened.click("ai-provider-codex-switch");
    let mut codex = Provider::cli(Backend::Codex);
    codex.enabled = false;
    assert_eq!(opened.settings().ai_providers, [Provider::cli(Backend::Claude), codex]);

    // the order holds: Codex first
    opened.click("ai-provider-codex-up");
    assert_eq!(
        opened
            .settings()
            .ai_providers
            .iter()
            .map(|provider| provider.id.as_str())
            .collect::<Vec<_>>(),
        ["codex", "claude"]
    );

    // the tools are both in the list: "Add provider" offers the APIs alone
    opened.click("ai-provider-add");
    for kind in ["anthropic", "openai", "openai-compatible"] {
        assert!(opened.shows(&format!("ai-provider-add-{kind}")), "{kind}");
    }
    assert!(!opened.shows("ai-provider-add-claude"));
    assert!(!opened.shows("ai-provider-add-codex"));
}

#[gpui::test]
fn a_tool_not_found_is_added_from_the_menu(cx: &mut TestAppContext) {
    let mut opened = opened(Settings::default(), &[], memory(), cx);
    assert!(opened.shows("ai-providers"));
    assert!(!opened.shows("ai-provider-claude"));
    opened.click("ai-provider-add");
    opened.click("ai-provider-add-codex");
    assert_eq!(opened.settings().ai_providers, [Provider::cli(Backend::Codex)]);
    assert_eq!(opened.editing(), None, "a tool has nothing to type");
    opened.click("ai-provider-add");
    assert!(!opened.shows("ai-provider-add-codex"));
    assert!(opened.shows("ai-provider-add-claude"));
}

/// Adds an OpenAI-compatible provider at the mock with the key, its test first.
fn add_compatible(opened: &mut Opened, mock: &Mock) {
    opened.click("ai-provider-add");
    opened.click("ai-provider-add-openai-compatible");
    assert_eq!(opened.editing().as_deref(), Some("api-1"));
    assert!(opened.settings().ai_providers.is_empty(), "nothing joins before Save");

    // a server's base URL is a must
    opened.click("ai-provider-save");
    assert!(opened.problem().unwrap().contains("base URL"));
    assert!(opened.settings().ai_providers.is_empty());

    opened.type_into("ai-provider-name", "Local server");
    opened.type_into("ai-provider-base-url", &format!("{}/v1", mock.base));
    opened.type_into("ai-provider-key", KEY);
    opened.click("ai-provider-test");
    assert_eq!(opened.tested().as_deref(), Some("Connected: 3 models"));
    let heads = mock.heads();
    assert_eq!(heads.len(), 1, "{heads:?}");
    assert!(heads[0].starts_with("GET /v1/models "), "{}", heads[0]);
    assert!(
        heads[0]
            .to_ascii_lowercase()
            .contains(&format!("authorization: bearer {KEY}").to_ascii_lowercase()),
        "the key typed goes with the test"
    );
}

/// Edits the saved provider `saved`: a model typed is kept, a key typed nothing keeps the key, a bad model is
/// refused; what the list holds then.
fn edit_saved(opened: &mut Opened, saved: &Provider, keys: &MemoryKeys) -> Provider {
    // edited: the key typed nothing, so the key saved stays
    opened.click("ai-provider-api-1-edit");
    assert_eq!(opened.editing().as_deref(), Some("api-1"));
    assert!(opened.shows("ai-provider-remove-key"));
    opened.type_into("ai-provider-model", "local-model-1");
    opened.click("ai-provider-save");
    let edited = Provider {
        model: Some("local-model-1".to_owned()),
        ..saved.clone()
    };
    assert_eq!(opened.settings().ai_providers[1], edited);
    assert_eq!(keys.get("api-1").unwrap().unwrap().expose(), KEY);

    // a model's name with a space is refused
    opened.click("ai-provider-api-1-edit");
    opened.type_into("ai-provider-model", "two words");
    opened.click("ai-provider-save");
    assert!(opened.problem().unwrap().contains("no spaces"));
    opened.click("ai-provider-cancel");
    assert_eq!(opened.editing(), None);
    assert_eq!(opened.settings().ai_providers[1], edited);

    edited
}

/// Moves the provider up, turns it off, then removes it with its key.
fn move_turn_off_and_remove(opened: &mut Opened, keys: &MemoryKeys) {
    opened.click("ai-provider-api-1-up");
    assert_eq!(opened.settings().ai_providers[0].id, "api-1");
    opened.click("ai-provider-api-1-switch");
    assert!(!opened.settings().ai_providers[0].enabled);

    opened.click("ai-provider-api-1-remove");
    assert_eq!(opened.settings().ai_providers, [Provider::cli(Backend::Claude)]);
    assert!(keys.get("api-1").unwrap().is_none(), "its key went with it");
}

#[gpui::test]
fn an_openai_compatible_provider_is_added_edited_moved_turned_off_and_removed(cx: &mut TestAppContext) {
    let mock = Mock::new(200, MODELS);
    let keys = memory();
    let mut opened = opened(Settings::default(), &[Backend::Claude], keys.clone(), cx);
    add_compatible(&mut opened, &mock);

    opened.click("ai-provider-save");
    assert_eq!(opened.editing(), None);
    assert_eq!(keys.get("api-1").unwrap().unwrap().expose(), KEY);
    let saved = Provider {
        id: "api-1".to_owned(),
        kind: Backend::Compatible,
        name: "Local server".to_owned(),
        enabled: true,
        base_url: Some(format!("{}/v1", mock.base)),
        model: None,
        has_key: true,
    };
    let settings = opened.settings();
    assert_eq!(settings.ai_providers, [Provider::cli(Backend::Claude), saved.clone()]);
    assert!(!render(&settings).contains(KEY), "the settings file keeps no key");
    assert!(!format!("{settings:?}").contains(KEY));
    assert!(opened.shows("ai-provider-api-1"));

    // an AI setting's dropdown offers it
    opened.click("chat-translator");
    opened.click("chat-translator-api-1");
    assert_eq!(
        opened.settings().chat_translator.tool,
        ToolChoice::Only("api-1".to_owned())
    );

    let edited = edit_saved(&mut opened, &saved, &keys);
    move_turn_off_and_remove(&mut opened, &keys);
    assert_ne!(edited, saved);

    // a new provider takes no id that an AI setting still names
    opened.click("ai-provider-add");
    opened.click("ai-provider-add-anthropic");
    assert_eq!(opened.editing().as_deref(), Some("api-2"));
    opened.click("ai-provider-cancel");
    assert_eq!(opened.settings().ai_providers, [Provider::cli(Backend::Claude)]);
}

#[gpui::test]
fn a_key_that_the_keyring_refuses_leaves_the_provider_unsaved_and_says_why(cx: &mut TestAppContext) {
    let mock = Mock::new(200, MODELS);
    let mut opened = opened(Settings::default(), &[], Arc::new(RefusingKeys), cx);
    opened.click("ai-provider-add");
    opened.click("ai-provider-add-anthropic");
    opened.type_into("ai-provider-base-url", &mock.base);
    opened.type_into("ai-provider-key", KEY);
    opened.click("ai-provider-save");
    let problem = opened.problem().unwrap();
    assert!(
        problem.contains("The keyring did not keep the key: the keyring is locked"),
        "{problem}"
    );
    assert!(!problem.contains(KEY));
    assert!(opened.settings().ai_providers.is_empty());
    assert_eq!(opened.editing().as_deref(), Some("api-1"));
}

#[gpui::test]
fn a_saved_key_is_removed_and_a_test_says_what_the_api_refused(cx: &mut TestAppContext) {
    let mock = Mock::new(401, r#"{"error":{"message":"bad key"}}"#);
    let keys = memory();
    keys.put("api-1", &Secret::new(KEY)).unwrap();
    let anthropic = Provider {
        base_url: Some(mock.base.clone()),
        has_key: true,
        ..Provider::new_api(Backend::Anthropic, &[])
    };
    let settings = Settings {
        ai_providers: vec![anthropic.clone()],
        ..Settings::default()
    };
    let mut opened = opened(settings, &[], keys.clone(), cx);
    opened.click("ai-provider-api-1-edit");
    // the key saved goes with the test when none is typed
    opened.click("ai-provider-test");
    let tested = opened.tested().unwrap();
    assert!(tested.contains("refused the API key"), "{tested}");
    assert!(mock.heads()[0].contains(KEY));

    opened.click("ai-provider-remove-key");
    assert!(keys.get("api-1").unwrap().is_none());
    assert!(!opened.settings().ai_providers[0].has_key);
    assert!(!opened.shows("ai-provider-remove-key"));
    // with no key saved or typed, the test asks for one
    opened.click("ai-provider-test");
    assert_eq!(
        opened.tested().as_deref(),
        Some("Type the API key to test the connection.")
    );
}

#[gpui::test]
fn a_tool_s_editor_renames_it_only(cx: &mut TestAppContext) {
    let mut opened = opened(Settings::default(), &[Backend::Claude], memory(), cx);
    opened.click("ai-provider-claude-edit");
    assert!(!opened.shows("ai-provider-base-url"));
    assert!(!opened.shows("ai-provider-key"));
    opened.type_into("ai-provider-name", "");
    opened.click("ai-provider-save");
    assert_eq!(opened.problem().as_deref(), Some("Type a name for the provider."));
    opened.type_into("ai-provider-name", "My Claude");
    opened.click("ai-provider-save");
    assert_eq!(opened.settings().ai_providers[0].name, "My Claude");
}

#[test]
fn the_fields_typed_are_checked() {
    let compatible = Provider::new_api(Backend::Compatible, &[]);
    let openai = Provider::new_api(Backend::OpenAi, &[]);
    assert!(edited(&compatible, &"n".repeat(MAX_NAME + 1), "http://127.0.0.1:1", "").is_err());
    assert!(edited(&compatible, "Server", "", "").is_err());
    assert!(edited(&compatible, "Server", "ftp://127.0.0.1", "").is_err());
    assert!(edited(&compatible, "Server", "http://127.0.0.1:1", "-x").is_err());
    let made = edited(&openai, "Mine", "", "gpt-x").unwrap();
    assert_eq!((made.base_url, made.model), (None, Some("gpt-x".to_owned())));
    // "default" and "recommended" name no model
    assert!(edited(&openai, "Mine", "", "default").is_err());
}
