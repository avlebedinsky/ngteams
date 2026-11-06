//! The call window's People pane ("Participants"), as Teams shows it: a meeting's Share invite (its join link
//! copied), who waits in its lobby (Admit all, and Admit or Deny each), and who is in the call (the account first),
//! each with whether they speak, their microphone muted and their hand raised.
use super::*;
use teams_core::LobbyPerson;

impl CallStage {
    /// The People pane.
    pub(super) fn roster_pane(&self, active: &ActiveCall, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let people = self.people.of(active.id);
        let mut rows = vec![self_row(
            active,
            self.own_name.clone(),
            self.hand_rank(self.core.me()),
            palette,
        )];
        if active.view.participants.is_empty() {
            rows.extend(people.iter().enumerate().map(|(index, (_, name))| {
                roster_row(format!("call-roster-{index}"), name.clone(), Signs::default(), palette)
            }));
        } else {
            rows.extend(active.view.participants.iter().enumerate().map(|(index, participant)| {
                let name = participant_name(participant, people);
                let signs = Signs {
                    speaking: participant.speaking,
                    muted: participant.muted,
                    hand: participant
                        .hand_raised
                        .then(|| self.hand_rank(&participant.mri).unwrap_or(0)),
                    organizer: participant.organizer && active.kind == CallKind::Meeting,
                };
                roster_row(format!("call-roster-{index}"), name, signs, palette)
            }));
        }
        let heading = if active.kind == CallKind::Meeting {
            format!("In this meeting ({})", rows.len())
        } else {
            format!("Currently in this call ({})", rows.len())
        };
        let close = selected_by(
            "call-roster-close",
            icon_button("call-roster-close", Glyph::X, "Close")
                .small()
                .on_click(cx.listener(|this, _, _, cx| this.show_roster(false, cx))),
        );
        let lobby = (!active.lobby.is_empty()).then(|| self.lobby_section(&active.lobby, palette, cx));
        side_pane("call-roster-pane", "Participants", close, palette)
            .children(self.invite(active, palette, cx))
            .child(
                div()
                    .id("call-roster-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(lobby)
                    .child(section_heading(heading, None, palette))
                    .children(rows),
            )
            .into_any_element()
    }

    /// A meeting's Share invite, once its join link is known: a menu whose Copy meeting link puts it on the clipboard.
    fn invite(&self, active: &ActiveCall, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let link = self.join_link(active)?;
        let copied = self.meeting.as_ref().is_some_and(|meeting| meeting.copied);
        let stage = cx.weak_entity();
        let button = selected_by(
            "call-share-invite",
            Button::new("call-share-invite")
                .outline()
                .small()
                .icon(Glyph::USER_PLUS.icon())
                .label("Share invite")
                .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu: PopupMenu, _, _| {
                    let (stage, link) = (stage.clone(), link.clone());
                    menu.min_w(px(220.0)).item(
                        PopupMenuItem::element(|_, _| {
                            div()
                                .debug_selector(|| "call-copy-link".to_owned())
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(Glyph::LINK.icon().size(px(14.0)))
                                .child("Copy meeting link")
                        })
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(link.clone()));
                            let _ = stage.update(cx, CallStage::link_copied);
                        }),
                    )
                }),
        );
        Some(
            div()
                .px_4()
                .pb_2()
                .flex()
                .items_center()
                .gap_2()
                .child(button)
                .when(copied, |row| {
                    row.child(
                        div()
                            .debug_selector(|| "call-link-copied".to_owned())
                            .text_xs()
                            .text_color(rgb(palette.muted_foreground))
                            .child("Link copied"),
                    )
                })
                .into_any_element(),
        )
    }

    /// Copy meeting link put the meeting's link on the clipboard: Share invite says so.
    fn link_copied(&mut self, cx: &mut Context<Self>) {
        if let Some(meeting) = self.meeting.as_mut() {
            meeting.copied = true;
        }
        cx.notify();
    }

    /// The meeting's join link, when this call is a meeting's and its thread named one.
    pub(super) fn join_link(&self, active: &ActiveCall) -> Option<String> {
        if active.kind != CallKind::Meeting {
            return None;
        }
        self.meeting
            .as_ref()
            .filter(|meeting| meeting.call == active.id)
            .and_then(|meeting| meeting.details.as_ref()?.join_url.clone())
    }

    /// "Waiting in the lobby": Admit all, then each person waiting with Deny and Admit.
    fn lobby_section(&self, waiting: &[LobbyPerson], palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let everyone: Vec<Mri> = waiting.iter().map(|person| person.mri.clone()).collect();
        let admit_all = (!self.core.read_only()).then(|| {
            selected_by(
                "call-lobby-admit-all",
                Button::new("call-lobby-admit-all")
                    .ghost()
                    .xsmall()
                    .label("Admit all")
                    .text_color(rgb(palette.brand))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        for waiting in &everyone {
                            this.ask(
                                Ask::Lobby {
                                    waiting: waiting.clone(),
                                    admit: true,
                                },
                                cx,
                            );
                        }
                    })),
            )
            .into_any_element()
        });
        let heading = format!("Waiting in lobby ({})", waiting.len());
        div()
            .debug_selector(|| "call-roster-lobby".to_owned())
            .pb_2()
            .child(section_heading(heading, admit_all, palette))
            .children(
                waiting
                    .iter()
                    .enumerate()
                    .map(|(index, person)| self.lobby_row(index, person, palette, cx)),
            )
            .into_any_element()
    }

    /// One person waiting in the lobby: their avatar and name, then Deny and Admit as Teams' icons.
    fn lobby_row(&self, index: usize, person: &LobbyPerson, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let name: SharedString = person.name.clone().unwrap_or_else(|| SOMEONE.to_owned()).into();
        let decide = |admit: bool, cx: &mut Context<Self>| {
            let (verb, glyph, tip) = if admit {
                ("admit", Glyph::CHECK, "Admit")
            } else {
                ("deny", Glyph::X, "Deny")
            };
            let id = format!("call-roster-{verb}-{index}");
            let waiting = person.mri.clone();
            selected_by(
                id.clone(),
                icon_button(SharedString::from(id), glyph, tip)
                    .small()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.ask(
                            Ask::Lobby {
                                waiting: waiting.clone(),
                                admit,
                            },
                            cx,
                        );
                    })),
            )
        };
        let buttons =
            (!self.core.read_only()).then(|| div().flex().gap_1().child(decide(false, cx)).child(decide(true, cx)));
        div()
            .debug_selector(move || format!("call-roster-waiting-{index}"))
            .h(px(44.0))
            .px_4()
            .flex()
            .items_center()
            .gap_3()
            .child(people::avatar(Some(&name), None, Size::Small))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(rgb(palette.foreground))
                    .child(name),
            )
            .children(buttons)
            .into_any_element()
    }
}

