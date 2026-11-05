//! Teams and channels (M2 Task 16) on GPUI's test context, over the fake engine (`common`): the sidebar lists the
//! teams with their channels from the store, "Loading teams…" before a read settled them, again when the engine tells
//! the teams changed; a channel clicked opens as Teams' channel, synced first: its posts as a chat's messages, each
//! post's replies summed up under it, a box posting to the channel, and a post's thread beside it (its replies, its
//! box replying to the post's chain), reactions, edits and deletes as in a chat; the trace holds ids and kinds, never
//! a name. Every id and name is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

mod common;

use std::sync::Arc;

use common::{LAB_A, LAB_B, Rig, Written};
use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};
use teams_core::{Action, EngineError, Event, NameFrom, Person, READING_TEAMS};
use teams_store::{ChannelKind, Message, Team, TeamChannel};
use teams_ui::chat_view::{Body, ChatView, MessageAction, MessageItem, Place};
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::{Main, Shell, ShellParts};
use teams_ui::sidebar::Section;
use teams_ui::teams_list::{LOADING, UNREADABLE};
use teams_ui::{ChannelView, TeamsList, ViewTrace};

const TEAM: &str = "19:channel0001@thread.tacv2";
const CHANNEL: &str = "19:channel0002@thread.tacv2";
const ARCHIVED: &str = "19:channel0004@thread.tacv2";
const POST: &str = "1760000300000";
const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const LAB_CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000c@unq.gbl.spaces";

