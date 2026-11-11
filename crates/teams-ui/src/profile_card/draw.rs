//! How a profile card is drawn, from the reference shots of browser Teams' card: the compact card 340 px wide with a
//! 72 px picture, the full one up to 840 px square with a 100 px picture and its tabs.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px, relative,
    rgb,
};
use gpui_component::Disableable as _;
use gpui_component::Size;
use gpui_component::input::{Escape, Input};
use gpui_component::tooltip::Tooltip;
use teams_core::CardPerson;

use super::words::{self, Length, Now};
use super::{
    CLOSE, CONTACT, CardOf, CardTab, EMAIL, Form, JOB_TITLE, MORE_CONTACT, NOT_LOADED, OFFICE, OUT_OF_OFFICE, PROFILE,
    ProfileCard, ProfileCardEvent, QUICK_FAILED, QUICK_HINT, QUICK_MAY_HAVE_ARRIVED, QUICK_SENT, QuickNote, REPORTS_TO,
    SHOW_MORE,
};
use crate::icons::{Glyph, icon_button, selected_by};
use crate::keyboard;
use crate::people;
use crate::theme::{self, Palette};

/// The compact card's width and most height, px, as Teams draws it.
pub const COMPACT_WIDTH: f32 = 340.0;
pub const COMPACT_MOST: f32 = 440.0;
/// The full card's most width and height, px.
pub const FULL_MOST: f32 = 840.0;
/// The pictures, px.
const COMPACT_FACE: f32 = 72.0;
const FULL_FACE: f32 = 100.0;
/// A face on the Organization tab, px.
const ORG_FACE: f32 = 48.0;
const ROW_FACE: f32 = 32.0;

/// One of the card's actions as it is drawn: its test selector, its icon, its tooltip and whether it is lit.
struct Act {
    key: &'static str,
    glyph: Glyph,
    tip: String,
    enabled: bool,
}

impl ProfileCard {
    /// The picture with the presence badge on its corner.
    fn face(&self, size: f32, palette: Palette, cx: &gpui::App) -> AnyElement {
        let name = self.name();
        let availability = self.models.presence.read(cx).of(&self.of.mri);
        let badge = crate::presence_badge::badge(&availability, (size / 4.0).max(14.0), palette)
            .map(|badge| badge.absolute().right(px(-2.0)).bottom(px(-2.0)));
        div()
            .debug_selector(|| "profile-card-face".to_owned())
            .relative()
            .flex_none()
            .size(px(size))
            .child(
                people::avatar(Some(&name), self.of.picture.as_ref(), Size::Large)
                    .size(px(size))
                    .text_size(px(size * 0.4)),
            )
            .children(badge)
            .into_any_element()
    }

