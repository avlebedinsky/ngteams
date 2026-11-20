//! The account menu's account parts: the account at the top (its picture, its name and what tells it apart), the
//! rows that change its picture ([`crate::account_picture`]), and "Sign out and remove" at the bottom.

use super::*;

/// The side of the account's picture at the top of the account menu, px.
const MENU_AVATAR: f32 = 40.0;

/// The parts of the account menu that show the account; each is `None` before the accounts were handed in.
pub(super) struct AccountMenuParts {
    pub header: Option<gpui::Div>,
    pub pictures: Option<gpui::Div>,
    pub remove: Option<gpui::Div>,
}

/// What the account menu shows of its account.
struct MenuAccount {
    name: SharedString,
    detail: SharedString,
    server: Option<SharedString>,
    face: Option<people::Picture>,
    chosen: bool,
    can_choose: bool,
    can_remove: bool,
}

impl Shell {
    /// The account menu's account parts (see the module docs). "Sign out and remove" shows only in an app that
    /// permits removal; the picture rows only where a picture can be kept.
    pub(super) fn account_menu_parts(&self, palette: Palette, cx: &mut Context<Self>) -> AccountMenuParts {
        let account = self.accounts.as_ref().and_then(|(accounts, _)| {
            let accounts = accounts.read(cx);
            let tile = accounts.tile(self.core.me())?;
            Some(MenuAccount {
                name: tile.name.clone(),
                detail: tile.detail.clone(),
                server: tile.server.clone(),
                face: tile.face().cloned(),
                chosen: tile.chosen_picture.is_some(),
                can_choose: accounts.can_choose_pictures(),
                can_remove: accounts.can_remove(),
            })
        });
        let Some(account) = account else {
            return AccountMenuParts {
                header: None,
                pictures: None,
                remove: None,
            };
        };
        AccountMenuParts {
            header: Some(menu_header(&account, palette)),
            pictures: account
                .can_choose
                .then(|| Self::picture_rows(account.chosen, palette, cx)),
            remove: account.can_remove.then(|| Self::remove_row(palette, cx)),
        }
    }

    /// "Change picture", and "Use profile picture" while the account shows a picture the user chose.
    fn picture_rows(chosen: bool, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let change = keyboard::stop(
            "status-menu-change-picture",
            menu_row("status-menu-change-picture", palette)
                .child(Glyph::IMAGE_PLUS.icon().size(px(16.0)))
                .child("Change picture")
                .on_click(cx.listener(|this, _, window, cx| {
                    this.status_focus.closed(window, cx);
                    this.close_status_menu(cx);
                    this.change_picture(cx);
                })),
            palette,
        );
        let profile = chosen.then(|| {
            keyboard::stop(
                "status-menu-profile-picture",
                menu_row("status-menu-profile-picture", palette)
                    .child(Glyph::USER.icon().size(px(16.0)))
                    .child("Use profile picture")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.status_focus.closed(window, cx);
                        this.close_status_menu(cx);
                        this.with_accounts(cx, Accounts::use_profile_picture);
                    })),
                palette,
            )
        });
        div()
            .w_full()
            .mt_1()
            .pt_1()
            .border_t_1()
            .border_color(rgb(palette.border))
            .child(change)
            .children(profile)
    }

    /// "Change picture" of this shell's account. The menu is closed then, so a picture that can't be used is said in a
    /// toast.
    fn change_picture(&mut self, cx: &mut Context<Self>) {
        let Some((accounts, _)) = &self.accounts else {
            return;
        };
        let key = self.core.me().to_owned();
        let changing = accounts.update(cx, |accounts, cx| accounts.change_picture(&key, cx));
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            if let Some(why) = changing.await {
                let _ = this.update(cx, |this, cx| this.show_toast_text(why, cx));
            }
        })
        .detach();
    }

    /// Runs `act` on the accounts with this shell's account.
    fn with_accounts(&self, cx: &mut Context<Self>, act: impl FnOnce(&mut Accounts, &str, &mut Context<Accounts>)) {
        if let Some((accounts, _)) = &self.accounts {
            let key = self.core.me().to_owned();
            accounts.update(cx, |accounts, cx| act(accounts, &key, cx));
        }
    }

    fn remove_row(palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .w_full()
            .mt_1()
            .pt_1()
            .border_t_1()
            .border_color(rgb(palette.border))
            .child(keyboard::stop(
                "status-menu-remove-account",
                menu_row("status-menu-remove-account", palette)
                    .text_color(rgb(theme::DANGER))
                    .child(Glyph::LOG_OUT.icon().size(px(16.0)))
                    .child("Sign out and remove")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.status_focus.closed(window, cx);
                        this.ask_remove_account(cx);
                    })),
                palette,
            ))
    }
}

/// The account at the top of the menu: its picture, its name, and what tells it apart (its own server too).
fn menu_header(account: &MenuAccount, palette: Palette) -> gpui::Div {
    let face = people::avatar(Some(&account.name), account.face.as_ref(), Size::Size(px(MENU_AVATAR)));
    div()
        .debug_selector(|| "status-menu-account".to_owned())
        .px_2()
        .pt_1()
        .pb_2()
        .mb_1()
        .flex()
        .items_center()
        .gap_3()
        .border_b_1()
        .border_color(rgb(palette.border))
        .child(
            div()
                .debug_selector(|| "status-menu-account-picture".to_owned())
                .flex_none()
                .child(face),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .font_weight(gpui::FontWeight::BOLD)
                        .truncate()
                        .child(account.name.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette.muted_foreground))
                        .child(div().truncate().child(account.detail.clone()))
                        .children(account.server.clone().map(|server| {
                            div()
                                .debug_selector(|| "status-menu-account-server".to_owned())
                                .child(server)
                        })),
                ),
        )
}