/// What a row of the People pane shows of its person: whether they speak or are muted, where their raised hand
/// stands in the line of hands up (from 1; 0 when unknown), and whether they organize the meeting.
#[derive(Clone, Copy, Default)]
struct Signs {
    speaking: bool,
    muted: bool,
    hand: Option<usize>,
    organizer: bool,
}

/// A side pane of the call window (People, Chat, Video effects, Meeting info): its title and Close over what it holds.
pub(super) fn side_pane(
    selector: &'static str,
    title: &'static str,
    close: impl IntoElement,
    palette: Palette,
) -> gpui::Div {
    div()
        .debug_selector(move || selector.to_owned())
        .w(px(PANE_WIDTH))
        .flex_none()
        .h_full()
        .flex()
        .flex_col()
        .bg(rgb(palette.surface))
        .border_l_1()
        .border_color(rgb(palette.border))
        .child(
            div()
                .h(px(48.0))
                .flex_none()
                .px_4()
                .flex()
                .items_center()
                .justify_between()
                .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
                .child(close),
        )
}

/// A section's heading in the People pane, with an action on its right.
fn section_heading(text: String, action: Option<AnyElement>, palette: Palette) -> gpui::Div {
    div()
        .h(px(32.0))
        .px_4()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgb(palette.muted_foreground))
                .child(SharedString::from(text)),
        )
        .children(action)
}

