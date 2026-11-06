//! A translated chat as its views draw it, on GPUI's test context over the fake engine (`common`) and a fake `claude`
//! (synthetic messages only): the header's language picker sets the chat's language; the chat's messages drawn
//! translated with Teams' translation mark, a click on it showing the message as written; a message sent from the box
//! translated before it goes; and a tool that fails saying why in the chat and in the box, which sends nothing until
//! Send without translating.

#![cfg(test)]

#[expect(dead_code, reason = "the shared stand-ins hold more than this test uses")]
mod common;
#[path = "common/parts.rs"]
mod parts;

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{LAB_A, LAB_B, Rig};
use gpui::{
    AppContext as _, Context, Entity, IntoElement, Modifiers, ParentElement as _, Render, Styled as _, TestAppContext,
    VisualTestContext, Window, div, px, size,
};
use teams_core::Action;
use teams_store::Message;
use teams_ui::ViewTrace;
use teams_ui::chat_translate::{ChatTranslation, header_button};
use teams_ui::chat_view::{ChatView, Item};
use teams_ui::local_ai::{Backend, Tools};
use teams_ui::settings::{ChatLanguage, Settings, SettingsModel};

const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";

/// A fake Claude Code: each message of a batch as `DE <text>`, an outgoing message as `EN <text>`.
const FAKE_CLAUDE: &str = r#"#!/bin/bash
while IFS= read -r line; do
  ans=""
  case "$line" in
    *'<translate>'*)
      while IFS= read -r m; do
        n=$(printf '%s' "$m" | sed 's/^<m id=\\"\([0-9]*\)\\".*/\1/')
        t=$(printf '%s' "$m" | sed 's/^.*\\">\(.*\)<\/m>$/\1/')
        ans="$ans<m id=\\\"$n\\\" lang=\\\"en\\\">DE $t</m>"
      done < <(printf '%s' "$line" | grep -o '<m id=\\"[0-9]*\\" from=\\"[^\\]*\\">[^<]*</m>')
      ;;
    *'<outgoing'*)
      t=$(printf '%s' "$line" | sed 's/.*<outgoing to=\\"[a-z]*\\">\(.*\)<\/outgoing>.*/\1/')
      ans="<out lang=\\\"en\\\">EN $t</out>"
      ;;
  esac
  printf '{"type":"result","subtype":"success","is_error":false,"result":"%s"}\n' "$ans"
done
"#;

/// A fake Claude Code that isn't signed in.
const SIGNED_OUT: &str = "#!/bin/bash\necho 'Invalid API key. Please run /login' >&2\nexit 1\n";

fn tool(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    fs::write(&path, body).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn message(id: &str, at: i64, sender: &str, text: &str) -> Message {
    Message {
        conversation_id: AB.to_owned(),
        id: id.to_owned(),
        version: at,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: Some("Lab Bee".to_owned()),
        message_type: "Text".to_owned(),
        content: text.to_owned(),
        properties: None,
        arrival_time: at,
        compose_time: Some(at),
        edit_time: None,
        delete_time: None,
    }
}

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
    cx.run_until_parked();
}

/// Draws until `done` holds, the fake tool answering on its own thread meanwhile.
fn until(cx: &mut VisualTestContext, done: impl Fn(&mut VisualTestContext) -> bool) {
    for _ in 0..1500 {
        draw(cx);
        if done(cx) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("never happened");
}

/// The a–b chat (two of b's messages, one of a's) translated into German, in a window of its own.
fn translated_chat(rig: &Rig) -> (Entity<ChatView>, &'static mut VisualTestContext) {
    rig.core.messages.lock().unwrap().insert(
        AB.to_owned(),
        vec![
            message("1003", 3_000, LAB_B, "See you"),
            message("1002", 2_000, LAB_A, "Lunch?"),
            message("1001", 1_000, LAB_B, "Hello"),
        ],
    );
    rig.cx.update(teams_ui::window::init);
    let settings = Settings {
        chat_languages: vec![ChatLanguage {
            account: LAB_A.to_owned(),
            chat: AB.to_owned(),
            language: "de",
        }],
        ..Settings::default()
    };
    let parts = parts::parts(rig, settings, Arc::new(ViewTrace::off()));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| ChatView::new(parts, AB.to_owned(), window, cx));
    cx.simulate_resize(size(px(800.0), px(600.0)));
    (view, cx)
}

fn mark_of(view: &Entity<ChatView>, cx: &mut VisualTestContext, id: &str) -> Option<teams_ui::chat_translate::Mark> {
    cx.update(|_, cx| {
        view.read(cx).items().iter().find_map(|item| match item {
            Item::Message(message) if message.id == id => message.translation.clone(),
            _ => None,
        })
    })
}

