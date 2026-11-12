//! A chat's translation ([`teams_ui::chat_translate`]) on GPUI's test context, against a fake core keeping what the store
//! would and a fake `claude` that logs every question it is asked (synthetic messages only): off by default; picked,
//! the chat's messages translated a batch at a time with the chat as context and kept; reopened (the app restarted),
//! shown at once with no question asked; an edit or another language translated again; the account's own message
//! sent translated into the chat's language and shown as written, before and after a restart; and every failure saying
//! why, sending nothing.

#![cfg(test)]

#[path = "support/fake_tool.rs"]
mod fake_tool;

#[expect(dead_code, reason = "the shared stand-ins hold more than this test uses")]
mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{FakeCore, LAB_A, LAB_B};
use futures_util::FutureExt as _;
use gpui::{AppContext as _, Entity, TestAppContext};
use teams_proto::chat::RichText;
use teams_store::{Message, SentOriginal, TranslationKind};
use teams_ui::chat_translate::picker::tip_of;
use teams_ui::chat_translate::{ChatTranslation, Limits, Mark, Progress, SendFailure, Status, language_name};
use teams_ui::local_ai::{Backend, Failure, Tools};
use teams_ui::settings::{Settings, SettingsModel};

const CHAT: &str = "19:synthetic-translated@unq.gbl.spaces";

/// A fake Claude Code: logs each question beside itself; translates each message of a batch as `DE <text>` (one that
/// starts with "Guten" is German already) and an outgoing message as `EN <text>`.
const FAKE_CLAUDE: &str = r#"#!/bin/bash
log="$(dirname "$0")/received.log"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$log"
  echo '{"type":"system","subtype":"init"}'
  ans=""
  case "$line" in
    *'<translate>'*)
      while IFS= read -r m; do
        n=$(printf '%s' "$m" | sed 's/^<m id=\\"\([0-9]*\)\\".*/\1/')
        t=$(printf '%s' "$m" | sed 's/^.*\\">\(.*\)<\/m>$/\1/')
        case "$t" in
          Guten*) ans="$ans<m id=\\\"$n\\\" lang=\\\"de\\\" same/>";;
          *) ans="$ans<m id=\\\"$n\\\" lang=\\\"en\\\">DE $t</m>";;
        esac
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

fn fake(dir: &Path, body: &str) -> PathBuf {
    fake_tool::write(dir, "claude", body)
}