    /// One of the actions: an icon with its tooltip, a keyboard stop; drawn off (`enabled` false) as Teams draws the
    /// own card's calls.
    fn action(
        &self,
        Act {
            key,
            glyph,
            tip,
            enabled,
        }: Act,
        palette: Palette,
        on: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tip = SharedString::from(tip);
        let label = tip.clone();
        let button = div()
            .id(key)
            .debug_selector(move || key.to_owned())
            .size(px(32.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .border_1()
            .border_color(gpui::transparent_black())
            .text_color(rgb(if enabled {
                palette.foreground
            } else {
                palette.muted_foreground
            }))
            .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
            .child(Glyph::icon(glyph).size(px(20.0)))
            .when(enabled, |button| {
                button
                    .cursor_pointer()
                    .hover(move |button| button.bg(rgb(palette.hover)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| on(this, window, cx)))
            })
            .when(!enabled, |button| button.opacity(0.45));
        let stop = keyboard::stop(key, button, palette).edge().tab_stop(enabled);
        let stop = if key == "profile-card-chat" {
            stop.handle(&self.chat_focus)
        } else {
            stop
        };
        stop.into_any_element()
    }

    /// The actions: Chat, the org chart, then the video and audio calls (none in the read-only mode; drawn off on the
    /// account's own card, as Teams draws them).
    fn actions(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let name = self.name();
        let calls = !self.core.read_only() && self.models.calls.read(cx).shows();
        let calling = self.writes();
        let mut row = div()
            .debug_selector(|| "profile-card-actions".to_owned())
            .flex()
            .items_center()
            .gap_1()
            .child(self.action(
                Act {
                    key: "profile-card-chat",
                    glyph: Glyph::CHAT,
                    tip: super::chat_tip(&name),
                    enabled: self.can_chat(cx),
                },
                palette,
                |this, _, cx| this.chat(cx),
                cx,
            ))
            .child(self.action(
                Act {
                    key: "profile-card-org",
                    glyph: Glyph::ORG_CHART,
                    tip: super::org_tip(&name),
                    enabled: true,
                },
                palette,
                Self::org_chart,
                cx,
            ));
        if calls {
            row = row
                .child(self.action(
                    Act {
                        key: "profile-card-video",
                        glyph: Glyph::VIDEO,
                        tip: super::video_tip(&name),
                        enabled: calling,
                    },
                    palette,
                    |this, _, cx| this.call(true, cx),
                    cx,
                ))
                .child(self.action(
                    Act {
                        key: "profile-card-audio",
                        glyph: Glyph::PHONE,
                        tip: super::audio_tip(&name),
                        enabled: calling,
                    },
                    palette,
                    |this, _, cx| this.call(false, cx),
                    cx,
                ));
        }
        row.into_any_element()
    }

    /// The name, semibold, its job title under it when known.
    fn title(&self, size: f32, palette: Palette) -> AnyElement {
        let job = self.card().and_then(|card| card.job_title.clone());
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .debug_selector(|| "profile-card-name".to_owned())
                    .text_size(px(size))
                    .font_weight(FontWeight::SEMIBOLD)
                    .truncate()
                    .child(self.name()),
            )
            .children(job.map(|job| {
                div()
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .truncate()
                    .child(job)
            }))
            .into_any_element()
    }

    /// The quick message box and Send, with what happened to the last one under it; none on the account's own card
    /// or in the read-only mode.
    fn quick_message(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.writes() {
            return None;
        }
        let empty = self.quick.read(cx).value().trim().is_empty();
        let send = selected_by(
            "profile-card-send",
            icon_button("profile-card-send", Glyph::SEND, "Send")
                .disabled(empty || self.is_sending())
                .on_click(cx.listener(|this, _, window, cx| this.send_quick(window, cx))),
        );
        let note = self.note.as_ref().map(|note| {
            let (text, color) = match note {
                QuickNote::Sent => (QUICK_SENT, palette.muted_foreground),
                QuickNote::Failed => (QUICK_FAILED, theme::DANGER),
                QuickNote::MayHaveArrived => (QUICK_MAY_HAVE_ARRIVED, theme::DANGER),
            };
            div()
                .debug_selector(|| "profile-card-quick-note".to_owned())
                .text_xs()
                .text_color(rgb(color))
                .child(text)
        });
        let field = div()
            .debug_selector(|| "profile-card-quick".to_owned())
            .flex_1()
            .min_w_0()
            .capture_action(cx.listener(|this, _: &Escape, _, cx| {
                this.escape(cx);
                cx.stop_propagation();
            }))
            .child(Input::new(&self.quick).aria_label(QUICK_HINT));
        Some(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().flex().items_center().gap_2().child(field).child(send))
                .children(note)
                .into_any_element(),
        )
    }

