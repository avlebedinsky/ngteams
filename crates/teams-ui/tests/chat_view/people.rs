//! Names and pictures in the views (M2 Task 12), on GPUI's test context over the fake engine (`common`): the chat
//! list names a 1:1 chat by the other member's profile name and shows their picture, a group by its members' names
//! (a name the binary knew among them); the chat view names a sender by their profile over the name their message
//! carried, with their avatar, and an MRI only when nothing names them; a profile's change (`Event::Profile`) shows in
//! both. A picture reaches a view only as bytes from the engine. Every id, name and byte here is made up.

use std::sync::Arc;

use super::common::{LAB_A, LAB_B, Rig};
use gpui::{AppContext as _, Entity, ImageFormat, TestAppContext, VisualTestContext, px, size};
use teams_core::{Event, NameFrom, Person};
use teams_store::{Conversation, ConversationKind, Member, Message, StoredPicture};
use teams_ui::ViewTrace;
use teams_ui::chat_list::ChatList;
use teams_ui::chat_view::{ChatView, MessageItem};
use teams_ui::people::{Face, Shown, image_of};
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::{Shell, ShellParts};

const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const OUTSIDER: &str = "8:orgid:00000000-0000-4000-8000-0000000000ff";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const GROUP: &str = "19:000000000000000000000000000group@thread.v2";
const PICTURE: [u8; 4] = [0x89, b'P', b'N', b'G'];

fn picture() -> StoredPicture {
    StoredPicture {
        media_type: "image/png".to_owned(),
        bytes: PICTURE.to_vec(),
    }
}

fn person(mri: &str, name: &str, from: NameFrom, picture: Option<StoredPicture>) -> Person {
    Person {
        mri: mri.to_owned(),
        name: Some(name.to_owned()),
        from: Some(from),
        picture,
        looking: false,
    }
}

fn message(conversation: &str, id: &str, sender: &str, carried: Option<&str>, at: i64) -> Message {
    Message {
        conversation_id: conversation.to_owned(),
        id: id.to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: carried.map(str::to_owned),
        message_type: "RichText/Html".to_owned(),
        content: format!("<p>message {id}</p>"),
        properties: None,
        arrival_time: at,
        compose_time: Some(at),
        edit_time: None,
        delete_time: None,
    }
}

/// a's store: the a–b chat and an untitled group of a, b and c. b has a profile from Graph with a picture, and their
/// messages carry another name; c has only a name the binary knew; someone outside the lab wrote in the group and
/// nothing names them.
fn rig() -> Rig {
    let rig = Rig::start();
    let core = &rig.core;
    core.conversations.lock().unwrap().extend([
        Conversation {
            id: AB.to_owned(),
            kind: ConversationKind::OneToOne,
            topic: None,
            version: 1,
            last_activity: Some(3_000),
        },
        Conversation {
            id: GROUP.to_owned(),
            kind: ConversationKind::Group,
            topic: None,
            version: 1,
            last_activity: Some(2_000),
        },
    ]);
    let members = |mris: &[&str]| mris.iter().map(|mri| Member::new(mri, None)).collect::<Vec<_>>();
    core.members
        .lock()
        .unwrap()
        .insert(AB.to_owned(), members(&[LAB_A, LAB_B]));
    core.members
        .lock()
        .unwrap()
        .insert(GROUP.to_owned(), members(&[LAB_A, LAB_B, LAB_C]));
    core.messages.lock().unwrap().insert(
        AB.to_owned(),
        vec![
            message(AB, "300", LAB_A, Some("lab-a"), 3_000),
            message(AB, "200", LAB_B, Some("Name From Message"), 2_000),
            message(AB, "100", OUTSIDER, None, 1_000),
        ],
    );
    let mut people = core.people.lock().unwrap();
    people.insert(
        LAB_B.to_owned(),
        person(LAB_B, "Graph Bee", NameFrom::Graph, Some(picture())),
    );
    people.insert(LAB_C.to_owned(), person(LAB_C, "lab-c", NameFrom::Known, None));
    drop(people);
    rig.cx.update(teams_ui::window::init);
    rig
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

fn list_window(rig: &Rig) -> (Entity<ChatList>, &'static mut VisualTestContext) {
    let parts = ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: rig
            .cx
            .update(|cx| cx.new(|_| SettingsModel::in_memory(Settings::default()))),
        trace: Arc::new(ViewTrace::off()),
    };
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    draw(cx);
    let list = cx.update(|_, cx| shell.read(cx).list().clone());
    (list, cx)
}

fn chat_window(rig: &Rig) -> (Entity<ChatView>, &'static mut VisualTestContext) {
    let parts = super::parts::parts(rig, Settings::default(), Arc::new(ViewTrace::off()));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| ChatView::new(parts, AB.to_owned(), window, cx));
    cx.simulate_resize(size(px(800.0), px(600.0)));
    draw(cx);
    (view, cx)
}

fn item(view: &Entity<ChatView>, cx: &mut VisualTestContext, id: &str) -> MessageItem {
    cx.update(|_, cx| view.read(cx).message_items().find(|item| item.id == id).cloned())
        .unwrap_or_else(|| panic!("message {id} shown"))
}

