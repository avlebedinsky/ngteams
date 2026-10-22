//! Teams and channels (M2 Task 16) on GPUI's test context, over the fake engine (`common`): the Teams rail lists the
//! teams with their channels from the store, "Loading teams…" before a read settled them, again when the engine tells
//! the teams changed; a channel clicked opens read-only, synced first, its posts each with its replies under it,
//! replies to a post the store doesn't hold under "Replies to an earlier post", no composer and no write asked; the
//! trace holds ids and kinds, never a name. Every id and name is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use common::{LAB_A, LAB_B, Rig, Written};
use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};
use teams_core::Event;
use teams_store::{Message, Team, TeamChannel};
use teams_ui::channel_view::{EARLIER_POST, READ_ONLY, arrange_threads};
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::{Main, RailItem, Shell, ShellParts};
use teams_ui::teams_list::LOADING;
use teams_ui::{ChannelView, TeamsList, ViewTrace};

const TEAM: &str = "19:channel0001@thread.tacv2";
const CHANNEL: &str = "19:channel0002@thread.tacv2";
const ARCHIVED: &str = "19:channel0004@thread.tacv2";
const POST: &str = "1760000300000";
const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";

fn teams() -> Vec<Team> {
    let channel = |id: &str, name: &str, general: bool| TeamChannel {
        id: id.to_owned(),
        name: name.to_owned(),
        general,
    };
    vec![
        Team {
            id: TEAM.to_owned(),
            name: "Made-up Team".to_owned(),
            archived: false,
            channels: vec![
                channel(TEAM, "General", true),
                channel(CHANNEL, "Made-up Channel", false),
            ],
        },
        Team {
            id: ARCHIVED.to_owned(),
            name: "Old Team".to_owned(),
            archived: true,
            channels: vec![channel(ARCHIVED, "General", true)],
        },
    ]
}

fn message(id: &str, sender: &str, content: &str) -> Message {
    let arrival: i64 = id.parse().unwrap();
    Message {
        conversation_id: CHANNEL.to_owned(),
        id: id.to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: Some("Carried Name".to_owned()),
        message_type: "RichText/Html".to_owned(),
        content: content.to_owned(),
        properties: None,
        arrival_time: arrival,
        compose_time: Some(arrival),
        edit_time: None,
        delete_time: None,
    }
}

/// The channel's messages (newest first, as the store pages them) and the post each belongs to: a post by b with
/// replies by c and a, a post by c, and a reply by b to a post the store doesn't hold.
fn channel_store() -> (Vec<Message>, Vec<(String, String)>) {
    let messages = vec![
        message("1760000400100", LAB_B, "<p>late reply</p>"),
        message("1760000400000", LAB_C, "<p>second post</p>"),
        message("1760000300200", LAB_A, "<p>my reply</p>"),
        message("1760000300100", LAB_C, "<p>a reply</p>"),
        message(POST, LAB_B, "<p>first post</p>"),
    ];
    let roots = [
        ("1760000400100", "1759000000000"),
        ("1760000400000", "1760000400000"),
        ("1760000300200", POST),
        ("1760000300100", POST),
        (POST, POST),
    ]
    .map(|(message, root)| (message.to_owned(), root.to_owned()))
    .to_vec();
    (messages, roots)
}

fn rig() -> Rig {
    let rig = Rig::start();
    rig.cx.update(teams_ui::window::init);
    rig
}

fn shell_window(rig: &Rig, trace: Arc<ViewTrace>) -> (Entity<Shell>, &'static mut VisualTestContext) {
    let parts = ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: rig
            .cx
            .update(|cx| cx.new(|_| SettingsModel::in_memory(Settings::default()))),
        trace,
    };
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    cx.run_until_parked();
    (shell, cx)
}

fn bounds(cx: &mut VisualTestContext, selector: &str) -> Option<gpui::Bounds<gpui::Pixels>> {
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
}

fn click(cx: &mut VisualTestContext, selector: &str) {
    let found = bounds(cx, selector).unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(found.center(), Modifiers::none());
    cx.run_until_parked();
}