    /// The availability box: their presence and calendar, their working hours, their clock (see [`words`]); none
    /// while nothing of it is known.
    fn availability(&self, length: Length, palette: Palette, cx: &gpui::App) -> Option<AnyElement> {
        let card = self.card();
        let live = self.models.presence.read(cx).of(&self.of.mri);
        let now = now(cx);
        let clock = now.clock;
        let schedule = card.and_then(|card| card.schedule.as_ref());
        let presence = words::presence(card.and_then(|card| card.presence.as_ref()), &live, now.ms, length);
        let said = words::availability_line(presence, schedule.and_then(|schedule| words::calendar(schedule, &now)));
        let hours = schedule.and_then(|schedule| words::work_hours(schedule, clock));
        let their_zone = schedule.and_then(|schedule| schedule.utc_offset_minutes);
        let clock_line = words::clock_line(their_zone, self.own(), &now);
        if said.is_none() && hours.is_none() && clock_line.is_none() {
            return None;
        }
        let mark = crate::presence_badge::badge(&live, 16.0, palette).map_or_else(
            || Glyph::CALENDAR.icon().size(px(16.0)).into_any_element(),
            IntoElement::into_any_element,
        );
        let top = (said.is_some() || hours.is_some()).then(|| {
            div()
                .py_2()
                .flex()
                .items_start()
                .gap_3()
                .child(div().pt_0p5().child(mark))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .children(said.map(|said| {
                            div()
                                .debug_selector(|| "profile-card-availability-line".to_owned())
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(said)
                        }))
                        .children(hours.map(|hours| div().text_xs().child(hours))),
                )
        });
        let rule = top.is_some() && clock_line.is_some();
        let bottom = clock_line.map(|clock_line| {
            div()
                .debug_selector(|| "profile-card-clock".to_owned())
                .py_2()
                .flex()
                .items_center()
                .gap_3()
                .text_xs()
                .child(Glyph::CLOCK.icon().size(px(16.0)))
                .child(clock_line)
        });
        Some(
            div()
                .debug_selector(|| "profile-card-availability".to_owned())
                .px_3()
                .flex()
                .flex_col()
                .rounded_md()
                .border_1()
                .border_color(rgb(palette.border))
                .children(top)
                .when(rule, |availability| availability.child(hairline(palette)))
                .children(bottom)
                .into_any_element(),
        )
    }

    /// "Out of office", when they are, with until when their calendar says.
    fn out_of_office(&self, palette: Palette, cx: &gpui::App) -> Option<AnyElement> {
        let card = self.card().filter(|card| card.out_of_office == Some(true))?;
        let until = card
            .schedule
            .as_ref()
            .filter(|schedule| schedule.current == Some(teams_proto::FreeBusyStatus::OutOfOffice))
            .and_then(|schedule| schedule.until_ms)
            .map(|until| format!("{OUT_OF_OFFICE} until {}", now(cx).when(until)));
        Some(
            div()
                .debug_selector(|| "profile-card-out-of-office".to_owned())
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(palette.brand))
                        .child(OUT_OF_OFFICE),
                )
                .children(until.map(|until| div().text_xs().child(until)))
                .into_any_element(),
        )
    }

    /// The quiet line of a card that couldn't be read.
    fn failed(&self, palette: Palette) -> Option<AnyElement> {
        matches!(self.loaded, super::Loaded::Failed).then(|| {
            div()
                .debug_selector(|| "profile-card-failed".to_owned())
                .text_xs()
                .text_color(rgb(palette.muted_foreground))
                .child(NOT_LOADED)
                .into_any_element()
        })
    }

    /// Their mail addresses.
    fn emails(&self) -> Vec<String> {
        self.card().map(|card| card.emails.clone()).unwrap_or_default()
    }

    // --- the compact card ---

    fn compact(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let header = div()
            .px_6()
            .pt_6()
            .pb_3()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(self.face(COMPACT_FACE, palette, cx))
                    .child(self.title(14.0, palette)),
            )
            .child(self.actions(palette, cx));
        let body = div()
            .id("profile-card-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_4()
            .py_3()
            .flex()
            .flex_col()
            .gap_3()
            .children(self.quick_message(palette, cx))
            .children(self.availability(Length::Short, palette, cx))
            .child(self.compact_profile(palette, cx))
            .children(self.out_of_office(palette, cx))
            .children(self.failed(palette));
        div()
            .w(px(COMPACT_WIDTH))
            .max_h(px(COMPACT_MOST))
            .flex()
            .flex_col()
            .bg(rgb(palette.surface))
            .border_1()
            .border_color(rgb(palette.border))
            .rounded_lg()
            .shadow_lg()
            .child(header)
            .child(hairline(palette))
            .child(body)
            .into_any_element()
    }

    /// "Profile >", the email, "Show more": the full card's Profile tab from either link.
    fn compact_profile(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let heading = link(
            "profile-card-profile",
            palette,
            div()
                .flex()
                .items_center()
                .gap_1()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(palette.foreground))
                .child(PROFILE)
                .child(Glyph::CHEVRON_RIGHT.icon().size(px(14.0))),
        )
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.show_tab(CardTab::Profile, window, cx)));
        let emails: Vec<AnyElement> = self
            .emails()
            .into_iter()
            .enumerate()
            .map(|(at, address)| email_row(at, address, palette))
            .collect();
        let more = link(
            "profile-card-show-more",
            palette,
            div().text_xs().text_color(rgb(palette.brand)).child(SHOW_MORE),
        )
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.show_tab(CardTab::Profile, window, cx)));
        div()
            .px_2()
            .flex()
            .flex_col()
            .items_start()
            .gap_2()
            .child(heading)
            .children(emails)
            .child(more)
            .into_any_element()
    }

    // --- the full card ---

    fn full(&self, palette: Palette, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let viewport = window.viewport_size();
        let width = (f32::from(viewport.width) - 32.0).clamp(320.0, FULL_MOST);
        let height = (f32::from(viewport.height) - 32.0).clamp(320.0, FULL_MOST);
        let body = match self.tab {
            CardTab::Overview => self.overview(palette, cx),
            CardTab::Profile => self.profile(palette, cx),
            CardTab::Organization => self.organization(palette, cx),
        };
        div()
            .w(px(width))
            .h(px(height))
            .flex()
            .flex_col()
            .bg(rgb(palette.surface))
            .border_1()
            .border_color(rgb(palette.border))
            .rounded_lg()
            .shadow_lg()
            .child(self.top_bar(palette, cx))
            .child(
                div()
                    .px_6()
                    .flex()
                    .items_center()
                    .gap_5()
                    .child(self.face(FULL_FACE, palette, cx))
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(self.title(24.0, palette))
                            .child(self.actions(palette, cx)),
                    ),
            )
            .child(self.tab_bar(palette, cx))
            .child(
                div()
                    .id("profile-card-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_8()
                    .py_4()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(body),
            )
            .into_any_element()
    }

    /// Back (when a card came before this one) on the left, Close on the right.
    fn top_bar(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let back = self.back.last().map(|before| {
            let name = before
                .name
                .clone()
                .unwrap_or_else(|| SharedString::from("the card before"));
            selected_by(
                "profile-card-back",
                icon_button("profile-card-back", Glyph::ARROW_LEFT, super::back_tip(&name))
                    .on_click(cx.listener(|this, _, window, cx| this.go_back(window, cx))),
            )
        });
        let close = selected_by(
            "profile-card-close",
            icon_button("profile-card-close", Glyph::X, CLOSE)
                .on_click(cx.listener(|_, _, _, cx| cx.emit(ProfileCardEvent::Close))),
        );
        div()
            .h(px(48.0))
            .flex_none()
            .px_3()
            .pt_2()
            .flex()
            .items_center()
            .text_color(rgb(palette.foreground))
            .children(back)
            .child(div().flex_1())
            .child(close)
            .into_any_element()
    }

    /// The tabs, the chosen one underlined; Left, Right, Up, Down, Home and End move among them.
    fn tab_bar(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let tabs = CardTab::ALL.map(|tab| {
            let key = format!("profile-card-tab-{}", tab.key());
            let chosen = self.tab == tab;
            keyboard::stop(
                key.clone(),
                crate::teams_page::tab_button(key, tab.label(), chosen, palette, cx)
                    .text_sm()
                    .text_color(rgb(if chosen { palette.brand } else { palette.foreground }))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.show_tab(tab, window, cx)))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                        this.tab_key(tab, event, window, cx);
                    })),
                palette,
            )
            .handle(&self.tabs.get(tab.key(), cx))
            .tab_stop(chosen)
        });
        div()
            .mt_4()
            .h(px(44.0))
            .flex_none()
            .px_6()
            .flex()
            .items_stretch()
            .gap_4()
            .border_b_1()
            .border_color(rgb(palette.border))
            .children(tabs)
            .into_any_element()
    }

    fn overview(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let more = link(
            "profile-card-more-contact",
            palette,
            div().text_sm().text_color(rgb(palette.brand)).child(MORE_CONTACT),
        )
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.show_tab(CardTab::Profile, window, cx)));
        div()
            .flex()
            .flex_col()
            .items_stretch()
            .gap_4()
            .children(self.quick_message(palette, cx))
            .children(self.availability(Length::Long, palette, cx))
            .children(self.out_of_office(palette, cx))
            .child(self.contact(false, palette, cx))
            .child(div().flex().child(more))
            .children(self.failed(palette))
            .into_any_element()
    }

    fn profile(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .items_stretch()
            .gap_4()
            .children(self.quick_message(palette, cx))
            .children(self.availability(Length::Long, palette, cx))
            .children(self.out_of_office(palette, cx))
            .child(self.contact(true, palette, cx))
            .children(self.failed(palette))
            .into_any_element()
    }

    /// "Contact information": the email and the chat address in two columns; with `all`, every address and the work
    /// fields known too.
    fn contact(&self, all: bool, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let mut fields: Vec<AnyElement> = Vec::new();
        let emails = self.emails();
        let shown = if all { emails.len() } else { emails.len().min(1) };
        for (at, address) in emails.into_iter().take(shown).enumerate() {
            let mail = address.clone();
            let value = link(
                format!("profile-card-email-{at}"),
                palette,
                div().text_sm().text_color(rgb(palette.brand)).child(address),
            )
            .on_click(move |_, _, cx| ProfileCard::email(&mail, cx));
            fields.push(field(Glyph::MAIL, EMAIL, value.into_any_element(), palette));
        }
        let card = self.card();
        let chat_address = card.and_then(|card| card.im_addresses.first().or(card.user_principal_name.as_ref()));
        if let Some(address) = chat_address.cloned() {
            let value = link(
                "profile-card-im",
                palette,
                div().text_sm().text_color(rgb(palette.brand)).child(address),
            )
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.chat(cx)));
            fields.push(field(Glyph::CHAT, super::CHAT, value.into_any_element(), palette));
        }
        let plain = |text: &String| div().text_sm().child(text.clone()).into_any_element();
        if let Some(job) = card.and_then(|card| card.job_title.as_ref()) {
            fields.push(field(Glyph::BRIEFCASE, JOB_TITLE, plain(job), palette));
        }
        if let Some(office) = card.and_then(|card| card.office.as_ref()) {
            fields.push(field(Glyph::MAP_PIN, OFFICE, plain(office), palette));
        }
        div()
            .debug_selector(|| "profile-card-contact".to_owned())
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(CONTACT))
            .child(div().flex().flex_wrap().gap_y_3().children(fields))
            .into_any_element()
    }

    /// The Organization tab: the managers, the person, their reports (or the note that the directory names none), then
    /// the people they work with.
    fn organization(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(card) = self.card() else {
            return div().children(self.failed(palette)).into_any_element();
        };
        let name = self.name();
        let organization = card.organization.clone().unwrap_or_default();
        let chart = if organization.managers.is_empty() && organization.directs.is_empty() {
            note_box(&super::no_org(&name), palette)
        } else {
            self.chart(&organization, palette, cx)
        };
        let working = card.working_with.clone().unwrap_or_default();
        let tiles: Vec<AnyElement> = working
            .iter()
            .enumerate()
            .map(|(at, person)| self.tile(at, person, palette, cx))
            .collect();
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(chart)
            .when(!tiles.is_empty(), |tab| {
                tab.child(divider_label(&super::works_with(&name), palette))
                    .child(div().flex().flex_wrap().gap_3().children(tiles))
            })
            .into_any_element()
    }

    /// The managers ("Reports to"), the person, and "People reporting to {name} ({n})".
    fn chart(
        &self,
        organization: &teams_core::CardOrganization,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let managers: Vec<AnyElement> = organization
            .managers
            .iter()
            .enumerate()
            .map(|(at, person)| self.person_row(format!("profile-card-manager-{at}"), person, palette, cx))
            .collect();
        let directs: Vec<AnyElement> = organization
            .directs
            .iter()
            .enumerate()
            .map(|(at, person)| self.person_row(format!("profile-card-direct-{at}"), person, palette, cx))
            .collect();
        let name = self.name();
        let heading = |text: String| {
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(text)
                .into_any_element()
        };
        let own = div()
            .debug_selector(|| "profile-card-chart-self".to_owned())
            .px_3()
            .py_2()
            .rounded_md()
            .bg(rgb(palette.selected))
            .text_sm()
            .font_weight(FontWeight::SEMIBOLD)
            .child(name.clone());
        div()
            .debug_selector(|| "profile-card-chart".to_owned())
            .flex()
            .flex_col()
            .gap_2()
            .when(!managers.is_empty(), |chart| {
                chart.child(heading(REPORTS_TO.to_owned())).children(managers)
            })
            .child(own)
            .when(!directs.is_empty(), |chart| {
                chart
                    .child(heading(super::reporting(&name, organization.directs.len())))
                    .children(directs)
            })
            .into_any_element()
    }

    /// A person as the Organization tab knows them: their name and picture as the engine gives them.
    fn card_of(&self, person: &CardPerson) -> CardOf {
        let shown = self.faces.get(&person.mri);
        let name = shown
            .and_then(|shown| shown.name.clone())
            .or_else(|| person.name.clone().map(SharedString::from));
        CardOf::new(person.mri.clone(), name).with_picture(shown.and_then(|shown| shown.picture.clone()))
    }

    /// A small face with its presence.
    fn small_face(&self, of: &CardOf, size: f32, palette: Palette, cx: &gpui::App) -> AnyElement {
        let availability = self.models.presence.read(cx).of(&of.mri);
        let badge = crate::presence_badge::badge(&availability, 14.0, palette)
            .map(|badge| badge.absolute().right(px(-2.0)).bottom(px(-2.0)));
        div()
            .relative()
            .flex_none()
            .size(px(size))
            .child(people::avatar(of.name.as_ref(), of.picture.as_ref(), Size::Medium).size(px(size)))
            .children(badge)
            .into_any_element()
    }

    /// A manager or a report: their face, name and address; a click opens their card here.
    fn person_row(&self, key: String, person: &CardPerson, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let of = self.card_of(person);
        let name = of
            .name
            .clone()
            .unwrap_or_else(|| SharedString::from(crate::chat_list::unnamed(&of.mri)));
        let face = self.small_face(&of, ROW_FACE, palette, cx);
        let row = div()
            .id(SharedString::from(key.clone()))
            .debug_selector(move || key.clone())
            .px_3()
            .py_2()
            .flex()
            .items_center()
            .gap_3()
            .rounded_md()
            .cursor_pointer()
            .hover(move |row| row.bg(rgb(palette.hover)))
            .child(face)
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().text_sm().truncate().child(name))
                    .children(person.email.clone().map(|email| {
                        div()
                            .text_xs()
                            .text_color(rgb(palette.muted_foreground))
                            .truncate()
                            .child(email)
                    })),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.open_person(of.clone(), window, cx);
            }));
        row.into_any_element()
    }

    /// One of the people they work with: a tile of their face and name; a click opens their card here.
    fn tile(&self, at: usize, person: &CardPerson, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let of = self.card_of(person);
        let name = of
            .name
            .clone()
            .unwrap_or_else(|| SharedString::from(crate::chat_list::unnamed(&of.mri)));
        let face = self.small_face(&of, ORG_FACE, palette, cx);
        let tile = div()
            .id(("profile-card-person", at))
            .debug_selector(move || format!("profile-card-person-{at}"))
            .w(px(134.0))
            .h(px(140.0))
            .p_3()
            .flex()
            .flex_col()
            .items_center()
            .gap_2()
            .rounded_md()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.background))
            .cursor_pointer()
            .hover(move |tile| tile.bg(rgb(palette.hover)))
            .child(face)
            .child(
                div()
                    .w_full()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_center()
                    .line_clamp(2)
                    .child(name),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.open_person(of.clone(), window, cx);
            }));
        keyboard::stop(("profile-card-person-stop", at), tile, palette)
            .edge()
            .into_any_element()
    }
}

