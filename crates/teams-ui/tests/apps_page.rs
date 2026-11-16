//! The rail's Store on GPUI's test context, over a fake core (`teams_ui::apps_page`): it shows only with
//! `NGTEAMS_BOTS=1`; its sections and search; an app's details and Add, which adds it for the account; "Add to a chat"
//! and its picker's Go; "Manage your apps" and a chat's apps, whose Remove asks first; nothing offered that writes in
//! the read-only mode.
//!
//! Every app, chat and id here is made up.

#![cfg(test)]

#[expect(
    dead_code,
    reason = "the helpers every test file shares; these tests use some of them"
)]
mod common;

use std::sync::Arc;

use common::Rig;
use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};
use serde_json::json;
use teams_core::sync::CatchUpReport;
use teams_core::{AppListing, Cause, Event, TeamWritten};
use teams_proto::{StoreApp, StoreSections, TeamWrite};
use teams_store::{Conversation, ConversationKind, Member};
use teams_ui::ViewTrace;
use teams_ui::apps_page::AppsPage;
use teams_ui::apps_page::chat_apps::ChatAppsDialog;
use teams_ui::apps_page::details::AppDetailsDialog;
use teams_ui::bots_flag::BotsFlag;
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::{Main, Shell, ShellParts};

const HELPER: &str = "00000000-0000-4000-8000-00000000a004";
const POLLS: &str = "00000000-0000-4000-8000-00000000a005";
const GROUP_CHAT: &str = "19:made-up-group@thread.v2";
const POLLS_TILE: &str = "https://res.cdn.office.net/teamsappdata/evergreen-assets/apps/made-up-polls_largeImage.png";
const BOT_CHAT: &str = "19:made-up-helper-chat@unq.gbl.spaces";

fn apps() -> Vec<StoreApp> {
    teams_proto::AppDefinitions::read_answer(
        json!([
            {"id": HELPER, "name": "Made-up Helper", "developerName": "Made-up Maker", "shortDescription": "Helps",
             "version": "1.0.0", "permissions": ["Identity"],
             "bots": [{"id": "b4", "scopes": ["personal"]}], "staticTabs": [{"entityId": "home"}]},
            {"id": POLLS, "name": "Made-up Polls", "developerName": "Made-up Maker", "shortDescription": "Polls",
             "largeImageUrl": POLLS_TILE,
             "bots": [{"id": "b5", "scopes": ["groupChat", "team"]}]},
        ])
        .to_string()
        .as_bytes(),
    )
}

fn listing() -> AppListing {
    let sections = StoreSections::read(
        json!({"appCategoriesDictionary": {
            "adminPreInstalledApps": {"apps": [{"id": HELPER, "rating": {"count": 1200, "average": 4.3}}]},
            "popularApps": {"apps": [{"id": HELPER}, {"id": POLLS}]},
        }})
        .to_string()
        .as_bytes(),
    );
    AppListing { sections, apps: apps() }
}

fn app(id: &str) -> StoreApp {
    apps()
        .into_iter()
        .find(|store| store.app.id == id)
        .expect("a made-up app")
}

fn shell_window(rig: &Rig, bots: bool) -> (Entity<Shell>, &'static mut VisualTestContext) {
    shell_window_with(rig, bots, Vec::new())
}

fn shell_window_with(
    rig: &Rig,
    bots: bool,
    more: Vec<Conversation>,
) -> (Entity<Shell>, &'static mut VisualTestContext) {
    rig.cx.update(teams_ui::window::init);
    rig.cx.update(|cx| cx.set_global(BotsFlag(bots)));
    *rig.core.app_listing.lock().unwrap() = listing();
    *rig.core.team_written.lock().unwrap() = Some(TeamWritten::default());
    let mut conversations = vec![Conversation {
        id: GROUP_CHAT.to_owned(),
        kind: ConversationKind::Group,
        topic: Some("Made-up group".to_owned()),
        version: 1,
        last_activity: Some(100),
    }];
    conversations.extend(more);
    *rig.core.conversations.lock().unwrap() = conversations;
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
    cx.simulate_mouse_move(found.center(), None, Modifiers::none());
    cx.simulate_click(found.center(), Modifiers::none());
    cx.run_until_parked();
}