fn teams() -> Vec<Team> {
    let channel = |id: &str, name: &str, general: bool| TeamChannel {
        id: id.to_owned(),
        name: name.to_owned(),
        general,
        kind: ChannelKind::Standard,
        about: teams_store::ChannelAbout::default(),
        shown: true,
    };
    vec![
        Team {
            id: TEAM.to_owned(),
            name: "Made-up Team".to_owned(),
            archived: false,
            group_id: None,
            mailbox: None,
            owner: false,
            can_add_channel: false,
            can_add_private_channel: false,
            about: teams_store::TeamAbout::default(),
            channels: vec![
                channel(TEAM, "General", true),
                channel(CHANNEL, "Made-up Channel", false),
            ],
        },
        Team {
            id: ARCHIVED.to_owned(),
            name: "Old Team".to_owned(),
            archived: true,
            group_id: None,
            mailbox: None,
            owner: false,
            can_add_channel: false,
            can_add_private_channel: false,
            about: teams_store::TeamAbout::default(),
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

/// The channel view the shell shows.
fn channel_view(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Entity<ChannelView> {
    cx.update(|_, cx| shell.read(cx).channel().cloned().expect("a channel view"))
}

/// The channel's posts' view.
fn posts(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Entity<ChatView> {
    let view = channel_view(shell, cx);
    cx.update(|_, cx| view.read(cx).posts().clone())
}

/// The rows a view shows.
fn rows(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Vec<MessageItem> {
    cx.update(|_, cx| view.read(cx).message_items().cloned().collect())
}

/// The ids of the messages a view shows, the oldest first.
fn shown(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Vec<String> {
    rows(view, cx).into_iter().map(|item| item.id).collect()
}

/// The thread open beside the posts: its post and its view.
fn thread(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Option<(String, Entity<ChatView>)> {
    let view = channel_view(shell, cx);
    cx.update(|_, cx| {
        view.read(cx)
            .thread()
            .map(|(post, view)| (post.to_owned(), view.clone()))
    })
}

/// Moves the pointer over message `id`, so its hover bar shows.
fn hover(cx: &mut VisualTestContext, id: &str) {
    let row = bounds(cx, &format!("message-{id}")).unwrap_or_else(|| panic!("message {id} is drawn"));
    cx.simulate_mouse_move(row.center(), None, Modifiers::none());
    cx.run_until_parked();
}

/// The shell with the channel's store in the fake engine, `CHANNEL` opened from the sidebar.
fn opened(rig: &Rig, trace: Arc<ViewTrace>) -> (Entity<Shell>, &'static mut VisualTestContext) {
    *rig.core.teams.lock().unwrap() = Some(teams());
    let (messages, roots) = channel_store();
    rig.core.messages.lock().unwrap().insert(CHANNEL.to_owned(), messages);
    rig.core.roots.lock().unwrap().insert(CHANNEL.to_owned(), roots);
    let (shell, cx) = shell_window(rig, trace);
    click(cx, "channel-row-1");
    (shell, cx)
}

/// Nothing was written: no write, resend or typing asked of the engine.
fn assert_nothing_written(rig: &Rig) {
    let asked = rig.core.asked();
    for write in ["write", "resend", "transient"] {
        assert!(!asked.contains(&write), "{write} asked: {asked:?}");
    }
    assert!(rig.core.actions.lock().unwrap().is_empty());
}

#[test]
fn channel_text_selection_copies_posts_from_the_model_and_the_context_menu() {
    use teams_ui::selection::{MessageKey, Spot};

    let rig = rig();
    let (shell, cx) = opened(&rig, Arc::new(ViewTrace::off()));
    let view = posts(&shell, cx);
    assert!(!cx.update(|_, cx| view.update(cx, |view, cx| view.copy_selection(cx))));
    let selection = cx.update(|_, cx| view.read(cx).text_selection().clone());
    selection.select(
        Spot {
            message: MessageKey::new((1_760_000_300_000, 0), POST),
            path: vec![0],
            offset: 0,
        },
        Spot {
            message: MessageKey::new((1_760_000_400_000, 0), "1760000400000"),
            path: vec![0],
            offset: "second post".len(),
        },
    );
    assert!(cx.update(|_, cx| view.update(cx, |view, cx| view.copy_selection(cx))));
    let expected = "first post\n\nsecond post";
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()).as_deref(),
        Some(expected)
    );
    selection.open_menu(gpui::point(gpui::px(400.), gpui::px(200.)));
    cx.update(|window, _| window.refresh());
    click(cx, "selection-copy");
    assert!(selection.menu().is_none());
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()).as_deref(),
        Some(expected)
    );
    assert_nothing_written(&rig);
}

#[test]
fn channel_pictures_open_in_order_and_a_missing_target_leaves_the_viewer_closed() {
    let rig = rig();
    *rig.core.teams.lock().unwrap() = Some(teams());
    let first = "https://us-api.asm.skype.com/v1/objects/0-eus-d9-channel-first/views/imgo";
    let second = "https://us-api.asm.skype.com/v1/objects/0-eus-d9-channel-second/views/imgo";
    let picture = |id, src| {
        message(
            id,
            LAB_B,
            &format!(
                "<p><img src=\"{src}\" itemtype=\"http://schema.skype.com/AMSImage\" width=\"64\" height=\"32\" alt=\"synthetic\"></p>"
            ),
        )
    };
    let mut bytes = Vec::new();
    image::RgbaImage::from_pixel(64, 32, image::Rgba([40, 120, 200, 255]))
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    let png = Arc::new(teams_proto::PastedPicture {
        format: teams_proto::PastedFormat::Png,
        bytes,
    });
    rig.core
        .pictures
        .lock()
        .unwrap()
        .extend([(first.to_owned(), png.clone()), (second.to_owned(), png)]);
    rig.core.messages.lock().unwrap().insert(
        CHANNEL.to_owned(),
        vec![picture("1760000300100", second), picture(POST, first)],
    );
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    click(cx, "channel-row-1");
    let view = posts(&shell, cx);
    cx.update(|_, cx| view.update(cx, |view, cx| view.open_picture("missing", first, cx)));
    assert!(cx.update(|_, cx| teams_ui::picture_viewer::shown(cx).is_none()));
    click(cx, "message-text-1760000300000-open-[0]");
    let viewer = cx.update(|_, cx| teams_ui::picture_viewer::shown(cx).unwrap());
    assert_eq!(cx.update(|_, cx| viewer.read(cx).at()), (0, 2));
    cx.update(|_, cx| viewer.update(cx, teams_ui::picture_viewer::PictureViewer::next));
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| viewer.read(cx).at()), (1, 2));
    cx.update(|_, cx| teams_ui::picture_viewer::close(cx));
    assert_nothing_written(&rig);
}

#[test]
fn a_channel_forward_opens_its_original_in_the_mock_browser_when_that_chat_is_not_listed() {
    let rig = rig();
    *rig.core.teams.lock().unwrap() = Some(teams());
    let fixture: serde_json::Value = serde_json::from_str(
        &include_str!("../../../tests/fixtures/chatsvc/forward.json")
            .replace("<guid:c>", &LAB_C["8:orgid:".len()..])
            .replace("<group>", "000000000000000000000000000group"),
    )
    .unwrap();
    let forward = &fixture["read_back"][0];
    let mut post = message(POST, LAB_B, forward["content"].as_str().unwrap());
    post.properties = Some(forward["properties"].to_string());
    rig.core.messages.lock().unwrap().insert(CHANNEL.to_owned(), vec![post]);
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    click(cx, "channel-row-1");
    let view = posts(&shell, cx);
    let original = rows(&view, cx)[0]
        .forwarded
        .as_ref()
        .unwrap()
        .original
        .as_ref()
        .unwrap()
        .clone();
    click(cx, "forward-time-1760000300000");
    assert_eq!(cx.opened_url(), original.web_link);
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Channel(CHANNEL.to_owned())
    );
    assert_nothing_written(&rig);
}

#[test]
fn a_channel_clicked_opens_with_its_posts_their_replies_summed_up_and_a_box_posting_to_it() {
    let rig = rig();
    let written = Written::default();
    let (shell, cx) = opened(&rig, Arc::new(ViewTrace::new(written.clone())));
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Channel(CHANNEL.to_owned())
    );
    assert_eq!(*rig.core.synced.lock().unwrap(), [CHANNEL]);
    let view = posts(&shell, cx);
    assert_eq!(cx.update(|_, cx| view.read(cx).place().clone()), Place::Channel);
    // the posts, the oldest first; a reply to a post the store doesn't hold is in no post's summary
    assert_eq!(shown(&view, cx), [POST, "1760000400000"]);
    let summaries: Vec<Option<(usize, Vec<String>)>> = rows(&view, cx)
        .into_iter()
        .map(|item| {
            item.replies.map(|replies| {
                let names = replies.people.iter().map(|(name, _)| name.to_string()).collect();
                (replies.count, names)
            })
        })
        .collect();
    // the latest replier first, each once
    assert_eq!(
        summaries,
        [
            Some((2, vec!["Carried Name".to_owned(), "Carried Name".to_owned()])),
            None
        ]
    );
    for selector in [
        "channel-header",
        "channel-view",
        "compose",
        "compose-box",
        &format!("replies-{POST}"),
    ] {
        assert!(bounds(cx, selector).is_some(), "{selector} is drawn");
    }
    // the box writes to the channel, sends no typing and takes no file: a channel's files go to its team's site
    assert!(bounds(cx, "channel-read-only").is_none());
    for absent in ["compose-attach", "compose-loop", "compose-apps"] {
        assert!(bounds(cx, absent).is_none(), "{absent} is drawn");
    }
    assert!(bounds(cx, "replies-1760000400000").is_none());
    assert_nothing_written(&rig);
    assert_posts_traced(&written);
}

/// The trace has the channel's posts shown, by id, and no text or name.
fn assert_posts_traced(written: &Written) {
    let shown = written
        .lines()
        .into_iter()
        .rfind(|line| line["view"] == "channel-view" && line["change"] == "shown")
        .expect("traced");
    assert_eq!(shown["conversation"], CHANNEL);
    assert_eq!(shown["messages"], serde_json::json!([POST, "1760000400000"]));
    for text in ["first post", "Carried Name", "Made-up"] {
        assert!(!written.text().contains(text), "{text} traced");
    }
}

#[test]
fn a_message_typed_in_a_channels_box_is_posted_to_the_channel_with_no_typing_sent() {
    let rig = rig();
    let (shell, cx) = opened(&rig, Arc::new(ViewTrace::off()));
    let view = posts(&shell, cx);
    let compose = cx.update(|_, cx| view.read(cx).compose().clone());
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.focus(window, cx)));
    cx.simulate_input("A new post");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let actions = rig.core.actions.lock().unwrap().clone();
    let [Action::Send { conversation, text }] = actions.as_slice() else {
        panic!("one post: {actions:?}")
    };
    assert_eq!(conversation, CHANNEL);
    assert_eq!(text.html(), "<p>A new post</p>");
    assert!(rig.core.transients.lock().unwrap().is_empty(), "no typing in a channel");
}