/// The signs on a row's right: a raised hand with its place in the line (Teams' "1 ✋" chip), the speaker while they
/// speak, the microphone (muted or not).
fn signs(signs: Signs, palette: Palette) -> gpui::Div {
    let Signs {
        speaking, muted, hand, ..
    } = signs;
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_2()
        .children(hand.map(|rank| {
            div()
                .debug_selector(move || format!("call-roster-hand-{rank}"))
                .h(px(22.0))
                .px_1p5()
                .flex()
                .items_center()
                .gap_1()
                .rounded_md()
                .bg(rgb(palette.hover))
                .text_xs()
                .when(rank > 0, |chip| chip.child(SharedString::from(rank.to_string())))
                .child(Glyph::HAND.icon().size(px(14.0)).text_color(rgb(gallery::HAND_COLOUR)))
        }))
        .child(if muted {
            Glyph::MIC_OFF
                .icon()
                .size(px(16.0))
                .text_color(rgb(palette.muted_foreground))
        } else if speaking {
            Glyph::VOLUME.icon().size(px(16.0)).text_color(rgb(palette.brand))
        } else {
            Glyph::MIC
                .icon()
                .size(px(16.0))
                .text_color(rgb(palette.muted_foreground))
        })
}

/// One row of the People pane: the person's initials (ringed in gold while their hand is up), their name with
/// "Organizer" under it for the meeting's, and their signs.
fn roster_row(selector: impl Into<String>, name: SharedString, shown: Signs, palette: Palette) -> AnyElement {
    let selector = selector.into();
    let avatar = people::avatar(Some(&name), None, Size::Small);
    div()
        .debug_selector(move || selector.clone())
        .h(px(48.0))
        .px_4()
        .flex()
        .items_center()
        .gap_3()
        .child(hand_ring(avatar, shown.hand.is_some()))
        .child(name_and_role(name, shown.organizer, palette))
        .child(signs(shown, palette))
        .into_any_element()
}

/// A row's avatar, ringed in Teams' raised-hand gold while `raised`.
fn hand_ring(avatar: impl IntoElement, raised: bool) -> gpui::Div {
    div()
        .flex_none()
        .p(px(2.0))
        .rounded_full()
        .border_2()
        .border_color(if raised {
            rgb(gallery::HAND_COLOUR)
        } else {
            gpui::transparent_black().into()
        })
        .child(avatar)
}

/// A row's name, with "Organizer" under it for the meeting's organizer, as Teams' People pane says it.
fn name_and_role(name: SharedString, organizer: bool, palette: Palette) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .child(div().truncate().child(name))
        .when(organizer, |names| {
            names.child(
                div()
                    .debug_selector(|| "call-roster-organizer".to_owned())
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .child("Organizer"),
            )
        })
}

/// The account's own row of the People pane, as Teams shows it: its name and initials (its mark, and "You", until
/// its profile names it), "Organizer" under it for the meeting's organizer, and its signs.
fn self_row(active: &ActiveCall, name: Option<SharedString>, rank: Option<usize>, palette: Palette) -> AnyElement {
    let shown = Signs {
        speaking: active.view.speaking && !active.muted,
        muted: active.muted,
        hand: active.hand_raised.then(|| rank.unwrap_or(0)),
        organizer: active.organizer && active.kind == CallKind::Meeting,
    };
    let (mark, name) = match name {
        Some(name) => (people::avatar(Some(&name), None, Size::Small).into_any_element(), name),
        None => (
            div()
                .size(px(24.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(rgb(palette.selected))
                .text_color(rgb(palette.brand))
                .child(Glyph::USER.icon().size(px(14.0)))
                .into_any_element(),
            SharedString::from("You"),
        ),
    };
    div()
        .debug_selector(|| "call-roster-self".to_owned())
        .h(px(48.0))
        .px_4()
        .flex()
        .items_center()
        .gap_3()
        .child(hand_ring(mark, shown.hand.is_some()))
        .child(name_and_role(name, shown.organizer, palette))
        .child(signs(shown, palette))
        .into_any_element()
}