fn writes(rig: &Rig) -> Vec<TeamWrite> {
    rig.core
        .team_writes
        .lock()
        .unwrap()
        .iter()
        .map(|(write, _)| write.clone())
        .collect()
}

fn open_store(shell: &Entity<Shell>, cx: &mut VisualTestContext) {
    click(cx, "rail-apps");
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Apps);
}

#[test]
fn the_store_shows_only_with_the_bots_flag() {
    let rig = Rig::start();
    let (_, cx) = shell_window(&rig, false);
    assert!(bounds(cx, "rail-apps").is_none());
    assert!(bounds(cx, "rail-chats").is_some());
}

/// An own server that does not name `appsService` gets no Store, no app details and no chat's apps, as with Workflows.
#[test]
fn an_own_server_without_apps_offers_no_store_and_no_chat_apps() {
    let rig = Rig::start();
    *rig.core.capabilities.lock().unwrap() = Some(teams_core::Capabilities {
        apps: false,
        ..teams_core::Capabilities::ALL
    });
    *rig.core.chat_apps.lock().unwrap() = vec![app(POLLS)];
    let (shell, cx) = shell_window(&rig, true);
    assert!(bounds(cx, "rail-apps").is_none(), "no Store on the rail");
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.open_apps(window, cx);
            shell.open_app_details(app(HELPER), window, cx);
            shell.open_chat_apps(GROUP_CHAT.to_owned(), window, cx);
            shell.open_chat(GROUP_CHAT.to_owned(), window, cx);
        });
    });
    cx.run_until_parked();
    assert_ne!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Apps);
    assert!(cx.update(|_, cx| shell.read(cx).apps().is_none()));
    assert!(cx.update(|_, cx| shell.read(cx).team_dialog().is_none()));
    assert!(bounds(cx, "chat-header").is_some());
    assert!(bounds(cx, "chat-apps").is_none(), "no Apps in this chat");
    *rig.core.capabilities.lock().unwrap() = None;
    cx.update(|_, cx| shell.update(cx, |_, cx| cx.notify()));
    assert!(bounds(cx, "rail-apps").is_some(), "the Store once the server serves apps");
    assert!(bounds(cx, "chat-apps").is_some());
}

#[test]
fn the_store_lists_the_org_s_and_popular_apps_and_searches_them() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, true);
    open_store(&shell, cx);
    assert!(bounds(cx, "apps-org").is_some(), "Added by your org");
    assert!(bounds(cx, "apps-org-0").is_some());
    assert!(bounds(cx, "apps-tile-popular-1").is_some());
    let page = cx.update(|_, cx| shell.read(cx).apps().cloned()).expect("the page");
    cx.update(|window, cx| page.update(cx, |page, cx| page.set_search("polls", window, cx)));
    assert!(bounds(cx, "apps-results").is_some());
    assert!(bounds(cx, "apps-tile-found-0").is_some());
    assert!(bounds(cx, "apps-tile-found-1").is_none(), "only the app named so");
    cx.update(|window, cx| page.update(cx, |page, cx| page.set_search("nothing like it", window, cx)));
    assert!(bounds(cx, "apps-none").is_some());
    click(cx, "rail-chats");
    assert_ne!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Apps);
}

#[test]
fn an_app_s_details_add_it_for_the_account_with_its_definition() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, true);
    open_store(&shell, cx);
    click(cx, "apps-org-0");
    assert!(bounds(cx, "app-details").is_some());
    assert!(bounds(cx, "app-details-capabilities").is_some());
    assert!(bounds(cx, "app-details-permissions").is_some());
    assert!(bounds(cx, "app-details-stats").is_some(), "its rating");
    assert!(
        bounds(cx, "app-details-add-chat").is_none(),
        "a personal app goes nowhere else"
    );
    click(cx, "app-details-add");
    assert!(
        matches!(writes(&rig).as_slice(), [TeamWrite::AddPersonalApp { app }] if app.id() == HELPER),
        "{:?}",
        writes(&rig)
    );
    assert!(bounds(cx, "app-details-added").is_some(), "the app is added");
    *rig.core.user_apps.lock().unwrap() = vec![app(HELPER)];
    click(cx, "app-details-close");
    assert!(cx.update(|_, cx| shell.read(cx).team_dialog().is_none()));
    assert!(
        bounds(cx, "apps-added-check").is_some(),
        "the store reads the account's apps again"
    );
}