#[test]
fn a_posts_reply_in_thread_opens_its_thread_beside_the_posts_whose_box_replies_to_its_chain() {
    let rig = rig();
    let written = Written::default();
    let (shell, cx) = opened(&rig, Arc::new(ViewTrace::new(written.clone())));
    assert!(thread(&shell, cx).is_none());
    hover(cx, POST);
    // Teams' bar on a channel's post: the quick reactions, react, reply in thread, more; no quote
    for selector in ["pick-like", "action-react", "action-reply-in-thread", "message-more"] {
        assert!(bounds(cx, selector).is_some(), "{selector} is drawn");
    }
    assert!(bounds(cx, "action-reply").is_none());
    click(cx, "action-reply-in-thread");
    let (post, view) = thread(&shell, cx).expect("the thread opens");
    assert_eq!(post, POST);
    assert_eq!(
        cx.update(|_, cx| view.read(cx).place().clone()),
        Place::Thread { post: POST.to_owned() }
    );
    // the post and its replies, the oldest first
    assert_eq!(shown(&view, cx), [POST, "1760000300100", "1760000300200"]);
    for selector in ["channel-thread", "channel-thread-header", "compose-send-to"] {
        assert!(bounds(cx, selector).is_some(), "{selector} is drawn");
    }
    // its box has the focus, as Teams' does: what is typed is a reply, posted to the post's chain
    cx.simulate_input("A reply");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let actions = rig.core.actions.lock().unwrap().clone();
    let [Action::Send { conversation, text }] = actions.as_slice() else {
        panic!("one reply: {actions:?}")
    };
    assert_eq!(conversation, &format!("{CHANNEL};messageid={POST}"));
    assert_eq!(text.html(), "<p>A reply</p>");
    click(cx, "channel-thread-close");
    assert!(thread(&shell, cx).is_none());
    assert!(bounds(cx, "channel-thread").is_none());
    let changes: Vec<String> = written
        .lines()
        .into_iter()
        .filter(|line| line["view"] == "channel-thread")
        .map(|line| line["change"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(changes, ["opened", "closed"]);
}

#[test]
fn a_posts_replies_summary_opens_its_thread_too() {
    let rig = rig();
    let (shell, cx) = opened(&rig, Arc::new(ViewTrace::off()));
    click(cx, &format!("replies-{POST}"));
    let (post, view) = thread(&shell, cx).expect("the thread opens");
    assert_eq!(post, POST);
    assert_eq!(shown(&view, cx), [POST, "1760000300100", "1760000300200"]);
    assert_nothing_written(&rig);
}

#[test]
fn a_post_and_a_reply_take_the_accounts_reaction_in_the_channel_as_a_chats_message_does() {
    let rig = rig();
    let (shell, cx) = opened(&rig, Arc::new(ViewTrace::off()));
    hover(cx, "1760000400000");
    click(cx, "pick-like");
    click(cx, &format!("replies-{POST}"));
    hover(cx, "1760000300100");
    click(cx, "pick-heart");
    let actions = rig.core.actions.lock().unwrap().clone();
    let reacted: Vec<(String, String, String)> = actions
        .into_iter()
        .filter_map(|action| match action {
            Action::React {
                conversation,
                message_id,
                key,
            } => Some((conversation, message_id, key)),
            _ => None,
        })
        .collect();
    let expected = [(CHANNEL, "1760000400000", "like"), (CHANNEL, "1760000300100", "heart")]
        .map(|(conversation, message, key)| (conversation.to_owned(), message.to_owned(), key.to_owned()));
    assert_eq!(reacted, expected);
    let _ = shell;
}

#[test]
fn the_accounts_own_reply_in_a_thread_is_deleted_from_its_more_options_after_asking() {
    const MINE: &str = "1760000300200";
    let rig = rig();
    let (shell, cx) = opened(&rig, Arc::new(ViewTrace::off()));
    click(cx, &format!("replies-{POST}"));
    let (_, view) = thread(&shell, cx).expect("the thread opens");
    let actions = |id: &str, cx: &mut VisualTestContext| {
        rows(&view, cx)
            .into_iter()
            .find(|item| item.id == id)
            .map(|item| item.actions())
            .unwrap_or_default()
    };
    // in a thread a message is no reply's target: no reply of its own; the account's own is edited and deleted
    let mine = actions(MINE, cx);
    assert!(
        mine.contains(&MessageAction::Edit) && mine.contains(&MessageAction::Delete),
        "{mine:?}"
    );
    assert!(!mine.contains(&MessageAction::Reply) && !mine.contains(&MessageAction::ReplyInThread));
    let theirs = actions("1760000300100", cx);
    assert!(!theirs.contains(&MessageAction::Delete), "{theirs:?}");
    hover(cx, MINE);
    click(cx, "message-more");
    click(cx, "action-delete");
    click(cx, "confirm-delete");
    let written = rig.core.actions.lock().unwrap().clone();
    assert!(
        written.contains(&Action::Delete {
            conversation: CHANNEL.to_owned(),
            message_id: MINE.to_owned(),
        }),
        "{written:?}"
    );
}

#[test]
fn the_accounts_own_reply_edited_in_its_thread_is_saved_in_the_channel() {
    const MINE: &str = "1760000300200";
    let rig = rig();
    let (shell, cx) = opened(&rig, Arc::new(ViewTrace::off()));
    click(cx, &format!("replies-{POST}"));
    hover(cx, MINE);
    click(cx, "action-edit");
    cx.simulate_input(" again");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let actions = rig.core.actions.lock().unwrap().clone();
    let [
        Action::Edit {
            conversation,
            message_id,
            ..
        },
    ] = actions.as_slice()
    else {
        panic!("one edit: {actions:?}")
    };
    // where the reply is stored, never the post's chain its box posts new replies to
    assert_eq!((conversation.as_str(), message_id.as_str()), (CHANNEL, MINE));
    let _ = shell;
}

#[test]
fn a_channel_message_found_opens_the_channel_at_its_post_read_back_from_the_service() {
    const OLD_POST: &str = "1750000000000";
    let rig = rig();
    *rig.core.teams.lock().unwrap() = Some(teams());
    let (messages, roots) = channel_store();
    rig.core.messages.lock().unwrap().insert(CHANNEL.to_owned(), messages);
    rig.core.roots.lock().unwrap().insert(CHANNEL.to_owned(), roots);
    // a post older than the store holds, at the service
    rig.core.older.lock().unwrap().insert(
        CHANNEL.to_owned(),
        vec![vec![message(OLD_POST, LAB_C, "<p>old post</p>")]],
    );
    let written = Written::default();
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::new(written.clone())));
    let target = teams_ui::search::HitTarget {
        conversation: CHANNEL.to_owned(),
        message: OLD_POST.to_owned(),
        arrival_time: 1_750_000_000_000,
        thread: Some(OLD_POST.to_owned()),
    };
    shell.update_in(cx, |shell, window, cx| shell.open_message(target, window, cx));
    cx.run_until_parked();
    let view = posts(&shell, cx);
    assert!(shown(&view, cx).contains(&OLD_POST.to_owned()), "read back to it");
    let revealed: Vec<serde_json::Value> = written
        .lines()
        .into_iter()
        .filter(|line| line["view"] == "channel-view" && line["change"] == "revealed")
        .collect();
    assert_eq!(revealed.len(), 1, "{}", written.text());
    assert_eq!(revealed[0]["messages"], serde_json::json!([OLD_POST]));
}