/// The questions the fake was asked so far.
fn questions(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join("received.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn message(id: &str, version: i64, sender: &str, text: &str) -> Message {
    Message {
        conversation_id: CHAT.to_owned(),
        id: id.to_owned(),
        version,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: None,
        message_type: "Text".to_owned(),
        content: text.to_owned(),
        properties: None,
        arrival_time: version,
        compose_time: None,
        edit_time: None,
        delete_time: None,
    }
}

struct Rig {
    app: TestAppContext,
    core: Arc<FakeCore>,
    settings: Entity<SettingsModel>,
    dir: tempfile::TempDir,
}

impl Rig {
    fn new(script: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let program = fake(dir.path(), script);
        let mut app = TestAppContext::single();
        app.update(|cx| cx.set_global(Tools::with(vec![(Backend::Claude, program)])));
        let settings = app.new(|_| SettingsModel::in_memory(Settings::default()));
        Self {
            app,
            core: Arc::new(FakeCore::default()),
            settings,
            dir,
        }
    }

    /// The chat's translation, as a newly opened chat view makes it (after a restart: a new one on the same store and
    /// settings).
    fn open(&mut self) -> Entity<ChatTranslation> {
        let core: Arc<dyn teams_ui::Core> = Arc::clone(&self.core) as _;
        let settings = self.settings.clone();
        let quick = Limits {
            turns: 100,
            chars: 1_000_000,
            first: Duration::from_secs(20),
            turn: Duration::from_secs(20),
        };
        self.app
            .new(|cx| ChatTranslation::new(core, settings, CHAT, cx).with_limits(quick))
    }

    fn follow(&mut self, translation: &Entity<ChatTranslation>, messages: &[Message]) {
        let held: Vec<&Message> = messages.iter().collect();
        translation.update(&mut self.app, |translation, cx| {
            translation.follow(&held, &|message| format!("Name of {}", message.id), cx);
        });
    }

    /// What `task` gives, run until it is done (the fake tool answering on its own thread meanwhile).
    fn wait<T: 'static>(&mut self, task: gpui::Task<T>) -> T {
        let mut task = Box::pin(task);
        for _ in 0..1500 {
            self.app.run_until_parked();
            if let Some(done) = (&mut task).now_or_never() {
                return done;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("never done");
    }

    /// Runs until `done` holds, the fake tool answering on its own thread meanwhile.
    fn until(&mut self, translation: &Entity<ChatTranslation>, done: impl Fn(&ChatTranslation) -> bool) {
        for _ in 0..1500 {
            self.app.run_until_parked();
            if translation.read_with(&self.app, |translation, _| done(translation)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("never happened");
    }
}

fn shown_text(translation: &ChatTranslation, message: &Message) -> Option<String> {
    translation.shown().text_of(message).map(|text| text.plain_text())
}

fn history() -> Vec<Message> {
    vec![
        message("1", 1, LAB_B, "Hello"),
        message("2", 2, LAB_B, "Guten Morgen"),
        message("3", 3, LAB_A, "Lunch?"),
        message("4", 4, LAB_B, "Sure & <soon>"),
    ]
}

/// The history read as written, then in German, translated once: the chat's translation and the history.
fn translated_history(rig: &mut Rig) -> (Entity<ChatTranslation>, Vec<Message>) {
    let messages = history();
    let translation = rig.open();
    rig.follow(&translation, &messages);
    rig.app.run_until_parked();
    // off by default: nothing marked, nothing asked
    translation.read_with(&rig.app, |translation, _| {
        assert_eq!(translation.status(), &Status::Off);
        assert_eq!(translation.shown().mark_of(&messages[0]), None);
        assert_eq!(shown_text(translation, &messages[0]), None);
    });
    assert!(questions(rig.dir.path()).is_empty());

    translation.update(&mut rig.app, |translation, cx| translation.set_language(Some("de"), cx));
    assert_eq!(
        rig.settings
            .read_with(&rig.app, |model, _| model.settings().chat_language_of(LAB_A, CHAT)),
        Some("de"),
        "kept in the settings"
    );
    // waiting, then translated: the four messages in one batch, the newest first in priority but in order
    translation.read_with(&rig.app, |translation, _| {
        assert_eq!(translation.status(), &Status::On);
    });
    rig.until(&translation, |translation| {
        shown_text(translation, &messages[3]).is_some()
    });
    translation.read_with(&rig.app, |translation, _| {
        let shown = translation.shown();
        assert_eq!(shown_text(translation, &messages[0]).as_deref(), Some("DE Hello"));
        assert_eq!(
            shown_text(translation, &messages[3]).as_deref(),
            Some("DE Sure & <soon>")
        );
        // already German: shown as written, unmarked
        assert_eq!(shown_text(translation, &messages[1]), None);
        assert_eq!(shown.mark_of(&messages[1]), None);
        assert_eq!(
            shown.mark_of(&messages[0]),
            Some(Mark::Translated {
                tip: "See original message (English)".into()
            })
        );
        assert_eq!(translation.batches(), 1);
        assert_eq!(translation.chat_language().as_deref(), Some("en"));
    });
    let asked = questions(rig.dir.path());
    assert_eq!(asked.len(), 1);
    assert!(
        asked[0].contains("Name of 1") && asked[0].contains("Sure &amp; &lt;soon&gt;"),
        "{}",
        asked[0]
    );
    // kept as the store keeps it, by message, version and language
    let kept = rig.core.translations.lock().unwrap().clone();
    assert_eq!(kept.len(), 4);
    assert!(
        kept.iter()
            .all(|row| row.language == "de" && row.conversation_id == CHAT)
    );
    assert_eq!(
        kept.iter().find(|row| row.message_id == "2").map(|row| row.kind),
        Some(TranslationKind::Same)
    );
    (translation, messages)
}

#[test]
fn a_chat_is_read_as_written_until_a_language_is_picked_and_its_original_offered() {
    let mut rig = Rig::new(FAKE_CLAUDE);
    let (translation, messages) = translated_history(&mut rig);
    // See original message, and back
    translation.update(&mut rig.app, |translation, cx| translation.toggle_original("1", cx));
    translation.read_with(&rig.app, |translation, _| {
        assert_eq!(shown_text(translation, &messages[0]), None);
        assert_eq!(
            translation.shown().mark_of(&messages[0]),
            Some(Mark::Original {
                tip: "See translation (German)".into()
            })
        );
    });
    translation.update(&mut rig.app, |translation, cx| translation.toggle_original("1", cx));
    translation.read_with(&rig.app, |translation, _| {
        assert!(shown_text(translation, &messages[0]).is_some());
    });
}

#[test]
fn a_translated_chat_reopened_asks_nothing_and_translates_only_what_is_new() {
    let mut rig = Rig::new(FAKE_CLAUDE);
    let (translation, messages) = translated_history(&mut rig);
    // the app restarted: the chat opens translated, and nothing is asked of the tool
    drop(translation);
    rig.app.run_until_parked();
    let reopened = rig.open();
    rig.follow(&reopened, &messages);
    rig.until(&reopened, |translation| shown_text(translation, &messages[0]).is_some());
    rig.app.run_until_parked();
    reopened.read_with(&rig.app, |translation, _| {
        assert_eq!(translation.language(), Some("de"));
        assert_eq!(
            shown_text(translation, &messages[3]).as_deref(),
            Some("DE Sure & <soon>")
        );
        assert_eq!(translation.batches(), 0, "no translation asked for stored messages");
    });
    assert_eq!(questions(rig.dir.path()).len(), 1, "the tool was asked nothing more");

    // an edit (a newer version) and a new message are translated, the rest isn't asked again
    let mut later = messages.clone();
    later[0] = message("1", 10, LAB_B, "Hello again");
    later.push(message("5", 11, LAB_B, "Bye"));
    rig.follow(&reopened, &later);
    rig.until(&reopened, |translation| shown_text(translation, &later[4]).is_some());
    reopened.read_with(&rig.app, |translation, _| {
        assert_eq!(shown_text(translation, &later[0]).as_deref(), Some("DE Hello again"));
        assert_eq!(translation.batches(), 1);
    });
    let asked = questions(rig.dir.path());
    assert_eq!(asked.len(), 2);
    assert!(asked[1].contains("Hello again") && asked[1].contains("Bye") && !asked[1].contains(">Lunch?<"));
    assert!(
        asked[1].contains("<context>"),
        "the chat before them as context: {}",
        asked[1]
    );
    let kept = rig.core.translations.lock().unwrap().clone();
    assert_eq!(
        kept.iter().find(|row| row.message_id == "1").map(|row| row.version),
        Some(10),
        "the older version's translation replaced"
    );

    // another language: translated again into it
    reopened.update(&mut rig.app, |translation, cx| translation.set_language(Some("fr"), cx));
    rig.until(&reopened, |translation| shown_text(translation, &later[4]).is_some());
    assert_eq!(questions(rig.dir.path()).len(), 3);
    // turned off: shown as written
    reopened.update(&mut rig.app, |translation, cx| translation.set_language(None, cx));
    reopened.read_with(&rig.app, |translation, _| {
        assert_eq!(translation.status(), &Status::Off);
        assert_eq!(shown_text(translation, &later[0]), None);
        assert_eq!(translation.shown().mark_of(&later[0]), None);
    });
}

/// The account's message written in German, sent in English and stored: the chat's translation, the chat, what was
/// written and what was sent.
fn sent_in_german(rig: &mut Rig) -> (Entity<ChatTranslation>, Vec<Message>, RichText) {
    let mut messages = vec![message("1", 1, LAB_B, "Hello")];
    let translation = rig.open();
    // nobody else wrote yet: the chat's language isn't known
    translation.update(&mut rig.app, |translation, cx| translation.set_language(Some("de"), cx));
    let written = RichText::plain("Hallo zusammen");
    let unknown = translation
        .update(&mut rig.app, |translation, cx| translation.outgoing(&written, cx))
        .unwrap();
    assert_eq!(rig.wait(unknown), Err(SendFailure::Unknown));

    rig.follow(&translation, &messages);
    rig.until(&translation, |translation| translation.chat_language().is_some());
    let task = translation
        .update(&mut rig.app, |translation, cx| translation.outgoing(&written, cx))
        .unwrap();
    let sent = rig.wait(task).unwrap().unwrap();
    assert_eq!(sent.plain_text(), "EN Hallo zusammen");
    let asked = questions(rig.dir.path());
    let last = asked.last().unwrap();
    assert!(
        last.contains("<outgoing to=\\\"en\\\">Hallo zusammen</outgoing>"),
        "{last}"
    );
    assert!(last.contains("Name of 1: Hello"), "the chat as context: {last}");

    // sent: shown as written while it goes, and once it is stored
    translation.update(&mut rig.app, |translation, cx| {
        translation.sent(&written, &sent, None, cx);
    });
    translation.update(&mut rig.app, |translation, _| {
        translation.sent_as("synthetic-client-1".to_owned(), &sent);
    });
    translation.read_with(&rig.app, |translation, _| {
        assert_eq!(
            translation
                .shown()
                .original_of_sent(&sent)
                .map(|text| text.plain_text()),
            Some("Hallo zusammen".to_owned())
        );
    });
    let kept = rig.core.sent_originals.lock().unwrap().clone();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].key, "synthetic-client-1");
    assert_eq!(kept[0].sent_text, "EN Hallo zusammen");
    let mut own = message("2", 2, LAB_A, "EN Hallo zusammen");
    own.client_message_id = Some("synthetic-client-1".to_owned());
    messages.push(own);
    rig.follow(&translation, &messages);
    rig.until(&translation, |translation| {
        shown_text(translation, &messages[1]).is_some()
    });
    translation.read_with(&rig.app, |translation, _| {
        assert_eq!(shown_text(translation, &messages[1]).as_deref(), Some("Hallo zusammen"));
        assert_eq!(
            translation.shown().mark_of(&messages[1]),
            Some(Mark::Translated {
                tip: "See the message as sent".into()
            })
        );
    });
    (translation, messages, written)
}

#[test]
fn the_account_s_message_goes_out_in_the_chat_s_language_and_shows_as_written() {
    let mut rig = Rig::new(FAKE_CLAUDE);
    let (translation, messages, _) = sent_in_german(&mut rig);
    translation.read_with(&rig.app, |translation, _| {
        assert_eq!(translation.chat_language().as_deref(), Some("en"));
    });
    assert_eq!(messages.len(), 2);
}

#[test]
fn the_account_s_messages_show_as_written_after_a_restart_and_a_german_chat_gets_them_as_written() {
    let mut rig = Rig::new(FAKE_CLAUDE);
    let (translation, messages, written) = sent_in_german(&mut rig);
    // after a restart, from the store: the original kept by its row, bound to its message as its translation
    rig.core.translations.lock().unwrap().clear();
    drop(translation);
    let reopened = rig.open();
    rig.follow(&reopened, &messages);
    rig.until(&reopened, |translation| shown_text(translation, &messages[1]).is_some());
    reopened.read_with(&rig.app, |translation, _| {
        assert_eq!(shown_text(translation, &messages[1]).as_deref(), Some("Hallo zusammen"));
    });
    assert!(
        rig.core
            .translations
            .lock()
            .unwrap()
            .iter()
            .any(|row| row.message_id == "2" && row.kind == TranslationKind::Original)
    );

    // an edit's original is kept by its message
    reopened.update(&mut rig.app, |translation, cx| {
        translation.sent(
            &RichText::plain("Neu"),
            &RichText::plain("New"),
            Some(SentOriginal::edit_key("2")),
            cx,
        );
    });
    assert!(
        rig.core
            .sent_originals
            .lock()
            .unwrap()
            .iter()
            .any(|kept| kept.key == "edit:2")
    );

    // a chat whose others write German: a German message goes as written
    let german = vec![message("7", 7, LAB_B, "Guten Tag")];
    rig.follow(&reopened, &german);
    rig.until(&reopened, |translation| translation.chat_language().is_some());
    let same = reopened
        .update(&mut rig.app, |translation, cx| translation.outgoing(&written, cx))
        .unwrap();
    assert_eq!(rig.wait(same), Ok(None));
}

#[test]
fn a_tool_that_fails_stops_the_reading_and_a_send_says_why() {
    let mut rig = Rig::new(SIGNED_OUT);
    let messages = vec![message("1", 1, LAB_B, "Hello")];
    let translation = rig.open();
    translation.update(&mut rig.app, |translation, cx| translation.set_language(Some("de"), cx));
    rig.follow(&translation, &messages);
    rig.until(&translation, |translation| {
        matches!(translation.status(), Status::Failed(_))
    });
    translation.read_with(&rig.app, |translation, _| {
        assert_eq!(
            translation.status(),
            &Status::Failed(Failure::SignedOut(Backend::Claude))
        );
        // shown as written, unmarked
        assert_eq!(shown_text(translation, &messages[0]), None);
        assert_eq!(translation.shown().mark_of(&messages[0]), None);
    });
    let task = translation
        .update(&mut rig.app, |translation, cx| {
            translation.outgoing(&RichText::plain("Hallo"), cx)
        })
        .unwrap();
    let failed = rig.wait(task);
    let Err(SendFailure::Tool(Failure::SignedOut(Backend::Claude))) = &failed else {
        panic!("{failed:?}");
    };
    assert!(failed.unwrap_err().message().contains("isn't signed in"));
    assert!(rig.core.translations.lock().unwrap().is_empty());

    // Try again with no tool at all: it says so
    rig.app.update(|cx| cx.set_global(Tools::with(Vec::new())));
    translation.update(&mut rig.app, |translation, cx| {
        translation.retry(cx);
    });
    rig.app.run_until_parked();
    translation.read_with(&rig.app, |translation, _| {
        assert!(matches!(translation.status(), Status::Failed(_)));
    });
    assert_eq!(language_name("en-US"), Some("English"));
    assert_eq!(language_name("xx"), None);
    assert!(SendFailure::Unknown.message().contains("language isn't known"));
}

/// A fake Claude Code that streams its answer (`--include-partial-messages`): the first message's translation, half of
/// the second's, then it waits for a `go` file beside itself before it writes the rest. It logs its process id.
const STREAMING: &str = r#"#!/bin/bash
dir="$(dirname "$0")"
echo "$$" > "$dir/pid"
delta() { printf '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"%s"}}}\n' "$1"; }
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$dir/received.log"
  case "$line" in
    *'<translate>'*)
      echo '{"type":"stream_event","event":{"type":"message_start"}}'
      delta '<m id=\"1\" lang=\"en\">DE one</m>'
      delta '\n<m id=\"2\" lang=\"en\">DE tw'
      for _ in $(seq 400); do [ -e "$dir/go" ] && break; sleep 0.05; done
      delta 'o</m>\n<m id=\"3\" lang=\"en\">DE three</m>'
      printf '{"type":"result","is_error":false,"result":"%s"}\n' '<m id=\"1\" lang=\"en\">DE one</m>\n<m id=\"2\" lang=\"en\">DE two</m>\n<m id=\"3\" lang=\"en\">DE three</m>'
      ;;
  esac
done
"#;

fn three() -> Vec<Message> {
    vec![
        message("1", 1, LAB_B, "one"),
        message("2", 2, LAB_B, "two"),
        message("3", 3, LAB_B, "three"),
    ]
}

/// The three messages followed and German picked, the streaming fake holding after the first message's answer.
fn streaming_halfway(rig: &mut Rig) -> (Entity<ChatTranslation>, Vec<Message>) {
    let messages = three();
    let translation = rig.open();
    rig.follow(&translation, &messages);
    rig.app.run_until_parked();
    translation.read_with(&rig.app, |translation, _| {
        assert!(!translation.busy(), "off: nothing under way");
        assert_eq!(translation.progress(), None);
        assert_eq!(tip_of(translation), "Translate chat");
    });
    translation.update(&mut rig.app, |translation, cx| translation.set_language(Some("de"), cx));
    // busy at once, before the tool said anything
    translation.read_with(&rig.app, |translation, _| {
        assert!(translation.busy());
        assert_eq!(translation.progress(), Some(Progress { done: 0, total: 3 }));
        assert_eq!(tip_of(translation), "Translating into German… 0 of 3");
    });
    rig.until(&translation, |translation| {
        shown_text(translation, &messages[0]).is_some()
    });
    (translation, messages)
}

#[test]
fn a_chat_being_translated_shows_its_progress_and_each_message_as_it_comes() {
    let mut rig = Rig::new(STREAMING);
    let (translation, messages) = streaming_halfway(&mut rig);
    translation.read_with(&rig.app, |translation, _| {
        // the first message came on its own; the second, half written, still waits, as does the third
        assert_eq!(shown_text(translation, &messages[0]).as_deref(), Some("DE one"));
        let shown = translation.shown();
        assert_eq!(shown.mark_of(&messages[1]), Some(Mark::Waiting));
        assert_eq!(shown.mark_of(&messages[2]), Some(Mark::Waiting));
        assert_eq!(shown_text(translation, &messages[1]), None);
        assert!(translation.busy());
        assert_eq!(translation.progress(), Some(Progress { done: 1, total: 3 }));
        assert_eq!(tip_of(translation), "Translating into German… 1 of 3");
    });
    assert_eq!(rig.core.translations.lock().unwrap().len(), 1, "kept as it came");
    // picking the language again starts nothing new
    translation.update(&mut rig.app, |translation, cx| translation.set_language(Some("de"), cx));
    rig.app.run_until_parked();
    translation.read_with(&rig.app, |translation, _| {
        assert_eq!(translation.batches(), 1);
        assert_eq!(translation.progress(), Some(Progress { done: 1, total: 3 }));
    });

    fs::write(rig.dir.path().join("go"), "").unwrap();
    rig.until(&translation, |translation| !translation.busy());
    translation.read_with(&rig.app, |translation, _| {
        assert_eq!(shown_text(translation, &messages[1]).as_deref(), Some("DE two"));
        assert_eq!(shown_text(translation, &messages[2]).as_deref(), Some("DE three"));
        assert_eq!(translation.progress(), Some(Progress { done: 3, total: 3 }));
        assert_eq!(translation.batches(), 1);
        assert_eq!(tip_of(translation), "Chat translated into German");
    });
    assert_eq!(questions(rig.dir.path()).len(), 1);
    assert_eq!(rig.core.translations.lock().unwrap().len(), 3);
}

#[test]
fn turning_translation_off_mid_run_stops_the_tool_and_drops_what_it_still_writes() {
    let mut rig = Rig::new(STREAMING);
    let (translation, messages) = streaming_halfway(&mut rig);
    let pid = fs::read_to_string(rig.dir.path().join("pid")).unwrap();
    let pid = pid.trim();
    assert!(fake_tool::running(pid));

    translation.update(&mut rig.app, |translation, cx| translation.set_language(None, cx));
    translation.read_with(&rig.app, |translation, _| {
        assert_eq!(translation.status(), &Status::Off);
        assert!(!translation.busy());
        assert_eq!(translation.progress(), None);
        assert_eq!(translation.shown().mark_of(&messages[1]), None);
        assert_eq!(shown_text(translation, &messages[0]), None);
    });
    // the tool is stopped, and what it would still have written goes nowhere
    for _ in 0..500 {
        if !fake_tool::running(pid) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!fake_tool::running(pid), "the tool was stopped");
    fs::write(rig.dir.path().join("go"), "").unwrap();
    for _ in 0..20 {
        rig.app.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
    }
    translation.read_with(&rig.app, |translation, _| {
        assert_eq!(translation.status(), &Status::Off);
        assert_eq!(shown_text(translation, &messages[1]), None);
    });
    assert_eq!(rig.core.translations.lock().unwrap().len(), 1);
}

#[test]
fn a_failure_ends_the_busy_state_and_says_how_to_try_again() {
    let mut rig = Rig::new(SIGNED_OUT);
    let messages = three();
    let translation = rig.open();
    rig.follow(&translation, &messages);
    translation.update(&mut rig.app, |translation, cx| translation.set_language(Some("de"), cx));
    translation.read_with(&rig.app, |translation, _| assert!(translation.busy()));
    rig.until(&translation, |translation| {
        matches!(translation.status(), Status::Failed(_))
    });
    translation.read_with(&rig.app, |translation, _| {
        assert!(!translation.busy());
        assert_eq!(translation.progress(), None);
        assert_eq!(translation.shown().mark_of(&messages[0]), None);
        assert_eq!(tip_of(translation), "Translating into German stopped");
    });
}