#[test]
fn an_added_bot_app_s_details_open_the_chat_with_its_bot() {
    let rig = Rig::start();
    *rig.core.user_apps.lock().unwrap() = vec![app(HELPER)];
    rig.core.members.lock().unwrap().insert(
        BOT_CHAT.to_owned(),
        vec![Member::new(&rig.core.me, None), Member::new("28:b4", None)],
    );
    let bot_chat = Conversation {
        id: BOT_CHAT.to_owned(),
        kind: ConversationKind::OneToOne,
        topic: None,
        version: 1,
        last_activity: Some(50),
    };
    let (shell, cx) = shell_window_with(&rig, true, vec![bot_chat]);
    // a catch-up drops the list's built rows: the bot's chat is still found while they are built again
    cx.update(|_, cx| {
        rig.models.apply(
            Event::CaughtUp {
                cause: Cause::Connected,
                report: CatchUpReport::default(),
            },
            cx,
        );
    });
    assert_eq!(
        cx.update(|_, cx| shell
            .read(cx)
            .list()
            .read(cx)
            .one_to_one_with("28:b4")
            .map(str::to_owned)),
        Some(BOT_CHAT.to_owned())
    );
    open_store(&shell, cx);
    click(cx, "apps-org-0");
    assert!(bounds(cx, "app-details-added").is_none());
    click(cx, "app-details-open");
    assert!(cx.update(|_, cx| shell.read(cx).team_dialog().is_none()));
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).list().read(cx).selected().map(str::to_owned)),
        Some(BOT_CHAT.to_owned())
    );
}

#[test]
fn an_add_before_add_to_a_chat_shows_the_app_added_in_the_store() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, true);
    open_store(&shell, cx);
    click(cx, "apps-org-0");
    click(cx, "app-details-add");
    assert!(bounds(cx, "app-details-added").is_some());
    *rig.core.user_apps.lock().unwrap() = vec![app(HELPER)];
    let dialog = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .and_then(|dialog| dialog.downcast::<AppDetailsDialog>().ok())
        .expect("the details");
    cx.update(|_, cx| dialog.update(cx, |dialog, cx| dialog.add_to(false, cx)));
    cx.run_until_parked();
    let page = cx.update(|_, cx| shell.read(cx).apps().cloned()).expect("the page");
    assert!(
        cx.update(|_, cx| page.read(cx).added(HELPER)),
        "the store reads the account's apps again"
    );
}

#[test]
fn add_to_a_chat_waits_for_an_add_in_flight() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, true);
    open_store(&shell, cx);
    click(cx, "apps-org-0");
    let dialog = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .and_then(|dialog| dialog.downcast::<AppDetailsDialog>().ok())
        .expect("the details");
    cx.update(|_, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.add(cx);
            dialog.add_to(false, cx);
        });
    });
    cx.run_until_parked();
    assert!(
        bounds(cx, "app-place-dialog").is_none(),
        "the details stay while Add runs"
    );
    assert!(cx.update(|_, cx| dialog.read(cx).added()));
}

#[test]
fn the_details_stay_open_while_an_add_runs() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, true);
    open_store(&shell, cx);
    click(cx, "apps-org-0");
    let dialog = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .and_then(|dialog| dialog.downcast::<AppDetailsDialog>().ok())
        .expect("the details");
    cx.update(|_, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.add(cx);
            dialog.close(cx);
        });
    });
    cx.run_until_parked();
    assert!(
        cx.update(|_, cx| shell.read(cx).team_dialog().is_some()),
        "the details stay while Add runs"
    );
    assert!(cx.update(|_, cx| dialog.read(cx).added()));
    *rig.core.user_apps.lock().unwrap() = vec![app(HELPER)];
    click(cx, "app-details-close");
    assert!(cx.update(|_, cx| shell.read(cx).team_dialog().is_none()));
    assert!(
        bounds(cx, "apps-added-check").is_some(),
        "the store reads the account's apps again"
    );
}