/// The time now, on the viewer's clock.
fn now(cx: &gpui::App) -> Now<'static> {
    Now {
        ms: words::now_ms(),
        viewer_offset: words::local_offset(),
        clock: crate::clock::cycle(cx).time_of_day(),
    }
}

/// A hairline across a box.
fn hairline(palette: Palette) -> gpui::Div {
    div().h(px(1.0)).w_full().flex_none().bg(rgb(palette.border))
}

/// A link: `face`, clickable, its test selector `key`.
fn link(key: impl Into<String>, palette: Palette, face: gpui::Div) -> gpui::Stateful<gpui::Div> {
    let key = key.into();
    let selector = key.clone();
    div()
        .id(SharedString::from(key))
        .debug_selector(move || selector.clone())
        .cursor_pointer()
        .rounded_sm()
        .hover(move |link| link.bg(rgb(palette.hover)))
        .child(face)
}

/// The compact card's email row: the mail icon and the address, opened in the mail app.
fn email_row(at: usize, address: String, palette: Palette) -> AnyElement {
    let mail = address.clone();
    link(
        format!("profile-card-email-{at}"),
        palette,
        div()
            .flex()
            .items_center()
            .gap_4()
            .text_xs()
            .child(Glyph::MAIL.icon().size(px(16.0)).text_color(rgb(palette.foreground)))
            .child(div().text_color(rgb(palette.brand)).child(address)),
    )
    .on_click(move |_, _, cx| ProfileCard::email(&mail, cx))
    .into_any_element()
}