#[test]
fn a_reply_found_opens_its_posts_thread_at_it() {
    let rig = rig();
    *rig.core.teams.lock().unwrap() = Some(teams());
    let (messages, roots) = channel_store();
    rig.core.messages.lock().unwrap().insert(CHANNEL.to_owned(), messages);
    rig.core.roots.lock().unwrap().insert(CHANNEL.to_owned(), roots);
    let written = Written::default();
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::new(written.clone())));
    let target = teams_ui::search::HitTarget {
        conversation: CHANNEL.to_owned(),
        message: "1760000300100".to_owned(),
        arrival_time: 1_760_000_300_100,
        thread: Some(POST.to_owned()),
    };
    shell.update_in(cx, |shell, window, cx| shell.open_message(target, window, cx));
    cx.run_until_parked();
    let (post, view) = thread(&shell, cx).expect("its thread opens");
    assert_eq!(post, POST);
    assert_eq!(
        cx.update(|_, cx| view.read(cx).selected().map(str::to_owned)),
        Some("1760000300100".to_owned())
    );
    assert!(
        written
            .lines()
            .iter()
            .any(|line| line["view"] == "thread-view" && line["change"] == "revealed")
    );
}

/// A thread event from the channel itself: `ThreadActivity/AddMember` adding a lone bot, which Teams draws nothing for.
fn bot_added(id: &str) -> Message {
    let mut event = message(
        id,
        CHANNEL,
        "<addmember><initiator>8:orgid:00000000-0000-4000-8000-00000000000a</initiator>\
        <target>28:00000000-0000-4000-8000-0000000000bb</target></addmember>",
    );
    event.message_type = "ThreadActivity/AddMember".to_owned();
    event.sender_name = None;
    event
}

/// A thread event from the channel itself, of `kind` with `content`.
fn channel_event(id: &str, kind: &str, content: &str) -> Message {
    let mut event = message(id, CHANNEL, content);
    event.message_type = kind.to_owned();
    event.sender = Some(CHANNEL.to_owned());
    event.sender_name = None;
    event
}

#[test]
fn the_channels_own_events_are_not_posts_as_teams_list_filters_them_out() {
    let rig = rig();
    *rig.core.teams.lock().unwrap() = Some(teams());
    let (mut messages, mut roots) = channel_store();
    let renamed =
        r#"{"oldValue":"Made-up Old","newValue":"Made-up New","user":"8:orgid:00000000-0000-4000-8000-00000000000a"}"#;
    let events = [
        // the service sent the rename twice, then the description unchanged
        ("1760000600000", "ThreadActivity/TopicTopicUpdated", renamed),
        ("1760000600001", "ThreadActivity/TopicTopicUpdated", renamed),
        (
            "1760000600002",
            "ThreadActivity/TopicDescriptionUpdated",
            r#"{"oldValue":null,"newValue":null,"user":"8:orgid:00000000-0000-4000-8000-00000000000a"}"#,
        ),
        (
            "1760000600003",
            "ThreadActivity/MemberJoined",
            r#"{"eventtime":1,"initiator":"8:orgid:00000000-0000-4000-8000-00000000000a","members":[{"id":"8:orgid:00000000-0000-4000-8000-00000000000b","friendlyname":"Made-up B"}]}"#,
        ),
    ];
    for (at, (id, kind, content)) in events.into_iter().enumerate() {
        messages.insert(at, channel_event(id, kind, content));
        roots.push((id.to_owned(), id.to_owned()));
    }
    // a lone bot added, and a reply under another
    messages.insert(0, message("1760000500100", LAB_B, "<p>welcome, bot</p>"));
    messages.insert(1, bot_added("1760000500000"));
    for (id, root) in [("1760000500100", "1760000500000"), ("1760000500000", "1760000500000")] {
        roots.push((id.to_owned(), root.to_owned()));
    }
    rig.core.messages.lock().unwrap().insert(CHANNEL.to_owned(), messages);
    rig.core.roots.lock().unwrap().insert(CHANNEL.to_owned(), roots);
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    click(cx, "channel-row-1");
    // the channel's posts, and none of its own events
    assert_eq!(shown(&posts(&shell, cx), cx), [POST, "1760000400000"]);
}