#[test]
fn add_to_a_chat_picks_a_chat_and_go_adds_the_app_there() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, true);
    open_store(&shell, cx);
    click(cx, "apps-tile-popular-1");
    assert!(
        bounds(cx, "app-details-add").is_none(),
        "an app for chats and teams only isn't added for oneself"
    );
    click(cx, "app-details-add-chat");
    assert!(bounds(cx, "app-place-dialog").is_some());
    click(cx, "app-place-0");
    click(cx, "app-place-go");
    assert!(
        matches!(writes(&rig).as_slice(), [TeamWrite::AddChatApp { chat, app }] if chat == GROUP_CHAT && app.id() == POLLS),
        "{:?}",
        writes(&rig)
    );
    assert!(
        cx.update(|_, cx| shell.read(cx).team_dialog().is_none()),
        "the picker closes"
    );
}

#[test]
fn manage_your_apps_removes_an_app_after_asking() {
    let rig = Rig::start();
    *rig.core.user_apps.lock().unwrap() = vec![app(HELPER)];
    let (shell, cx) = shell_window(&rig, true);
    open_store(&shell, cx);
    click(cx, "apps-manage");
    click(cx, "apps-mine-0-expand");
    click(cx, "apps-mine-0-remove");
    assert!(writes(&rig).is_empty(), "it asks first");
    assert!(bounds(cx, "apps-remove-question").is_some());
    click(cx, "apps-remove-confirm");
    assert!(
        matches!(writes(&rig).as_slice(), [TeamWrite::RemovePersonalApp { app }] if app == HELPER),
        "{:?}",
        writes(&rig)
    );
    assert!(bounds(cx, "apps-mine-0").is_none(), "gone from the list");
    assert!(bounds(cx, "apps-mine-none").is_some());
}

#[test]
fn a_refused_removal_says_so_and_keeps_the_app() {
    let rig = Rig::start();
    *rig.core.user_apps.lock().unwrap() = vec![app(HELPER)];
    let (shell, cx) = shell_window(&rig, true);
    *rig.core.team_written.lock().unwrap() = None;
    open_store(&shell, cx);
    click(cx, "apps-manage");
    click(cx, "apps-mine-0-expand");
    click(cx, "apps-mine-0-remove");
    click(cx, "apps-remove-confirm");
    assert!(bounds(cx, "apps-remove-problem").is_some());
    assert!(bounds(cx, "apps-mine-0").is_some());
}

fn manage_page(rig: &Rig, mine: Vec<StoreApp>) -> (Entity<AppsPage>, &'static mut VisualTestContext) {
    *rig.core.user_apps.lock().unwrap() = mine;
    let (shell, cx) = shell_window(rig, true);
    open_store(&shell, cx);
    click(cx, "apps-manage");
    let page = cx.update(|_, cx| shell.read(cx).apps().cloned()).expect("the page");
    (page, cx)
}

fn removes(rig: &Rig) -> usize {
    writes(rig)
        .iter()
        .filter(|write| {
            matches!(
                write,
                TeamWrite::RemovePersonalApp { .. } | TeamWrite::RemoveChatApp { .. }
            )
        })
        .count()
}

