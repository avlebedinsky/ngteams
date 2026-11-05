//! A meeting chat's tabs on GPUI's test context, over the fake engine (`common`) and a made-up meeting: the header's
//! Chat, Shared and Recap; "View recap" on a recording's card showing the Recap; the recap's parts, panes and
//! transcript (read when shown, searched, a paragraph clicked playing the recording from there); a recap or transcript
//! that can't be read and its Try again; another chat opening on Chat. Every id, name and word is made up.

#![cfg(test)]

#[expect(dead_code, reason = "the shared fake engine of the view tests: these use part of it")]
mod common;
#[path = "support/recap_fixtures.rs"]
mod fixtures;

use std::sync::Arc;

use common::Rig;
use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
use teams_ui::ViewTrace;
use teams_ui::recap_view::{Pane, RecapView};
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::{ChatTab, Shell, ShellParts};

fn locked<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The rig with the made-up meeting chats; the recorded one's recap and transcripts answered unless `unread`.
fn rig(unread: bool) -> Rig {
    let rig = Rig::start();
    let me = rig.core.me.to_string();
    for id in [fixtures::MEETING, fixtures::TRANSCRIPT_ONLY] {
        locked(&rig.core.conversations).push(fixtures::conversation(id));
    }
    locked(&rig.core.messages).insert(fixtures::MEETING.to_owned(), fixtures::messages(&me));
    locked(&rig.core.names).insert(format!("8:orgid:{}", fixtures::ORGANIZER), "Ada Lab".to_owned());
    if !unread {
        locked(&rig.core.meeting_contents).insert(fixtures::MEETING.to_owned(), fixtures::recorded());
        for item in ["ITEM1", "ITEM2"] {
            locked(&rig.core.transcripts).insert(fixtures::transcript_location(item), fixtures::VTT.to_owned());
        }
    }
    locked(&rig.core.meeting_contents).insert(fixtures::TRANSCRIPT_ONLY.to_owned(), fixtures::transcribed());
    rig
}

fn shell_window(rig: &Rig, chat: &str) -> (Entity<Shell>, &'static mut VisualTestContext) {
    let settings = Settings {
        selected_chat: Some(chat.to_owned()),
        ..Settings::default()
    };
    let parts = ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: rig.cx.update(|cx| cx.new(|_| SettingsModel::in_memory(settings))),
        trace: Arc::new(ViewTrace::off()),
    };
    rig.cx.update(teams_ui::window::init);
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    cx.simulate_resize(size(px(1600.0), px(1000.0)));
    draw(cx);
    (shell, cx)
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

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(at.center(), Modifiers::none());
    draw(cx);
}

fn recap(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Entity<RecapView> {
    cx.update(|_, cx| shell.read(cx).recap_view().cloned())
        .expect("the recap is made")
}

fn asked(rig: &Rig, what: &str) -> usize {
    locked(&rig.core.asked).iter().filter(|asked| **asked == what).count()
}

#[test]
fn view_recap_on_the_recordings_card_shows_the_recap_tab() {
    let rig = rig(false);
    let (shell, cx) = shell_window(&rig, fixtures::MEETING);
    assert!(
        cx.debug_bounds("chat-tab-chat shown").is_some(),
        "a meeting chat opens on Chat"
    );
    assert!(cx.debug_bounds("chat-tab-shared").is_some());
    assert!(cx.debug_bounds("chat-tab-recap").is_some());
    assert_eq!(
        asked(&rig, "meeting_contents"),
        0,
        "nothing read before the Recap shows"
    );
    click(cx, "view-recap-5");
    assert_eq!(cx.update(|_, cx| shell.read(cx).chat_tab()), ChatTab::Recap);
    // the card's recording is the part shown: the second
    let shown = recap(&shell, cx);
    assert_eq!(cx.update(|_, cx| shown.read(cx).selected()), 1);
    click(cx, "recap-part-0");
    assert!(cx.debug_bounds("chat-tab-recap shown").is_some());
    assert!(cx.debug_bounds("recap-view").is_some());
    assert_eq!(asked(&rig, "meeting_contents"), 1);
    // two recordings: Part 1 shown, with Share, Watch in browser and the files the chat shared
    for selector in [
        "recap-part-0",
        "recap-part-1",
        "recap-share",
        "recap-watch",
        "recap-more",
        "recap-file-0",
    ] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector} drawn");
    }
    assert!(
        cx.debug_bounds("recap-summary-upsell").is_some(),
        "the AI summary's upsell first"
    );
    assert_eq!(
        asked(&rig, "meeting_transcript"),
        0,
        "the transcript is read when its pane shows"
    );
    // back to Chat, and the first recording's card shows Part 1
    click(cx, "chat-tab-chat");
    click(cx, "view-recap-2");
    assert_eq!(cx.update(|_, cx| shown.read(cx).selected()), 0);
}