#[test]
fn a_post_with_a_file_or_card_no_capture_showed_says_so_with_its_link_in_teams_on_the_web() {
    let rig = rig();
    *rig.core.teams.lock().unwrap() = Some(teams());
    let (mut messages, roots) = channel_store();
    for message in &mut messages {
        message.properties = match message.id.as_str() {
            POST => Some(r#"{"files":"[{\"fileName\":\"made-up.txt\"}]","cards":"[]"}"#.to_owned()),
            "1760000400000" => Some(r#"{"files":"[]","cards":"[]","mentions":"[]"}"#.to_owned()),
            _ => None,
        };
    }
    rig.core.messages.lock().unwrap().insert(CHANNEL.to_owned(), messages);
    rig.core.roots.lock().unwrap().insert(CHANNEL.to_owned(), roots);
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    click(cx, "channel-row-1");
    let unshown: Vec<bool> = rows(&posts(&shell, cx), cx)
        .into_iter()
        .map(|item| item.unshown.any())
        .collect();
    assert_eq!(unshown, [true, false]);
    assert!(bounds(cx, &format!("unshown-{POST}")).is_some());
    assert!(bounds(cx, "unshown-1760000400000").is_none());
    // the post's own link in Teams on the web, as a channel's card button opens it
    click(cx, &format!("open-on-the-web-{POST}"));
    assert_eq!(
        cx.opened_url().as_deref(),
        Some(format!("https://teams.cloud.microsoft/l/message/{CHANNEL}/{POST}?parentMessageId={POST}").as_str())
    );
}

#[test]
fn a_channel_shown_reads_again_when_it_changes() {
    let rig = rig();
    *rig.core.teams.lock().unwrap() = Some(teams());
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    click(cx, "channel-row-1");
    let view = posts(&shell, cx);
    assert!(shown(&view, cx).is_empty());
    let (messages, roots) = channel_store();
    rig.core.messages.lock().unwrap().insert(CHANNEL.to_owned(), messages);
    rig.core.roots.lock().unwrap().insert(CHANNEL.to_owned(), roots);
    rig.send(Event::ConversationChanged {
        conversation: CHANNEL.to_owned(),
    });
    cx.run_until_parked();
    assert_eq!(shown(&view, cx), [POST, "1760000400000"]);
}

#[test]
fn a_channel_opened_again_is_synced_again() {
    let rig = rig();
    *rig.core.teams.lock().unwrap() = Some(teams());
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    click(cx, "channel-row-1");
    let view = channel_view(&shell, cx);
    // its first sync is over (and found nothing): opened again, the same view asks again
    click(cx, "channel-row-1");
    let again = channel_view(&shell, cx);
    assert_eq!(view, again, "the view is kept");
    assert_eq!(*rig.core.synced.lock().unwrap(), [CHANNEL, CHANNEL]);
}

#[test]
fn a_channel_shows_a_profile_read_after_it_was_shown() {
    let rig = rig();
    let (shell, cx) = opened(&rig, Arc::new(ViewTrace::off()));
    let view = posts(&shell, cx);
    let post_sender = |cx: &mut VisualTestContext| -> String { rows(&view, cx)[0].sender_name.to_string() };
    assert_eq!(post_sender(cx), "Carried Name", "no profile yet");
    // b's profile is read in the background: the channel's own messages didn't change, the name shown does
    rig.core.people.lock().unwrap().insert(
        LAB_B.to_owned(),
        Person {
            mri: LAB_B.to_owned(),
            name: Some("Profile Bee".to_owned()),
            from: Some(NameFrom::Graph),
            picture: None,
            looking: false,
        },
    );
    rig.send(Event::Profile { mri: LAB_B.to_owned() });
    cx.run_until_parked();
    assert_eq!(post_sender(cx), "Profile Bee");
}

fn teams_of(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Entity<TeamsList> {
    cx.update(|_, cx| shell.read(cx).teams().clone())
}

#[test]
fn the_sidebar_lists_the_teams_with_their_channels_from_the_store() {
    let rig = rig();
    let written = Written::default();
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::new(written.clone())));
    assert!(bounds(cx, "section-teams").is_some());
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
fn a_section_or_a_team_folds_and_unfolds_by_its_header() {
    let rig = rig();
    *rig.core.teams.lock().unwrap() = Some(teams());
    let written = Written::default();
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::new(written.clone())));
    let sidebar = cx.update(|_, cx| shell.read(cx).sidebar().clone());
    assert!(bounds(cx, "channel-row-2").is_some());

    // the first team folded: its channels go, the next team's keep their numbers
    click(cx, "team-0");
    assert!(!cx.update(|_, cx| sidebar.read(cx).team_open(TEAM)));
    for selector in ["channel-row-0", "channel-row-1"] {
        assert!(bounds(cx, selector).is_none(), "{selector} is folded");
    }
    assert!(bounds(cx, "channel-row-2").is_some());
    click(cx, "team-0");
    assert!(bounds(cx, "channel-row-1").is_some());

    // the section folded: no team shows; unfolded, the teams are asked for again
    let asked = rig.core.asked().iter().filter(|asked| **asked == "teams").count();
    click(cx, "section-teams");
    assert!(!cx.update(|_, cx| sidebar.read(cx).is_open(Section::Teams)));
    for selector in ["team-0", "channel-row-0", "teams-note"] {
        assert!(bounds(cx, selector).is_none(), "{selector} is folded");
    }
    click(cx, "section-teams");
    assert!(bounds(cx, "team-0").is_some());
    assert_eq!(
        rig.core.asked().iter().filter(|asked| **asked == "teams").count(),
        asked + 1
    );
    let folds: Vec<(String, String)> = written
        .lines()
        .into_iter()
        .filter(|line| line["view"] == "sidebar" || line["view"] == "team")
        .map(|line| {
            (
                line["view"].as_str().unwrap().to_owned(),
                line["change"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let expected = [
        ("team", "folded"),
        ("team", "unfolded"),
        ("sidebar", "folded"),
        ("sidebar", "unfolded"),
    ]
    .map(|(view, change)| (view.to_owned(), change.to_owned()));
    assert_eq!(folds, expected);
}

#[test]
fn a_channel_opened_takes_the_highlight_from_the_chat_and_back() {
    let rig = rig();
    *rig.core.teams.lock().unwrap() = Some(teams());
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    let teams = teams_of(&shell, cx);
    let list = cx.update(|_, cx| shell.read(cx).list().clone());
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(LAB_CHAT.to_owned(), window, cx)));
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| list.read(cx).selected().map(str::to_owned)),
        Some(LAB_CHAT.to_owned())
    );
    click(cx, "channel-row-1");
    // the channel is the list's selection: no chat row is highlighted (a favourite channel's row would be)
    assert_eq!(
        cx.update(|_, cx| list.read(cx).selected().map(str::to_owned)),
        Some(CHANNEL.to_owned())
    );
    assert_eq!(
        cx.update(|_, cx| teams.read(cx).selected().map(str::to_owned)),
        Some(CHANNEL.to_owned())
    );
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(LAB_CHAT.to_owned(), window, cx)));
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| teams.read(cx).selected().map(str::to_owned)), None);
}

