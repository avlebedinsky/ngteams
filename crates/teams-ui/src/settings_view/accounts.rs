//! The Accounts page: each account of the app with the picture that shows which account it is, "Change picture" and
//! "Use profile picture" ([`crate::account_picture`]), and "Add account" where the app adds accounts.
//!
//! Test selectors: `settings-account-<n>` (the row of the n-th account), `account-picture-change-<n>`,
//! `account-picture-profile-<n>`, `account-picture-problem-<n>` and `settings-add-account`.

use gpui_component::button::ButtonVariants as _;

use super::*;
use crate::accounts::{AccountTile, Accounts};
use crate::people::{self, Picture};

/// The side of an account's picture on the page, px.
const AVATAR: f32 = 40.0;
/// The narrowest an account's name gets before its buttons go under it, px.
const WORDS_MIN: f32 = 160.0;

/// What the page says under the list of accounts.
pub const PICTURE_NOTE: &str = "A picture you choose shows only in this app, so you can tell your accounts apart. \
                                Teams doesn't get it, and others still see your profile picture.";

/// One account's row, read before it is drawn.
struct AccountRow {
    index: usize,
    key: String,
    name: SharedString,
    detail: SharedString,
    server: Option<SharedString>,
    face: Option<Picture>,
    chosen: bool,
    problem: Option<SharedString>,
}

impl AccountRow {
    fn of(index: usize, tile: &AccountTile, accounts: &Accounts) -> Self {
        Self {
            index,
            key: tile.key.clone(),
            name: tile.name.clone(),
            detail: tile.detail.clone(),
            server: tile.server.clone(),
            face: tile.face().cloned(),
            chosen: tile.chosen_picture.is_some(),
            problem: accounts.picture_problem(&tile.key).cloned(),
        }
    }
}

impl SettingsView {
    /// The Accounts page's boxes: the accounts, then "Add account" where the app adds them.
    pub(super) fn accounts_page(&self, palette: Palette, cx: &App) -> Vec<AnyElement> {
        let Some(accounts) = self.app_accounts(cx) else {
            return Vec::new();
        };
        let read = accounts.read(cx);
        let rows: Vec<AccountRow> = read
            .tiles()
            .iter()
            .enumerate()
            .map(|(index, tile)| AccountRow::of(index, tile, read))
            .collect();
        let (can_choose, can_add) = (read.can_choose_pictures(), read.can_add());
        let rows = rows
            .into_iter()
            .map(|row| account_row(row, can_choose, &accounts, palette))
            .collect();
        let mut boxes = vec![group(Some("Your accounts"), rows, palette)];
        if can_choose {
            boxes.push(
                div()
                    .debug_selector(|| "account-picture-note".to_owned())
                    .px_1()
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child(PICTURE_NOTE)
                    .into_any_element(),
            );
        }
        if can_add {
            boxes.push(add_account(&accounts, palette));
        }
        boxes
    }
}

/// An account's row: its picture, name and what tells it apart, which picture it shows, and the picture buttons.
fn account_row(row: AccountRow, can_choose: bool, accounts: &Entity<Accounts>, palette: Palette) -> AnyElement {
    let index = row.index;
    let face = people::avatar(
        Some(&row.name),
        row.face.as_ref(),
        gpui_component::Size::Size(px(AVATAR)),
    );
    let shows = if row.chosen {
        "Shows a picture you chose"
    } else {
        "Shows the profile picture"
    };
    let words = div()
        .flex_1()
        .min_w(px(WORDS_MIN))
        .flex()
        .flex_col()
        .gap_0p5()
        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).truncate().child(row.name))
        .child(
            div()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .truncate()
                .child(row.detail),
        )
        .children(row.server.map(|server| {
            div()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .truncate()
                .child(server)
        }))
        .children(can_choose.then(|| {
            div()
                .debug_selector(move || format!("account-picture-shows-{index}"))
                .text_xs()
                .text_color(rgb(palette.muted_foreground))
                .child(shows)
        }));
    let buttons = can_choose.then(|| picture_buttons(index, &row.key, row.chosen, accounts));
    let problem = row.problem.map(|why| {
        div()
            .debug_selector(move || format!("account-picture-problem-{index}"))
            .px_4()
            .pb_3()
            .flex()
            .items_center()
            .gap_2()
            .text_sm()
            .text_color(rgb(theme::DANGER))
            .child(Glyph::CIRCLE_ALERT.icon().size(px(14.0)))
            .child(why)
    });
    div()
        .debug_selector(move || format!("settings-account-{index}"))
        .flex()
        .flex_col()
        .child(
            div()
                .min_h(px(64.0))
                .px_4()
                .py_2p5()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_3()
                .child(div().flex_none().child(face))
                .child(words)
                .children(buttons),
        )
        .children(problem)
        .into_any_element()
}

/// "Use profile picture" while the account shows a picture the user chose, and "Change picture".
fn picture_buttons(index: usize, key: &str, chosen: bool, accounts: &Entity<Accounts>) -> gpui::Div {
    let change = {
        let (accounts, key) = (accounts.clone(), key.to_owned());
        selected_by(
            format!("account-picture-change-{index}"),
            Button::new(("account-picture-change", index))
                .icon(Glyph::IMAGE_PLUS.icon())
                .label("Change picture")
                .outline()
                .small()
                .on_click(move |_, _, cx| accounts.update(cx, |accounts, cx| accounts.change_picture(&key, cx))),
        )
    };
    let profile = chosen.then(|| {
        let (accounts, key) = (accounts.clone(), key.to_owned());
        selected_by(
            format!("account-picture-profile-{index}"),
            Button::new(("account-picture-profile", index))
                .label("Use profile picture")
                .ghost()
                .small()
                .on_click(move |_, _, cx| {
                    accounts.update(cx, |accounts, cx| accounts.use_profile_picture(&key, cx).detach());
                }),
        )
    });
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .children(profile)
        .child(change)
}

/// "Add account": asks the app to add one, as the rail's "+" does.
fn add_account(accounts: &Entity<Accounts>, palette: Palette) -> AnyElement {
    let accounts = accounts.clone();
    group(
        None,
        vec![row(
            "Add another account",
            Some("Use more than one account in this window, and switch between them on the left."),
            selected_by(
                "settings-add-account",
                Button::new("settings-add-account")
                    .icon(Glyph::USER_PLUS.icon())
                    .label("Add account")
                    .outline()
                    .small()
                    .on_click(move |_, _, cx| accounts.update(cx, Accounts::ask_add)),
            ),
            palette,
        )],
        palette,
    )
}