#[test]
fn a_second_remove_before_the_reply_sends_no_second_write() {
    let rig = Rig::start();
    let (page, cx) = manage_page(&rig, vec![app(HELPER)]);
    cx.update(|_, cx| {
        page.update(cx, |page, cx| {
            page.ask_remove(app(HELPER), cx);
            page.remove(cx);
            page.remove(cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(removes(&rig), 1, "{:?}", writes(&rig));
    assert!(cx.update(|_, cx| page.read(cx).removing().is_none()));
}

#[test]
fn a_refresh_keeps_the_remove_in_flight() {
    let rig = Rig::start();
    let (page, cx) = manage_page(&rig, vec![app(HELPER)]);
    cx.update(|_, cx| {
        page.update(cx, |page, cx| {
            page.ask_remove(app(HELPER), cx);
            page.remove(cx);
            page.refresh(cx);
        });
    });
    cx.run_until_parked();
    assert!(
        cx.update(|_, cx| page.read(cx).removing().is_none()),
        "the reply came through: the question closed"
    );
}

#[test]
fn a_failed_remove_can_be_tried_again() {
    let rig = Rig::start();
    let (page, cx) = manage_page(&rig, vec![app(HELPER)]);
    *rig.core.team_written.lock().unwrap() = None;
    click(cx, "apps-mine-0-expand");
    click(cx, "apps-mine-0-remove");
    click(cx, "apps-remove-confirm");
    assert!(bounds(cx, "apps-remove-problem").is_some());
    click(cx, "apps-remove-confirm");
    assert_eq!(removes(&rig), 2, "{:?}", writes(&rig));
    assert!(cx.update(|_, cx| page.read(cx).removing().is_some_and(|(_, failed)| failed)));
}

#[test]
fn a_remove_s_reply_leaves_the_question_about_another_app() {
    let rig = Rig::start();
    let (page, cx) = manage_page(&rig, vec![app(HELPER), app(POLLS)]);
    cx.update(|_, cx| {
        page.update(cx, |page, cx| {
            page.ask_remove(app(HELPER), cx);
            page.remove(cx);
            page.keep(cx);
            page.ask_remove(app(POLLS), cx);
        });
    });
    cx.run_until_parked();
    assert!(!cx.update(|_, cx| page.read(cx).added(HELPER)), "the first app went");
    assert!(
        cx.update(|_, cx| page
            .read(cx)
            .removing()
            .is_some_and(|(store, failed)| store.app.id == POLLS && !failed)),
        "the question about the second app stays, with no problem"
    );
    *rig.core.team_written.lock().unwrap() = None;
    cx.update(|_, cx| {
        page.update(cx, |page, cx| {
            page.ask_remove(app(POLLS), cx);
            page.remove(cx);
            page.ask_remove(app(HELPER), cx);
        });
    });
    cx.run_until_parked();
    assert!(
        cx.update(|_, cx| page
            .read(cx)
            .removing()
            .is_some_and(|(store, failed)| store.app.id == HELPER && !failed)),
        "a failure of the other app's write does not show on this question"
    );
}

fn chat_apps(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Entity<ChatAppsDialog> {
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| shell.open_chat_apps(GROUP_CHAT.to_owned(), window, cx));
    });
    cx.run_until_parked();
    cx.update(|_, cx| shell.read(cx).team_dialog().cloned())
        .and_then(|dialog| dialog.downcast::<ChatAppsDialog>().ok())
        .expect("the chat's apps")
}

#[test]
fn a_chat_s_second_remove_before_the_reply_sends_no_second_write() {
    let rig = Rig::start();
    *rig.core.chat_apps.lock().unwrap() = vec![app(POLLS)];
    let (shell, cx) = shell_window(&rig, true);
    let dialog = chat_apps(&shell, cx);
    cx.update(|_, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.ask_remove(app(POLLS), cx);
            dialog.remove(cx);
            dialog.remove(cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(removes(&rig), 1, "{:?}", writes(&rig));
}

#[test]
fn a_chat_s_apps_stay_open_while_a_remove_runs() {
    let rig = Rig::start();
    *rig.core.chat_apps.lock().unwrap() = vec![app(POLLS)];
    let (shell, cx) = shell_window(&rig, true);
    let dialog = chat_apps(&shell, cx);
    cx.update(|_, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.ask_remove(app(POLLS), cx);
            dialog.remove(cx);
            dialog.close(cx);
            dialog.get_more(cx);
        });
    });
    cx.run_until_parked();
    assert!(
        cx.update(|_, cx| shell.read(cx).team_dialog().is_some()),
        "the chat's apps stay while Remove runs"
    );
    assert_ne!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Apps);
    assert!(bounds(cx, "chat-apps-none").is_some(), "the app went");
    click(cx, "chat-apps-close");
    assert!(cx.update(|_, cx| shell.read(cx).team_dialog().is_none()));
}

#[test]
fn a_chat_s_remove_reply_leaves_the_question_about_another_app() {
    let rig = Rig::start();
    *rig.core.chat_apps.lock().unwrap() = vec![app(POLLS), app(HELPER)];
    let (shell, cx) = shell_window(&rig, true);
    let dialog = chat_apps(&shell, cx);
    cx.update(|_, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.ask_remove(app(POLLS), cx);
            dialog.remove(cx);
            dialog.ask_remove(app(HELPER), cx);
        });
    });
    cx.run_until_parked();
    assert!(
        cx.update(|_, cx| dialog
            .read(cx)
            .removing()
            .is_some_and(|(store, failed)| store.app.id == HELPER && !failed)),
        "the question about the second app stays"
    );
    assert!(
        cx.update(|_, cx| dialog.read(cx).shown(cx).iter().all(|store| store.app.id != POLLS)),
        "the first app went"
    );
}