#[test]
fn a_team_list_with_none_says_so() {
    let rig = rig();
    *rig.core.teams.lock().unwrap() = Some(Vec::new());
    let (_, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    assert!(bounds(cx, "teams-note").is_some());
    assert!(bounds(cx, "channel-row-0").is_none());
}

#[test]
fn a_failed_teams_read_says_the_teams_can_t_be_read_until_one_is_read() {
    let rig = rig();
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    let list = teams_of(&shell, cx);
    assert!(!cx.update(|_, cx| list.read(cx).unreadable(cx)), "{LOADING}");
    // a read of another kind failing says nothing of the teams
    rig.send(Event::Failed {
        during: "syncing a conversation",
        error: EngineError::NotAChat,
    });
    cx.run_until_parked();
    assert!(!cx.update(|_, cx| list.read(cx).unreadable(cx)));
    rig.send(Event::Failed {
        during: READING_TEAMS,
        error: EngineError::NotAChat,
    });
    assert!(bounds(cx, "teams-note").is_some(), "{UNREADABLE}");
    assert!(cx.update(|_, cx| list.read(cx).unreadable(cx)));

    // a later read settles them
    *rig.core.teams.lock().unwrap() = Some(teams());
    rig.send(Event::Teams);
    cx.run_until_parked();
    assert!(!cx.update(|_, cx| list.read(cx).unreadable(cx)));
    assert!(bounds(cx, "teams-note").is_none());
    assert!(bounds(cx, "channel-row-0").is_some());
}

/// What the window shows of the channel: the channel view and no chat view, its box posting to the channel, the channel
/// not kept as the selected chat.
fn assert_shown_as_a_channel(shell: &Entity<Shell>, id: &str, cx: &mut VisualTestContext) {
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Channel(id.to_owned())
    );
    assert!(bounds(cx, "channel-view").is_some());
    assert!(bounds(cx, "compose").is_some(), "its box");
    let view = posts(shell, cx);
    assert_eq!(cx.update(|_, cx| view.read(cx).conversation().to_owned()), id);
    assert!(
        cx.update(|_, cx| shell.read(cx).chat().is_none()),
        "a chat view of it is kept"
    );
    let selected = cx.update(|_, cx| shell.read(cx).settings().read(cx).settings().selected_chat.clone());
    assert_eq!(selected, None);
}

#[test]
fn a_channel_opened_as_a_chat_opens_in_the_channel_view() {
    // a channel mention's notification opens its conversation through the shell's chat path
    let rig = rig();
    *rig.core.teams.lock().unwrap() = Some(teams());
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    shell.update_in(cx, |shell, window, cx| shell.open_chat(TEAM.to_owned(), window, cx));
    assert_shown_as_a_channel(&shell, TEAM, cx);
    assert_eq!(*rig.core.synced.lock().unwrap(), [TEAM]);
    assert_nothing_written(&rig);
}

#[test]
fn a_channel_the_list_doesn_t_show_opens_under_a_plain_header() {
    let rig = rig();
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    shell.update_in(cx, |shell, window, cx| {
        shell.open_channel_id(CHANNEL.to_owned(), window, cx);
    });
    assert_shown_as_a_channel(&shell, CHANNEL, cx);
    assert!(bounds(cx, "channel-header").is_some());
    assert_nothing_written(&rig);
}

#[test]
fn a_channel_kept_as_the_selected_chat_opens_as_a_channel_once_the_list_knows_it() {
    let rig = rig();
    *rig.core.teams.lock().unwrap() = Some(teams());
    let parts = ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: rig.cx.update(|cx| {
            cx.new(|_| {
                SettingsModel::in_memory(Settings {
                    selected_chat: Some(TEAM.to_owned()),
                    ..Settings::default()
                })
            })
        }),
        trace: Arc::new(ViewTrace::off()),
    };
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    cx.run_until_parked();
    assert_shown_as_a_channel(&shell, TEAM, cx);
}

#[test]
fn once_the_engine_stopped_the_teams_shown_stay() {
    let rig = rig();
    *rig.core.teams.lock().unwrap() = Some(teams());
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    let shutdown = cx.update(|_, cx| rig.bridge.update(cx, teams_ui::Bridge::shutdown));
    shutdown.detach();
    cx.run_until_parked();
    assert_eq!(rig.shutdowns(), 1);
    *rig.core.stopped.lock().unwrap() = true;
    // folded and unfolded: asked for again
    click(cx, "section-teams");
    click(cx, "section-teams");
    let list = teams_of(&shell, cx);
    assert_eq!(
        cx.update(|_, cx| list.read(cx).teams().map(<[Team]>::len)),
        Some(2),
        "answered stopped, the list keeps what it showed"
    );
}

#[test]
fn the_switcher_lists_the_channels_under_the_chats_and_opens_one() {
    let rig = rig();
    *rig.core.teams.lock().unwrap() = Some(teams());
    let written = Written::default();
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::new(written.clone())));
    cx.simulate_keystrokes("ctrl-k");
    cx.run_until_parked();
    let switcher = cx.update(|_, cx| shell.read(cx).switcher().cloned()).expect("open");
    let names = |cx: &mut VisualTestContext| -> Vec<String> {
        cx.update(|_, cx| {
            switcher
                .read(cx)
                .channels()
                .iter()
                .map(|(_, name)| name.to_string())
                .collect()
        })
    };
    // every channel at first, each under its team's name, in the list's order
    assert_eq!(
        names(cx),
        [
            "Made-up Team › General",
            "Made-up Team › Made-up Channel",
            "Old Team › General"
        ]
    );
    cx.simulate_input("made chan");
    cx.run_until_parked();
    assert_eq!(
        names(cx).first().map(String::as_str),
        Some("Made-up Team › Made-up Channel")
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_shown_as_a_channel(&shell, CHANNEL, cx);
    assert!(cx.update(|_, cx| shell.read(cx).switcher().is_none()), "closed");
    assert_nothing_written(&rig);
    assert!(
        written
            .lines()
            .iter()
            .any(|line| line["view"] == "channel" && line["change"] == "opened" && line["conversation"] == CHANNEL)
    );
    assert!(!written.text().contains("Made-up"), "the trace holds a name");
}