fn translation(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Entity<ChatTranslation> {
    cx.update(|_, cx| view.read(cx).translation().clone())
}

fn send(view: &Entity<ChatView>, cx: &mut VisualTestContext, text: &str) {
    let compose = cx.update(|_, cx| view.read(cx).compose().clone());
    cx.update(|window, cx| {
        compose.update(cx, |compose, cx| {
            compose.focus(window, cx);
            compose.editor().update(cx, |editor, cx| editor.insert(text, cx));
            compose.send(window, cx);
        });
    });
}

fn sent_texts(rig: &Rig) -> Vec<String> {
    rig.core
        .actions
        .lock()
        .unwrap()
        .iter()
        .filter_map(|action| match action {
            Action::Send { text, .. } => Some(
                text.rich_text()
                    .plain_text()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            _ => None,
        })
        .collect()
}

#[test]
fn a_translated_chat_draws_its_marks_and_sends_in_the_chat_s_language() {
    let rig = Rig::start();
    let dir = tempfile::tempdir().unwrap();
    let fake = tool(dir.path(), "claude", FAKE_CLAUDE);
    rig.cx
        .update(|cx| cx.set_global(Tools::with(vec![(Backend::Claude, fake)])));
    let (view, cx) = translated_chat(&rig);
    until(cx, |cx| {
        matches!(
            mark_of(&view, cx, "1001"),
            Some(teams_ui::chat_translate::Mark::Translated { .. })
        )
    });
    assert!(cx.debug_bounds("translation-mark-1001").is_some());
    assert!(cx.debug_bounds("chat-translate-notice").is_none());
    // the mark shows the message as written, and back
    let mark = cx.debug_bounds("translation-mark-1001").unwrap();
    cx.simulate_click(mark.center(), Modifiers::none());
    draw(cx);
    assert!(matches!(
        mark_of(&view, cx, "1001"),
        Some(teams_ui::chat_translate::Mark::Original { .. })
    ));

    // written in German, sent in the chat's language (English), after the box said it was translating
    send(&view, cx, "Hallo");
    until(cx, |_| !sent_texts(&rig).is_empty());
    assert_eq!(sent_texts(&rig), ["EN Hallo"]);
    let compose = cx.update(|_, cx| view.read(cx).compose().clone());
    cx.update(|_, cx| assert!(!compose.read(cx).translating() && compose.read(cx).untranslated().is_none()));
}

#[test]
fn a_tool_that_fails_says_so_and_a_send_goes_only_as_written_when_asked() {
    let rig = Rig::start();
    let dir = tempfile::tempdir().unwrap();
    let fake = tool(dir.path(), "claude", SIGNED_OUT);
    rig.cx
        .update(|cx| cx.set_global(Tools::with(vec![(Backend::Claude, fake)])));
    let (view, cx) = translated_chat(&rig);
    until(cx, |cx| cx.debug_bounds("chat-translate-notice").is_some());
    assert!(cx.debug_bounds("chat-translate-retry").is_some());

    send(&view, cx, "Hallo");
    until(cx, |cx| cx.debug_bounds("compose-untranslated").is_some());
    assert!(sent_texts(&rig).is_empty(), "nothing is sent untranslated on its own");
    let compose = cx.update(|_, cx| view.read(cx).compose().clone());
    cx.update(|_, cx| {
        let said = compose.read(cx).untranslated().cloned().unwrap_or_default();
        assert!(said.contains("isn't signed in"), "{said}");
    });
    // Try again fails the same way; Send without translating sends it as written
    let again = cx.debug_bounds("compose-translate-retry").unwrap();
    cx.simulate_click(again.center(), Modifiers::none());
    until(cx, |cx| cx.debug_bounds("compose-untranslated").is_some());
    assert!(sent_texts(&rig).is_empty());
    let as_written = cx.debug_bounds("compose-send-as-written").unwrap();
    cx.simulate_click(as_written.center(), Modifiers::none());
    until(cx, |_| !sent_texts(&rig).is_empty());
    assert_eq!(sent_texts(&rig), ["Hallo"]);
    assert!(cx.debug_bounds("compose-untranslated").is_none());

    // the chat's Try again
    let retry = cx.debug_bounds("chat-translate-retry").unwrap();
    cx.simulate_click(retry.center(), Modifiers::none());
    until(cx, |cx| cx.debug_bounds("chat-translate-notice").is_some());
    // turned off: the notice goes
    let model = translation(&view, cx);
    cx.update(|_, cx| model.update(cx, |model, cx| model.set_language(None, cx)));
    draw(cx);
    assert!(cx.debug_bounds("chat-translate-notice").is_none());
    assert_eq!(mark_of(&view, cx, "1001"), None);
}

/// The header's language picker on its own.
struct Header(Entity<ChatTranslation>);

impl Render for Header {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().p_4().child(header_button(&self.0, cx))
    }
}

#[test]
fn the_header_s_picker_sets_the_chat_s_language() {
    let rig = Rig::start();
    rig.cx.update(teams_ui::window::init);
    rig.cx.update(|cx| cx.set_global(Tools::with(Vec::new())));
    let settings = rig
        .cx
        .update(|cx| cx.new(|_| SettingsModel::in_memory(Settings::default())));
    let core: Arc<dyn teams_ui::Core> = rig.core.clone();
    let shared = settings.clone();
    let model = rig
        .cx
        .update(|cx| cx.new(|cx| ChatTranslation::new(core, shared, AB, cx)));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let picker = model.clone();
    let (_, cx) = app.add_window_view(|_, _| Header(picker));
    cx.simulate_resize(size(px(800.0), px(600.0)));
    draw(cx);
    let button = cx.debug_bounds("chat-translate").unwrap();
    cx.simulate_click(button.center(), Modifiers::none());
    draw(cx);
    assert!(
        cx.debug_bounds("chat-translate-off").is_some(),
        "the menu lists Show as written"
    );
    let catalan = cx.debug_bounds("chat-translate-ca").unwrap();
    cx.simulate_click(catalan.center(), Modifiers::none());
    draw(cx);
    cx.update(|_, cx| {
        assert_eq!(model.read(cx).language(), Some("ca"));
        assert_eq!(settings.read(cx).settings().chat_language_of(LAB_A, AB), Some("ca"));
    });
}