#[test]
fn a_chat_s_apps_are_removed_after_asking() {
    let rig = Rig::start();
    *rig.core.chat_apps.lock().unwrap() = vec![app(POLLS)];
    let (shell, cx) = shell_window(&rig, true);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| shell.open_chat_apps(GROUP_CHAT.to_owned(), window, cx));
    });
    assert!(bounds(cx, "chat-apps-0").is_some());
    click(cx, "chat-apps-0-remove");
    assert!(writes(&rig).is_empty(), "it asks first");
    click(cx, "chat-apps-confirm");
    assert!(
        matches!(writes(&rig).as_slice(), [TeamWrite::RemoveChatApp { chat, app }] if chat == GROUP_CHAT && app == POLLS),
        "{:?}",
        writes(&rig)
    );
    assert!(bounds(cx, "chat-apps-none").is_some());
    click(cx, "chat-apps-get-more");
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Apps);
}

#[test]
fn a_chat_s_apps_show_their_pictures() {
    let rig = Rig::start();
    *rig.core.chat_apps.lock().unwrap() = vec![app(POLLS)];
    let mut bytes = Vec::new();
    image::RgbaImage::from_pixel(48, 48, image::Rgba([60, 90, 200, 255]))
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    rig.core.app_pictures.lock().unwrap().insert(
        POLLS_TILE.to_owned(),
        Arc::new(teams_proto::PastedPicture {
            format: teams_proto::PastedFormat::Png,
            bytes,
        }),
    );
    let (shell, cx) = shell_window(&rig, true);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| shell.open_chat_apps(GROUP_CHAT.to_owned(), window, cx));
    });
    assert!(bounds(cx, "chat-apps-0").is_some());
    let dialog = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .and_then(|dialog| dialog.downcast::<ChatAppsDialog>().ok())
        .expect("the chat's apps");
    assert!(cx.update(|_, cx| dialog.read(cx).shows_picture(POLLS)));
}

#[test]
fn the_read_only_mode_offers_nothing_that_adds_or_removes() {
    let rig = Rig::start();
    *rig.core.read_only.lock().unwrap() = true;
    let (shell, cx) = shell_window(&rig, true);
    open_store(&shell, cx);
    click(cx, "apps-org-0");
    assert!(bounds(cx, "app-details").is_some());
    assert!(
        bounds(cx, "app-details-add").is_none(),
        "a personal app not added has no Add"
    );
    click(cx, "app-details-close");
    *rig.core.user_apps.lock().unwrap() = vec![app(HELPER)];
    cx.update(|_, cx| {
        shell
            .read(cx)
            .apps()
            .cloned()
            .expect("the page")
            .update(cx, AppsPage::refresh);
    });
    click(cx, "apps-tile-popular-1");
    assert!(bounds(cx, "app-details-add-chat").is_none());
    assert!(bounds(cx, "app-details-add-team").is_none());
    let dialog = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .and_then(|dialog| dialog.downcast::<AppDetailsDialog>().ok())
        .expect("the details");
    cx.update(|_, cx| dialog.update(cx, AppDetailsDialog::add));
    assert!(writes(&rig).is_empty());
    click(cx, "app-details-close");
    click(cx, "apps-manage");
    click(cx, "apps-mine-0-expand");
    assert!(bounds(cx, "apps-mine-0-places").is_some());
    assert!(bounds(cx, "apps-mine-0-remove").is_none());
}
