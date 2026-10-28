//! A forwarded message drawn as Teams draws one (`teams_ui::forwarded`), on GPUI's test context over the fake engine:
//! its content in a card under the arrow, the original's sender (their profile's name and picture, asked of the
//! engine) and the original's time in full; its picture drawn as a message's own, never a placeholder; a click on the
//! time tells the original, which the shell opens in the app when its chat list has the chat, else in Teams on the
//! web. The messages are the synthetic fixture `tests/fixtures/chatsvc/forward.json`: every name, link and id is made
//! up.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{AppContext as _, Bounds, Entity, Modifiers, Pixels, TestAppContext, VisualTestContext, px, size};
use serde_json::Value;
use teams_core::{NameFrom, Person};
use teams_store::{Conversation, ConversationKind, Message, StoredPicture};
use teams_ui::ViewTrace;
use teams_ui::chat_view::{ChatViewEvent, MessageAction};
use teams_ui::forwarded::Original;
use teams_ui::search::HitTarget;
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::{Shell, ShellParts};

use super::common::{LAB_A, LAB_B, Rig};
use super::{AB, NOON, chat_window, draw, items, message, rig_with, trace};

const FIXTURE: &str = include_str!("../../../../tests/fixtures/chatsvc/forward.json");
const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const GROUP: &str = "19:000000000000000000000000000group@thread.v2";
const PICTURE: &str = "https://us-api.asm.skype.com/v1/objects/0-eus-d9-0000000000000000000000000000f0d0/views/imgo";
/// The original messages' ids (their arrival times) and sent times, as the fixture gives them.
const FIRST_ORIGINAL: &str = "1760000040123";
const FIRST_SENT: i64 = 1_767_225_640_123;

/// The fixture's messages, from b in the a–b chat, as the store keeps them (`properties` JSON text), 501 and 502.
fn forwards() -> Vec<Message> {
    let text = FIXTURE
        .replace("<region>", "amer")
        .replace("<guid:a>", &LAB_A["8:orgid:".len()..])
        .replace("<guid:b>", &LAB_B["8:orgid:".len()..])
        .replace("<guid:c>", &LAB_C["8:orgid:".len()..])
        .replace("<group>", "000000000000000000000000000group");
    let fixture: Value = serde_json::from_str(&text).unwrap();
    (0..2)
        .map(|at| {
            let read_back = &fixture["read_back"][at];
            let id = format!("50{}", at + 1);
            let sent = NOON + i64::try_from(at).unwrap() * 60_000;
            let mut stored = message(&id, LAB_B, read_back["content"].as_str().unwrap(), sent);
            stored.properties = Some(read_back["properties"].to_string());
            stored
        })
        .collect()
}

/// A made-up PNG, `width` by `height`.
fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::RgbaImage::from_pixel(width, height, image::Rgba([200, 120, 40, 255]))
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    bytes
}

/// The rig with the forwards, c named by Graph with a picture, the forwarded picture's bytes on the engine, and the
/// group chat the originals were sent in.
fn rig() -> Rig {
    let rig = rig_with(forwards());
    let core = &rig.core;
    core.people.lock().unwrap().insert(
        LAB_C.to_owned(),
        Person {
            mri: LAB_C.to_owned(),
            name: Some("Lab Sea".to_owned()),
            from: Some(NameFrom::Graph),
            picture: Some(StoredPicture {
                media_type: "image/png".to_owned(),
                bytes: png(8, 8),
            }),
            looking: false,
        },
    );
    core.pictures.lock().unwrap().insert(
        PICTURE.to_owned(),
        Arc::new(teams_proto::PastedPicture {
            format: teams_proto::PastedFormat::Png,
            bytes: png(127, 250),
        }),
    );
    core.conversations.lock().unwrap().extend([
        Conversation {
            id: AB.to_owned(),
            kind: ConversationKind::OneToOne,
            topic: None,
            version: 1,
            last_activity: Some(NOON),
        },
        Conversation {
            id: GROUP.to_owned(),
            kind: ConversationKind::Group,
            topic: Some("Made-up group".to_owned()),
            version: 1,
            last_activity: Some(NOON - 1),
        },
    ]);
    // the first forward's original is stored, the second's isn't
    let mut original = message(FIRST_ORIGINAL, LAB_C, "<p>the original</p>", 1_760_000_040_123);
    original.conversation_id = GROUP.to_owned();
    core.messages.lock().unwrap().insert(GROUP.to_owned(), vec![original]);
    rig
}