/// A post of a meeting scheduled in the channel (or its cancellation's reply), synthetic in the shape seen 2026-11-03.
fn meeting_post(id: &str, title: &str, uid: &str, cancelled: bool) -> Message {
    let kind = if cancelled { "Cancelled" } else { "Created" };
    let meeting = serde_json::json!({
        "@type": format!("http://schema.skype.com/ScheduledMeeting{kind}"),
        "meetingtitle": title,
        "scheduledmeetinginfo": {"startTime": "2026-11-03T05:00:00+00:00", "endTime": "2026-11-03T05:30:00+00:00",
            "location": "Microsoft Teams Meeting", "iCalUid": uid, "eventType": "Single", "isCancelled": cancelled},
        "organizerId": "00000000-0000-4000-8000-00000000000a",
    });
    let mut message = message(
        id,
        LAB_B,
        if cancelled {
            "The meeting is cancelled"
        } else {
            "Scheduled a meeting"
        },
    );
    message.properties = Some(serde_json::json!({ "meeting": meeting.to_string() }).to_string());
    message
}

#[test]
fn a_meeting_scheduled_in_the_channel_is_drawn_as_its_card_cancelled_once_its_reply_says_so() {
    let rig = rig();
    *rig.core.teams.lock().unwrap() = Some(teams());
    let messages = vec![
        meeting_post("1760000400200", "Planning", "0a0b", true),
        meeting_post("1760000400100", "Review", "0c0d", false),
        meeting_post(POST, "Planning", "0a0b", false),
    ];
    let roots = [
        ("1760000400200", POST),
        ("1760000400100", "1760000400100"),
        (POST, POST),
    ]
    .map(|(message, root)| (message.to_owned(), root.to_owned()))
    .to_vec();
    rig.core.messages.lock().unwrap().insert(CHANNEL.to_owned(), messages);
    rig.core.roots.lock().unwrap().insert(CHANNEL.to_owned(), roots);
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::new(Written::default())));
    click(cx, "channel-row-1");
    let view = posts(&shell, cx);
    let cards: Vec<(String, bool, bool)> = rows(&view, cx)
        .into_iter()
        .filter_map(|post| match &post.body {
            Body::Meeting(card) => Some((card.title.to_string(), card.cancelled, card.place.is_some())),
            _ => None,
        })
        .collect();
    assert_eq!(
        cards,
        [("Planning".to_owned(), true, true), ("Review".to_owned(), false, true)]
    );
    for selector in [
        format!("channel-meeting-card-{POST}"),
        "channel-meeting-card-1760000400100".to_owned(),
    ] {
        assert!(bounds(cx, &selector).is_some(), "{selector} is drawn");
    }
    // the cancellation's reply is its text in the post's thread, never a card
    click(cx, &format!("replies-{POST}"));
    let (_, thread) = thread(&shell, cx).expect("the thread opens");
    let reply = rows(&thread, cx)
        .into_iter()
        .find(|item| item.id == "1760000400200")
        .expect("the reply");
    assert!(matches!(reply.body, Body::Text(_)), "{:?}", reply.body);
}

#[test]
fn the_card_of_a_meeting_the_account_scheduled_opens_its_details_from_the_team_s_calendar() {
    let rig = rig();
    let mut teams = teams();
    teams[0].mailbox = Some("made-up-team@lab.example.test".to_owned());
    *rig.core.teams.lock().unwrap() = Some(teams);
    // the account's meeting (its organizer is a), and a cancelled one: only the first offers Edit
    let messages = vec![
        meeting_post("1760000400200", "Planning", "0a0b", true),
        meeting_post("1760000400100", "Review", "0c0d", false),
        meeting_post(POST, "Planning", "0a0b", false),
    ];
    let roots = [
        ("1760000400200", POST),
        ("1760000400100", "1760000400100"),
        (POST, POST),
    ]
    .map(|(message, root)| (message.to_owned(), root.to_owned()))
    .to_vec();
    rig.core.messages.lock().unwrap().insert(CHANNEL.to_owned(), messages);
    rig.core.roots.lock().unwrap().insert(CHANNEL.to_owned(), roots);
    let data = serde_json::json!({"cid": CHANNEL, "mid": 1_760_000_400_100_u64, "uid": "8:orgid:00000000-0000-4000-8000-00000000000a"});
    let event = serde_json::json!({
        "objectId": "AQMkMadeUp", "iCalUID": "0c0d", "subject": "Review",
        "startTime": "2026-11-03T05:00:00+00:00", "endTime": "2026-11-03T05:30:00+00:00",
        "isOnlineMeeting": true, "skypeTeamsData": data.to_string(),
    });
    let full = teams_proto::ChannelEventRead::new("made-up-team@lab.example.test", "0c0d")
        .unwrap()
        .read_answer(&serde_json::to_vec(&event).unwrap())
        .unwrap();
    rig.core.calendar.lock().unwrap().full.insert("0c0d".to_owned(), full);
    let (_shell, cx) = shell_window(&rig, Arc::new(ViewTrace::new(Written::default())));
    click(cx, "channel-row-1");
    assert!(
        bounds(cx, "channel-meeting-edit-1760000400100").is_some(),
        "its organizer's Edit"
    );
    assert!(
        bounds(cx, &format!("channel-meeting-edit-{POST}")).is_none(),
        "none on a cancelled one"
    );
    click(cx, "channel-meeting-edit-1760000400100");
    assert_eq!(
        rig.core.calendar.lock().unwrap().full_reads,
        ["made-up-team@lab.example.test/0c0d"]
    );
    let form = cx
        .update(|_, cx| teams_ui::event_form::shown(cx))
        .expect("the form opens");
    cx.update(|_, cx| {
        let form = form.read(cx);
        assert!(form.changes());
        assert_eq!(form.place().map(|place| place.name.as_str()), Some("Made-up Channel"));
    });
    cx.update(|_, cx| teams_ui::event_form::close(cx));
    cx.run_until_parked();
}