#[test]
fn the_transcript_is_read_searched_and_plays_the_recording_from_a_paragraph() {
    let rig = rig(false);
    let (shell, cx) = shell_window(&rig, fixtures::MEETING);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.show_chat_tab(ChatTab::Recap, window, cx)));
    draw(cx);
    click(cx, "recap-pane-transcript");
    let view = recap(&shell, cx);
    assert_eq!(cx.update(|_, cx| view.read(cx).pane()), Pane::Transcript);
    assert_eq!(asked(&rig, "meeting_transcript"), 1);
    for selector in [
        "recap-ai-notice",
        "recap-started",
        "recap-turn-0",
        "recap-turn-2",
        "recap-download",
    ] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector} drawn");
    }
    // a transcript with a recording says who started it, not who stopped it
    assert!(cx.debug_bounds("recap-stopped").is_none());
    cx.update(|_, cx| view.update(cx, |view, _| view.find("video")));
    draw(cx);
    assert!(cx.debug_bounds("recap-search-count 1/4").is_some());
    click(cx, "recap-search-next");
    assert_eq!(
        cx.update(|_, cx| view.read(cx).search().map(teams_ui::recap::Search::label)),
        Some("2/4".to_owned())
    );
    // a paragraph clicked: the recording's player is made and asked to play from there
    click(cx, "recap-paragraph-1-0");
    assert!(cx.debug_bounds("video-player-recap-0").is_some());
    // Part 2: its own transcript read
    click(cx, "recap-part-1");
    assert_eq!(cx.update(|_, cx| view.read(cx).selected()), 1);
    assert_eq!(asked(&rig, "meeting_transcript"), 2);
    assert_eq!(
        cx.update(|_, cx| view.read(cx).search().cloned()),
        None,
        "a part shown starts unsearched"
    );
}

#[test]
fn a_transcript_alone_shows_no_player_and_who_stopped_it() {
    let rig = rig(false);
    locked(&rig.core.transcripts).insert(fixtures::transcript_location("ITEM9"), fixtures::VTT.to_owned());
    let (shell, cx) = shell_window(&rig, fixtures::TRANSCRIPT_ONLY);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.show_chat_tab(ChatTab::Recap, window, cx)));
    draw(cx);
    assert!(cx.debug_bounds("recap-recording").is_none());
    assert!(cx.debug_bounds("recap-watch").is_none());
    assert!(
        cx.debug_bounds("recap-part-0").is_none(),
        "one part shows no parts' row"
    );
    assert!(cx.debug_bounds("recap-no-files").is_some());
    click(cx, "recap-pane-transcript");
    assert!(cx.debug_bounds("recap-stopped").is_some());
}

#[test]
fn a_recap_that_cant_be_read_says_so_and_reads_again() {
    let rig = rig(true);
    let (shell, cx) = shell_window(&rig, fixtures::MEETING);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.show_chat_tab(ChatTab::Recap, window, cx)));
    draw(cx);
    assert!(cx.debug_bounds("recap-state The recap couldn't be loaded.").is_some());
    assert!(cx.debug_bounds("recap-open-web").is_some());
    locked(&rig.core.meeting_contents).insert(fixtures::MEETING.to_owned(), fixtures::recorded());
    click(cx, "recap-retry");
    assert_eq!(asked(&rig, "meeting_contents"), 2);
    assert!(cx.debug_bounds("recap-part-0").is_some());
    // the transcript can't be read: said, and read again on Try again
    click(cx, "recap-pane-transcript");
    assert!(
        cx.debug_bounds("recap-state The transcript couldn't be loaded.")
            .is_some()
    );
    locked(&rig.core.transcripts).insert(fixtures::transcript_location("ITEM1"), fixtures::VTT.to_owned());
    click(cx, "recap-transcript-retry");
    assert!(cx.debug_bounds("recap-turn-0").is_some());
}