fn bounds(cx: &mut VisualTestContext, selector: &str) -> Option<Bounds<Pixels>> {
    draw(cx);
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
}

fn within(inner: Bounds<Pixels>, outer: Bounds<Pixels>) -> bool {
    inner.origin.x >= outer.origin.x
        && inner.origin.y >= outer.origin.y
        && inner.origin.x + inner.size.width <= outer.origin.x + outer.size.width + px(1.0)
        && inner.origin.y + inner.size.height <= outer.origin.y + outer.size.height + px(1.0)
}

#[test]
fn a_forward_is_drawn_in_its_card_under_where_it_came_from() {
    let rig = rig();
    let (written_trace, _) = trace();
    let (view, cx) = chat_window(&rig, written_trace);
    cx.simulate_resize(size(px(900.0), px(900.0)));
    draw(cx);
    let shown = items(&view, cx);
    let origin = shown[0].forwarded.clone().expect("the first is a forward");
    assert_eq!(origin.sender.as_deref(), Some(LAB_C));
    assert_eq!(
        origin.name.as_deref(),
        Some("Lab Sea"),
        "the original's sender by their profile"
    );
    assert!(origin.face.named && origin.face.picture.is_some(), "with their picture");
    assert_eq!(origin.sent_at, Some(FIRST_SENT));
    let original = origin.original.expect("the original named in full");
    assert_eq!(
        original.target,
        HitTarget {
            conversation: GROUP.to_owned(),
            message: FIRST_ORIGINAL.to_owned(),
            arrival_time: 1_760_000_040_123,
        }
    );
    assert_eq!(
        original.web_link.as_deref(),
        teams_ui::rich_text::message_web_link(GROUP, FIRST_ORIGINAL).as_deref()
    );
    assert!(shown[1].forwarded.is_some());

    for id in ["501", "502"] {
        let bubble = bounds(cx, &format!("bubble-{id}")).expect("the bubble");
        let header = bounds(cx, &format!("forward-header-{id}")).expect("the card's header");
        let time = bounds(cx, &format!("forward-time-{id}")).expect("the original's time");
        assert!(
            within(header, bubble) && within(time, header),
            "{time:?} {header:?} {bubble:?}"
        );
    }
    // the forwarded picture is drawn, at its size, under the header: never its placeholder
    let picture = bounds(cx, "rich-picture").expect("the picture's block");
    let header = bounds(cx, "forward-header-502").unwrap();
    let bubble = bounds(cx, "bubble-502").unwrap();
    assert!(
        picture.origin.y >= header.origin.y + header.size.height,
        "{picture:?} {header:?}"
    );
    assert!(within(picture, bubble), "{picture:?} {bubble:?}");
    assert!(
        picture.size.height > px(240.0),
        "drawn at its height, not a line of text: {picture:?}"
    );
}

#[test]
fn a_click_on_the_original_s_time_tells_the_original() {
    let rig = rig();
    let (written_trace, _) = trace();
    let (view, cx) = chat_window(&rig, written_trace);
    let told: Rc<RefCell<Vec<ChatViewEvent>>> = Rc::default();
    let kept = Rc::clone(&told);
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &ChatViewEvent, _| {
            kept.borrow_mut().push(event.clone());
        })
        .detach();
    });
    let time = bounds(cx, "forward-time-501").expect("the original's time");
    cx.simulate_click(time.center(), Modifiers::none());
    draw(cx);
    let told = told.borrow().clone();
    let [ChatViewEvent::OpenOriginal(original)] = &told[..] else {
        panic!("the original told once: {told:?}");
    };
    assert_eq!(original.target.message, FIRST_ORIGINAL);
    assert_eq!(rig.shutdowns(), 0);
}