fn teams_of(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Entity<TeamsList> {
    cx.update(|_, cx| shell.read(cx).teams().clone())
}

#[test]
fn the_teams_rail_lists_the_teams_with_their_channels_from_the_store() {
    let rig = rig();
    let written = Written::default();
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::new(written.clone())));
    click(cx, "rail-teams");
    assert_eq!(cx.update(|_, cx| shell.read(cx).rail()), RailItem::Teams);
    let list = teams_of(&shell, cx);
    assert_eq!(cx.update(|_, cx| list.read(cx).teams().map(<[Team]>::len)), None);
    assert!(bounds(cx, "teams-note").is_some(), "{LOADING}");
    assert!(rig.core.asked().contains(&"teams"));

    // the engine read them in the background and tells they changed
    *rig.core.teams.lock().unwrap() = Some(teams());
    rig.send(Event::Teams);
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| list.read(cx).teams().map(<[Team]>::to_vec)),
        Some(teams())
    );
    for selector in ["team-0", "team-1", "channel-row-0", "channel-row-1", "channel-row-2"] {
        assert!(bounds(cx, selector).is_some(), "{selector} is drawn");
    }
    assert!(bounds(cx, "teams-note").is_none());
    let shown = written
        .lines()
        .into_iter()
        .rfind(|line| line["view"] == "teams-list" && line["change"] == "shown")
        .expect("traced");
    assert_eq!(shown["conversations"], serde_json::json!([TEAM, CHANNEL, ARCHIVED]));
    assert_eq!(shown["kinds"], serde_json::json!(["general", "channel", "general"]));
    for name in ["Made-up", "General", "Old Team"] {
        assert!(!written.text().contains(name), "{name} traced");
    }
}

#[test]
fn a_team_list_with_none_says_so() {
    let rig = rig();
    *rig.core.teams.lock().unwrap() = Some(Vec::new());
    let (_, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    click(cx, "rail-teams");
    assert!(bounds(cx, "teams-note").is_some());
    assert!(bounds(cx, "channel-row-0").is_none());
}

#[test]
fn a_channel_clicked_opens_read_only_with_its_posts_and_their_replies() {
    let rig = rig();
    *rig.core.teams.lock().unwrap() = Some(teams());
    let (messages, roots) = channel_store();
    rig.core.messages.lock().unwrap().insert(CHANNEL.to_owned(), messages);
    rig.core.roots.lock().unwrap().insert(CHANNEL.to_owned(), roots);
    let written = Written::default();
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::new(written.clone())));
    click(cx, "rail-teams");
    click(cx, "channel-row-1");
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Channel(CHANNEL.to_owned())
    );
    assert_eq!(*rig.core.synced.lock().unwrap(), [CHANNEL]);
    let view: Entity<ChannelView> = cx.update(|_, cx| shell.read(cx).channel().cloned().expect("a channel view"));
    // by their post's time, or without it their oldest reply's: the late reply to an earlier post comes last
    assert_eq!(
        thread_shapes(&view, cx),
        [
            shape(POST, Some(POST), &["1760000300100", "1760000300200"]),
            shape("1760000400000", Some("1760000400000"), &[]),
            shape("1759000000000", None, &["1760000400100"]),
        ]
    );
    let names: Vec<String> = cx.update(|_, cx| {
        view.read(cx).threads()[0]
            .replies
            .iter()
            .map(|reply| reply.sender_name.to_string())
            .collect()
    });
    assert_eq!(names, ["Carried Name", "You"]);
    for selector in [
        "channel-header",
        "channel-view",
        "channel-read-only",
        "thread-0",
        "earlier-post-2",
        "channel-message-1760000300100",
    ] {
        assert!(bounds(cx, selector).is_some(), "{selector} is drawn");
    }
    assert!(EARLIER_POST.contains("earlier") && READ_ONLY.contains("read-only"));
    assert_read_only(&rig, cx);
    assert_channel_traced(&written);
}

/// A thread as compared: its post's id, the post's id when it is held, its replies' ids.
type ThreadShape = (String, Option<String>, Vec<String>);

fn shape(root: &str, post: Option<&str>, replies: &[&str]) -> ThreadShape {
    (
        root.to_owned(),
        post.map(str::to_owned),
        replies.iter().map(|reply| (*reply).to_owned()).collect(),
    )
}

fn thread_shapes(view: &Entity<ChannelView>, cx: &mut VisualTestContext) -> Vec<ThreadShape> {
    cx.update(|_, cx| {
        view.read(cx)
            .threads()
            .iter()
            .map(|thread| {
                (
                    thread.root.clone(),
                    thread.post.as_ref().map(|post| post.id.clone()),
                    thread.replies.iter().map(|reply| reply.id.clone()).collect(),
                )
            })
            .collect()
    })
}