/// The service for a channel post's menu: one workflow run from a message, on.
#[expect(clippy::unnecessary_wraps, reason = "a workflow handler answers a service result")]
fn message_workflow_answer(request: &teams_proto::WorkflowRequest) -> teams_proto::WorkflowResponse {
    use serde_json::json;
    use teams_proto::WorkflowRequest;
    let card = json!({"type":"AdaptiveCard","body":[{"type":"Input.Text","id":"note","label":"Note"}]}).to_string();
    Ok(match request {
        WorkflowRequest::Environments => {
            json!({"value":[{"name":"Default-synthetic","properties":{"displayName":"Synthetic"}}]})
        }
        WorkflowRequest::List { .. } => json!({"value":[{"name":"synthetic-follow-flow","properties":{
            "displayName":"Follow up on a message","state":"Started",
            "definitionSummary":{"triggers":[{"swaggerOperationId":"ForASelectedMessageV2"}]}}}]}),
        WorkflowRequest::Get { .. } => json!({"name":"synthetic-follow-flow","properties":{
            "displayName":"Follow up on a message","state":"Started","definition":{"triggers":{"manual":{
                "type":"Request","kind":"ApiConnection",
                "inputs":{"operationId":"ForASelectedMessageV2","parameters":{"inputsAdaptiveCard": card}}}},"actions":{}}}}),
        _ => json!({"value": []}),
    })
}

#[test]
fn a_posts_more_options_runs_a_workflow_on_it_or_opens_workflows_for_it() {
    let rig = rig();
    *rig.core.teams.lock().unwrap() = Some(teams());
    *rig.core.workflow_handler.lock().unwrap() = Some(message_workflow_answer);
    let (messages, roots) = channel_store();
    rig.core.messages.lock().unwrap().insert(CHANNEL.to_owned(), messages);
    rig.core.roots.lock().unwrap().insert(CHANNEL.to_owned(), roots);
    let (_shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    click(cx, "channel-row-1");
    hover(cx, POST);
    click(cx, "message-more");
    assert!(bounds(cx, "message-menu").is_some());
    click(cx, "message-workflow-synthetic-follow-flow");
    assert!(bounds(cx, "message-menu").is_none(), "the menu closes");
    assert!(bounds(cx, "workflow-run").is_some(), "the run dialog opens");
    assert!(
        bounds(cx, "workflow-run-input-note").is_some(),
        "with the workflow's question"
    );
    click(cx, "workflow-run-close");
    assert!(bounds(cx, "workflow-run").is_none());
    hover(cx, POST);
    click(cx, "message-more");
    click(cx, "message-more-actions");
    click(cx, "message-create-workflows");
    assert!(
        bounds(cx, "workflow-dialog").is_some(),
        "the Workflows dialog for the post"
    );
}

#[test]
fn a_reply_in_a_posts_thread_runs_a_workflow_with_its_post() {
    const REPLY: &str = "1760000300100";
    let rig = rig();
    *rig.core.teams.lock().unwrap() = Some(teams());
    *rig.core.workflow_handler.lock().unwrap() = Some(message_workflow_answer);
    let (messages, roots) = channel_store();
    rig.core.messages.lock().unwrap().insert(CHANNEL.to_owned(), messages);
    rig.core.roots.lock().unwrap().insert(CHANNEL.to_owned(), roots);
    let (_shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    click(cx, "channel-row-1");
    click(cx, &format!("replies-{POST}"));
    hover(cx, REPLY);
    click(cx, "message-more");
    click(cx, "message-workflow-synthetic-follow-flow");
    click(cx, "workflow-run-submit");
    let requests = rig.core.workflow_requests.lock().unwrap().clone();
    let run = requests
        .iter()
        .find_map(|request| match request {
            teams_proto::WorkflowRequest::RunOnMessage { message, .. } => Some(message.clone()),
            _ => None,
        })
        .expect("the run went");
    assert_eq!(run.id, REPLY);
    assert_eq!(run.reply_to.as_deref(), Some(POST), "the reply's post");
    assert_eq!(run.conversation, CHANNEL);
    assert!(
        run.link.ends_with(&format!("/{REPLY}?parentMessageId={POST}")),
        "a channel message's link: {}",
        run.link
    );
    assert_eq!(run.team.as_ref().map(|(team, _)| team.as_str()), Some(TEAM));
}

/// A workflow's card post (synthetic: the webhook template's shape, every word made up), in the channel as Teams draws
/// it: the sender's name, the card on its surface with its title, and a button that sends to the bot opening the post
/// in Teams on the web; never the placeholder.
#[test]
fn a_workflows_card_post_is_drawn_as_its_card() {
    use base64::Engine as _;
    let card = serde_json::json!({
        "type": "message/card",
        "attachments": [{"contentType": "application/vnd.microsoft.card.adaptive", "content": {
            "type": "AdaptiveCard",
            "body": [
                {"type": "TextBlock", "text": "Made-up webhook check", "weight": "bolder", "wrap": true},
                {"type": "TextBlock", "text": "A made-up line.", "wrap": true}
            ],
            "actions": [{"type": "Action.Submit", "title": "Acknowledge"}]
        }}]
    });
    let b64 = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&card).unwrap());
    let id = "1760000500000";
    let mut post = message(
        id,
        "28:358f0194-6b0e-4dd3-af35-c24fe8a9ec87",
        &format!("<URIObject type=\"SWIFT.1\">Card<Swift b64=\"{b64}\"></Swift></URIObject>"),
    );
    post.message_type = "RichText/Media_Card".to_owned();
    post.sender_name = Some("Workflows".to_owned());
    let rig = rig();
    *rig.core.teams.lock().unwrap() = Some(teams());
    let (mut messages, mut roots) = channel_store();
    messages.insert(0, post);
    roots.push((id.to_owned(), id.to_owned()));
    rig.core.messages.lock().unwrap().insert(CHANNEL.to_owned(), messages);
    rig.core.roots.lock().unwrap().insert(CHANNEL.to_owned(), roots);
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    click(cx, "channel-row-1");
    let post = rows(&posts(&shell, cx), cx)
        .into_iter()
        .find(|item| item.id == id)
        .expect("its post");
    assert!(matches!(post.body, Body::Cards(_)), "{:?}", post.body);
    assert_eq!(post.sender_name.as_ref(), "Workflows");
    assert!(bounds(cx, &format!("card-{id}-c0b0.")).is_some(), "the card's title");
    click(cx, &format!("card-{id}-c0a0"));
    assert_eq!(
        cx.opened_url().as_deref(),
        Some(format!("https://teams.cloud.microsoft/l/message/{CHANNEL}/{id}?parentMessageId={id}").as_str())
    );
    assert_nothing_written(&rig);
}