/// The shell over the rig, the a–b chat open; and which chat it shows.
fn shell_window(rig: &Rig) -> (Entity<Shell>, &'static mut VisualTestContext) {
    let parts = ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: rig.cx.update(|cx| {
            cx.new(|_| {
                SettingsModel::in_memory(Settings {
                    selected_chat: Some(AB.to_owned()),
                    ..Settings::default()
                })
            })
        }),
        trace: Arc::new(ViewTrace::off()),
    };
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    cx.simulate_resize(size(px(1200.0), px(900.0)));
    draw(cx);
    (shell, cx)
}

fn shown_chat(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Option<String> {
    cx.update(|_, cx| {
        shell
            .read(cx)
            .chat()
            .map(|chat| chat.read(cx).conversation().to_owned())
    })
}

#[test]
fn the_shell_opens_the_original_in_its_chat_or_on_the_web() {
    let rig = rig();
    let (shell, cx) = shell_window(&rig);
    assert_eq!(shown_chat(&shell, cx).as_deref(), Some(AB));
    let time = bounds(cx, "forward-time-501").expect("the original's time");
    cx.simulate_click(time.center(), Modifiers::none());
    draw(cx);
    assert_eq!(
        shown_chat(&shell, cx).as_deref(),
        Some(GROUP),
        "the chat list has it: opened in the app"
    );
    assert_eq!(cx.opened_url(), None, "nothing opened on the web");

    // an original the store doesn't hold (older than the chat's stored pages): Teams on the web, at the message
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(AB.to_owned(), window, cx)));
    let time = bounds(cx, "forward-time-502").expect("the second original's time");
    cx.simulate_click(time.center(), Modifiers::none());
    draw(cx);
    assert_eq!(
        cx.opened_url(),
        teams_ui::rich_text::message_web_link(GROUP, "1760000050456")
    );
    assert_eq!(shown_chat(&shell, cx).as_deref(), Some(AB), "the chat shown stays");

    // a chat the account doesn't have: Teams on the web, at the message
    let elsewhere = "19:00000000000000000000000000000fff@thread.v2";
    let original = Original {
        target: HitTarget {
            conversation: elsewhere.to_owned(),
            message: FIRST_ORIGINAL.to_owned(),
            arrival_time: 1_760_000_040_123,
        },
        web_link: teams_ui::rich_text::message_web_link(elsewhere, FIRST_ORIGINAL),
    };
    let link = original.web_link.clone();
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_original(original, window, cx)));
    draw(cx);
    assert_eq!(cx.opened_url(), link);
    assert_eq!(shown_chat(&shell, cx).as_deref(), Some(AB), "the chat shown stays");
}

#[test]
fn a_message_that_wasn_t_forwarded_has_no_card() {
    let rig = rig_with(vec![message(
        "600",
        LAB_B,
        "<blockquote><p>a plain quote</p></blockquote>",
        NOON,
    )]);
    let (written_trace, _) = trace();
    let (view, cx) = chat_window(&rig, written_trace);
    assert!(items(&view, cx)[0].forwarded.is_none());
    assert!(bounds(cx, "forward-header-600").is_none());
    assert!(bounds(cx, "rich-forward").is_none());
    assert!(bounds(cx, "rich-quote").is_some());
}

#[test]
fn the_account_s_own_forward_offers_no_edit() {
    let mut own = forwards().remove(0);
    own.sender = Some(LAB_A.to_owned());
    let rig = rig_with(vec![own, message("601", LAB_A, "<p>an own line</p>", NOON + 120_000)]);
    let (written_trace, _) = trace();
    let (view, cx) = chat_window(&rig, written_trace);
    let shown = items(&view, cx);
    assert!(shown[0].is_forward() && !shown[1].is_forward());
    assert_eq!(
        shown[0].actions(),
        [
            MessageAction::Reply,
            MessageAction::React,
            MessageAction::Copy,
            MessageAction::Delete
        ],
        "a forward is the original's content: deleted, never edited"
    );
    assert!(shown[1].actions().contains(&MessageAction::Edit));
}