#[test]
fn the_shared_tab_lists_what_the_chat_shared_and_another_chat_opens_on_chat() {
    let rig = rig(false);
    let (shell, cx) = shell_window(&rig, fixtures::MEETING);
    click(cx, "chat-tab-shared");
    assert_eq!(cx.update(|_, cx| shell.read(cx).chat_tab()), ChatTab::Shared);
    // the recordings, a link and the file
    assert!(cx.debug_bounds("shared-row-3").is_some());
    click(cx, "shared-filter-files");
    assert!(cx.debug_bounds("shared-row-1").is_none(), "only the file");
    click(cx, "shared-filter-links");
    assert!(cx.debug_bounds("shared-row-2").is_some());
    assert!(cx.debug_bounds("shared-row-3").is_none());
    // another chat: Chat, and the other chat's tabs let go
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.open_chat(fixtures::TRANSCRIPT_ONLY.to_owned(), window, cx);
        });
    });
    draw(cx);
    assert_eq!(cx.update(|_, cx| shell.read(cx).chat_tab()), ChatTab::Chat);
    assert!(cx.update(|_, cx| shell.read(cx).shared_view().is_none()));
}

fn reopen(shell: &Entity<Shell>, cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.open_chat(fixtures::MEETING.to_owned(), window, cx);
        });
    });
    draw(cx);
}

fn chat_shown(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> bool {
    cx.update(|_, cx| shell.read(cx).chat().map(|chat| chat.read(cx).is_shown()))
        .expect("a chat view")
}

#[test]
fn the_same_chat_opened_again_keeps_its_tab_its_chat_unseen() {
    let rig = rig(false);
    let (shell, cx) = shell_window(&rig, fixtures::MEETING);
    for (tab, selector) in [(ChatTab::Recap, "chat-tab-recap"), (ChatTab::Shared, "chat-tab-shared")] {
        click(cx, selector);
        reopen(&shell, cx);
        assert_eq!(cx.update(|_, cx| shell.read(cx).chat_tab()), tab);
        // the chat under the tab is neither shown (no read receipts go) nor typed into
        assert!(!chat_shown(&shell, cx), "{tab:?}");
        click(cx, "chat-tab-chat");
        assert!(chat_shown(&shell, cx));
    }
}

#[test]
fn a_refresh_keeps_the_recording_shown_when_an_earlier_one_went() {
    let rig = rig(false);
    let (shell, cx) = shell_window(&rig, fixtures::MEETING);
    click(cx, "chat-tab-recap");
    click(cx, "recap-part-1");
    let view = recap(&shell, cx);
    let shown = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| {
            let view = view.read(cx);
            let part = view.parts().and_then(|parts| parts.get(view.selected()).cloned());
            part.and_then(|part| part.item_id)
        })
    };
    assert_eq!(shown(cx).as_deref(), Some("ITEM2"));
    // the first recording deleted elsewhere, then Refresh: the second is still the one shown, now Part 1
    let left: Vec<_> = fixtures::recorded()
        .into_iter()
        .filter(|content| content.item_id.as_deref() != Some("ITEM1"))
        .collect();
    locked(&rig.core.meeting_contents).insert(fixtures::MEETING.to_owned(), left);
    cx.update(|_, cx| view.update(cx, RecapView::read));
    draw(cx);
    assert_eq!(cx.update(|_, cx| view.read(cx).selected()), 0);
    assert_eq!(shown(cx).as_deref(), Some("ITEM2"));
}

#[test]
fn a_recap_that_held_nothing_is_read_again_when_shown_again() {
    let rig = rig(false);
    locked(&rig.core.meeting_contents).insert(fixtures::MEETING.to_owned(), Vec::new());
    let (_shell, cx) = shell_window(&rig, fixtures::MEETING);
    click(cx, "chat-tab-recap");
    assert!(cx.debug_bounds("recap-part-0").is_none());
    assert_eq!(asked(&rig, "meeting_contents"), 1);
    // the recording saved since: the tab shown again reads it
    locked(&rig.core.meeting_contents).insert(fixtures::MEETING.to_owned(), fixtures::recorded());
    click(cx, "chat-tab-chat");
    click(cx, "chat-tab-recap");
    assert_eq!(asked(&rig, "meeting_contents"), 2);
    assert!(cx.debug_bounds("recap-part-0").is_some());
    // a recap that held something isn't read again
    click(cx, "chat-tab-chat");
    click(cx, "chat-tab-recap");
    assert_eq!(asked(&rig, "meeting_contents"), 2);
}