#[test]
fn the_chat_list_names_people_by_their_profiles_and_shows_a_1_to_1_partner_s_picture() {
    let rig = rig();
    let (list, cx) = list_window(&rig);
    let row = |cx: &mut VisualTestContext, id: &str| cx.update(|_, cx| list.read(cx).row(id).cloned()).unwrap();

    let ab = row(cx, AB);
    assert_eq!((ab.name.as_ref(), ab.named), ("Graph Bee", true));
    let shown = ab.picture.expect("b's picture");
    assert_eq!((shown.format, shown.bytes.as_slice()), (ImageFormat::Png, &PICTURE[..]));
    let group = row(cx, GROUP);
    // a group without a topic by its members' first names, as Teams joins them
    assert_eq!((group.name.as_ref(), group.named), ("Graph and lab-c", true));
    assert_eq!(group.picture, None, "a group shows no member's picture");

    // b's profile changes in the background: every row named by b is built again
    rig.core
        .people
        .lock()
        .unwrap()
        .insert(LAB_B.to_owned(), person(LAB_B, "Renamed Bee", NameFrom::Graph, None));
    rig.send(Event::Profile { mri: LAB_B.to_owned() });
    draw(cx);
    let ab = row(cx, AB);
    assert_eq!((ab.name.as_ref(), ab.picture), ("Renamed Bee", None));
    assert_eq!(row(cx, GROUP).name.as_ref(), "lab-c and Renamed");
}

#[test]
fn the_chat_view_names_a_sender_by_their_profile_over_their_message_with_their_avatar() {
    let rig = rig();
    let (view, cx) = chat_window(&rig);

    let from_b = item(&view, cx, "200");
    assert_eq!((from_b.sender_name.as_ref(), from_b.face.named), ("Graph Bee", true));
    assert_eq!(
        from_b.face.picture.as_ref().map(|image| image.bytes.clone()),
        Some(PICTURE.to_vec())
    );
    // nothing names the outsider: their MRI, no initials
    let outsider = item(&view, cx, "100");
    assert_eq!(
        (outsider.sender_name.as_ref(), outsider.face),
        (OUTSIDER, Face::default())
    );
    // someone else's message has an avatar drawn; the account's own none
    for id in ["100", "200"] {
        assert!(
            cx.debug_bounds(Box::leak(format!("avatar-{id}").into_boxed_str()))
                .is_some(),
            "{id}"
        );
    }
    assert!(cx.debug_bounds("avatar-300").is_none());

    // without a profile name, the name the message carried wins over a name from elsewhere
    rig.core.people.lock().unwrap().insert(
        LAB_B.to_owned(),
        person(LAB_B, "Newest Message Name", NameFrom::Message, None),
    );
    rig.send(Event::Profile { mri: LAB_B.to_owned() });
    draw(cx);
    let from_b = item(&view, cx, "200");
    assert_eq!(
        (from_b.sender_name.as_ref(), from_b.face.picture),
        ("Name From Message", None)
    );

    // the outsider named later (a name the binary knew): shown once the engine tells it
    rig.core
        .people
        .lock()
        .unwrap()
        .insert(OUTSIDER.to_owned(), person(OUTSIDER, "Someone", NameFrom::Known, None));
    rig.send(Event::Profile {
        mri: OUTSIDER.to_owned(),
    });
    draw(cx);
    let outsider = item(&view, cx, "100");
    assert_eq!((outsider.sender_name.as_ref(), outsider.face.named), ("Someone", true));
}

#[test]
fn a_picture_is_handed_to_gpui_only_as_the_engine_s_bytes_of_a_kept_format() {
    let jpeg = StoredPicture {
        media_type: "image/jpeg".to_owned(),
        bytes: vec![0xFF, 0xD8],
    };
    let image = image_of(&jpeg).expect("a JPEG");
    assert_eq!(
        (image.format, image.bytes.as_slice()),
        (ImageFormat::Jpeg, &[0xFF, 0xD8][..])
    );
    // formatted, a picture shows its format and size, never its bytes; nor does anything that holds one
    assert_eq!(format!("{image:?}"), "Picture { format: Jpeg, bytes: 2 }");
    let face = Face {
        named: true,
        picture: Some(image),
    };
    assert!(!format!("{face:?}").contains("255"), "{face:?}");
    for media_type in ["image/gif", "image/svg+xml", "text/html", ""] {
        let other = StoredPicture {
            media_type: media_type.to_owned(),
            bytes: vec![1],
        };
        assert!(image_of(&other).is_none(), "{media_type}");
    }
    let shown = Shown::of(person(LAB_B, "Graph Bee", NameFrom::Graph, Some(picture())));
    assert!(shown.profiled());
    assert_eq!(shown.name.as_ref().map(AsRef::as_ref), Some("Graph Bee"));
    let known = Shown::of(person(LAB_C, "lab-c", NameFrom::Known, None));
    assert!(!known.profiled());
    assert_eq!(known.picture, None);
}

#[test]
fn an_answer_to_an_earlier_ask_never_replaces_a_later_one_in_the_chat_view() {
    let rig = rig();
    // b's first answer is held back, as an engine answering a batch of people one by one would be slow on it
    rig.core.hold_people.lock().unwrap().insert(LAB_B.to_owned());
    let (view, cx) = chat_window(&rig);

    // b's profile changes meanwhile: b is asked again and that answer comes at once
    rig.core.hold_people.lock().unwrap().clear();
    rig.core.people.lock().unwrap().insert(
        LAB_B.to_owned(),
        person(LAB_B, "Graph Bee Renamed", NameFrom::Graph, None),
    );
    rig.send(Event::Profile { mri: LAB_B.to_owned() });
    draw(cx);
    assert_eq!(item(&view, cx, "200").sender_name.as_ref(), "Graph Bee Renamed");

    // the first ask's answer, made before the change, comes last: it is dropped
    let held = std::mem::take(&mut *rig.core.held.lock().unwrap());
    assert_eq!(held.len(), 1);
    for (answer, person) in held {
        answer.send(Ok(person));
    }
    draw(cx);
    let from_b = item(&view, cx, "200");
    assert_eq!(
        (from_b.sender_name.as_ref(), from_b.face.picture),
        ("Graph Bee Renamed", None)
    );
}