/// A contact field: its icon, its label over its value; half the row.
fn field(glyph: Glyph, label: &'static str, value: AnyElement, palette: Palette) -> AnyElement {
    div()
        .w(relative(0.5))
        .pr_4()
        .flex()
        .items_center()
        .gap_3()
        .child(glyph.icon().size(px(18.0)))
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().text_xs().text_color(rgb(palette.muted_foreground)).child(label))
                .child(value),
        )
        .into_any_element()
}

/// The Organization tab's note: an alert icon and `text` in a grey box.
fn note_box(text: &str, palette: Palette) -> AnyElement {
    div()
        .debug_selector(|| "profile-card-no-org".to_owned())
        .px_4()
        .py_3()
        .flex()
        .items_center()
        .gap_3()
        .rounded_md()
        .border_1()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.hover))
        .text_sm()
        .child(Glyph::CIRCLE_ALERT.icon().flex_none().size(px(18.0)))
        .child(div().flex_1().min_w_0().child(SharedString::from(text.to_owned())))
        .into_any_element()
}

/// A label across a rule, as Teams heads the people someone works with.
fn divider_label(text: &str, palette: Palette) -> AnyElement {
    let rule = || div().flex_1().h(px(1.0)).bg(rgb(palette.border));
    div()
        .debug_selector(|| "profile-card-works-with".to_owned())
        .flex()
        .items_center()
        .gap_3()
        .text_sm()
        .child(rule())
        .child(SharedString::from(text.to_owned()))
        .child(rule())
        .into_any_element()
}

impl Render for ProfileCard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let card = match self.form {
            Form::Compact => self.compact(palette, cx),
            Form::Full => self.full(palette, window, cx),
        };
        div()
            .id("profile-card")
            .debug_selector(|| "profile-card".to_owned())
            .track_focus(&self.focus)
            .key_context("ProfileCard")
            .occlude()
            .text_color(rgb(palette.foreground))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.escape(cx);
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down_out(cx.listener(|_, _: &gpui::MouseDownEvent, _, cx| cx.emit(ProfileCardEvent::Close)))
            // a press anywhere on it (its quick message box too) keeps a card opened by a rest open
            .capture_any_mouse_down(cx.listener(|_, _: &gpui::MouseDownEvent, _, cx| {
                cx.emit(ProfileCardEvent::Engaged);
            }))
            .on_hover(cx.listener(|_, hovered: &bool, _, cx| cx.emit(ProfileCardEvent::Hovered(*hovered))))
            .child(card)
    }
}