/// Nothing in the window writes: no composer or message action is drawn, and no write was asked of the engine.
fn assert_read_only(rig: &Rig, cx: &mut VisualTestContext) {
    for selector in ["compose", "compose-mode", "message-actions"] {
        assert!(bounds(cx, selector).is_none(), "{selector} is drawn");
    }
    let asked = rig.core.asked();
    for write in ["write", "resend", "transient"] {
        assert!(!asked.contains(&write), "{write} asked: {asked:?}");
    }
    assert!(rig.core.actions.lock().unwrap().is_empty());
}

/// The trace has the channel opened and its messages shown in their order with their kinds, and no text or name.
fn assert_channel_traced(written: &Written) {
    let lines = written.lines();
    let opened = lines
        .iter()
        .find(|line| line["view"] == "channel" && line["change"] == "opened")
        .expect("traced");
    assert_eq!(opened["conversation"], CHANNEL);
    let shown = lines
        .iter()
        .rfind(|line| line["view"] == "channel-view" && line["change"] == "shown")
        .expect("traced");
    assert_eq!(
        shown["messages"],
        serde_json::json!([POST, "1760000300100", "1760000300200", "1760000400000", "1760000400100"])
    );
    assert_eq!(
        shown["kinds"],
        serde_json::json!(["post", "reply", "reply", "post", "reply"])
    );
    for text in ["first post", "Carried Name", "Made-up"] {
        assert!(!written.text().contains(text), "{text} traced");
    }
}

#[test]
fn a_channel_shown_reads_again_when_it_changes() {
    let rig = rig();
    *rig.core.teams.lock().unwrap() = Some(teams());
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    click(cx, "rail-teams");
    click(cx, "channel-row-1");
    let view: Entity<ChannelView> = cx.update(|_, cx| shell.read(cx).channel().cloned().expect("a channel view"));
    assert!(cx.update(|_, cx| view.read(cx).threads().is_empty()));
    let (messages, roots) = channel_store();
    rig.core.messages.lock().unwrap().insert(CHANNEL.to_owned(), messages);
    rig.core.roots.lock().unwrap().insert(CHANNEL.to_owned(), roots);
    rig.send(Event::ConversationChanged {
        conversation: CHANNEL.to_owned(),
    });
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| view.read(cx).threads().len()), 3);

    click(cx, "rail-chats");
    assert_eq!(cx.update(|_, cx| shell.read(cx).rail()), RailItem::Chats);
}

#[test]
fn once_the_engine_stopped_the_teams_shown_stay() {
    let rig = rig();
    *rig.core.teams.lock().unwrap() = Some(teams());
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    click(cx, "rail-teams");
    let shutdown = cx.update(|_, cx| rig.bridge.update(cx, teams_ui::Bridge::shutdown));
    shutdown.detach();
    cx.run_until_parked();
    assert_eq!(rig.shutdowns(), 1);
    *rig.core.stopped.lock().unwrap() = true;
    click(cx, "rail-chats");
    click(cx, "rail-teams");
    let list = teams_of(&shell, cx);
    assert_eq!(
        cx.update(|_, cx| list.read(cx).teams().map(<[Team]>::len)),
        Some(2),
        "answered stopped, the list keeps what it showed"
    );
}

#[test]
fn threads_take_their_posts_in_order_with_replies_under_them() {
    let (mut messages, roots) = channel_store();
    // a message whose post wasn't recorded is a post of its own; a control message isn't shown
    messages.push(message("1760000000000", LAB_B, "<p>no post recorded</p>"));
    let mut typing = message("1760000500000", LAB_B, "");
    typing.message_type = "Control/Typing".to_owned();
    messages.push(typing);
    let roots: HashMap<String, String> = roots.into_iter().collect();
    let threads = arrange_threads(messages, &roots);
    let shape: Vec<(&str, bool, usize)> = threads
        .iter()
        .map(|thread| (thread.root.as_str(), thread.post.is_some(), thread.replies.len()))
        .collect();
    assert_eq!(
        shape,
        [
            ("1760000000000", true, 0),
            (POST, true, 2),
            ("1760000400000", true, 0),
            ("1759000000000", false, 1)
        ]
    );
}
