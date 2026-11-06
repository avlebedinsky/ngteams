//! The calendar's left pane (its month, Add calendar, My calendars, Show all), its toolbar, the month picker under the
//! toolbar's dates and the notice of a control that can't act yet.

use chrono::{Datelike as _, Duration, NaiveDate};
use gpui::{
    AnyElement, App, ClickEvent, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};

use super::{ADD_CALENDAR_UNAVAILABLE, CalendarView, PANE_WIDTH, Toggle, separator};
use crate::calendar::{self, Mode};
use crate::icons::{Glyph, icon_button, selected_by};
use crate::theme::Palette;

impl CalendarView {
    /// The left pane: the title, the month, Add calendar, My calendars and Show all.
    pub(super) fn left_pane(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .debug_selector(|| "calendar-pane".to_owned())
            .w(px(PANE_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(rgb(palette.surface))
            .border_r_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .h(px(56.0))
                    .px_5()
                    .flex()
                    .items_center()
                    .text_xl()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("Calendar"),
            )
            .child(self.mini_month_view(palette, cx))
            .child(self.fold_line(palette, cx))
            .child(self.calendars_list(palette, cx))
    }

    /// The left pane's month: its header (fold, month, arrows), the weekday letters and the weeks.
    pub(super) fn mini_month_view(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let month = self.mini_month;
        let folded = self.toggles.on(Toggle::MiniFolded);
        let header = div()
            .flex()
            .items_center()
            .gap_1()
            .child(selected_by(
                "calendar-mini-fold",
                icon_button(
                    "calendar-mini-fold",
                    if folded {
                        Glyph::CHEVRON_RIGHT
                    } else {
                        Glyph::CHEVRON_DOWN
                    },
                    if folded {
                        "Show the month"
                    } else {
                        "Show this week only"
                    },
                )
                .small()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.toggles.flip(Toggle::MiniFolded);
                    cx.notify();
                })),
            ))
            .child(
                div()
                    .flex_1()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(month.format("%B %Y").to_string()),
            )
            .child(selected_by(
                "calendar-mini-previous",
                icon_button("calendar-mini-previous", Glyph::ARROW_UP, "Previous month")
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.mini_month = calendar::add_months(this.mini_month, -1);
                        cx.notify();
                    })),
            ))
            .child(selected_by(
                "calendar-mini-next",
                icon_button("calendar-mini-next", Glyph::ARROW_DOWN, "Next month")
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.mini_month = calendar::add_months(this.mini_month, 1);
                        cx.notify();
                    })),
            ));
        div()
            .px_4()
            .pt_2()
            .flex()
            .flex_col()
            .gap_1()
            .child(header)
            .child(self.month_grid(month, false, palette, cx))
    }

    /// A small month (the left pane's, or the toolbar's `picker`): the weekday letters and its weeks, the days shown
    /// marked, today circled; the pane's folded shows only the anchor's week. A day clicked shows its days (and closes
    /// the picker).
    pub(super) fn month_grid(
        &self,
        month: NaiveDate,
        picker: bool,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let id = if picker { "calendar-picker" } else { "calendar-mini" };
        let folded = !picker && self.toggles.on(Toggle::MiniFolded);
        let today = calendar::now().date_naive();
        let span = self.span();
        let first = calendar::monday(month);
        let letters = div().flex().children(calendar::WEEKDAY_LETTERS.iter().map(|letter| {
            div()
                .flex_1()
                .h(px(28.0))
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .child(*letter)
        }));
        let weeks = (0..6i64)
            .map(|week| first + Duration::days(7 * week))
            .filter(|monday| !folded || calendar::monday(self.anchor) == *monday)
            .map(|monday| {
                let marked = self.mode != Mode::Month && span.contains(monday) && span.days >= 5;
                div()
                    .flex()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(if marked { palette.brand } else { palette.surface }))
                    .when(marked, |row| row.bg(rgb(palette.selected)))
                    .children((0..7).map(|day| {
                        let date = monday + Duration::days(day);
                        self.mini_day(id, date, month, today, picker, palette, cx)
                    }))
            });
        div().flex().flex_col().gap_0p5().child(letters).children(weeks)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one day cell of a small month, drawn from the month's state"
    )]
    pub(super) fn mini_day(
        &self,
        id: &'static str,
        date: NaiveDate,
        month: NaiveDate,
        today: NaiveDate,
        picker: bool,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let in_month = date.month() == month.month();
        let is_today = date == today;
        let chosen = self.mode == Mode::Day && date == self.anchor;
        let selector = format!("{id}-{}", date.format("%Y-%m-%d"));
        let face = div()
            .size(px(26.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .text_sm()
            .text_color(rgb(if is_today {
                palette.on_brand
            } else if in_month {
                palette.foreground
            } else {
                palette.muted_foreground
            }))
            .when(is_today, |face| {
                face.bg(rgb(palette.brand)).font_weight(gpui::FontWeight::BOLD)
            })
            .when(chosen && !is_today, |face| face.bg(rgb(palette.selected)))
            .child(date.day().to_string());
        div()
            .debug_selector(move || selector.clone())
            .id(SharedString::from(format!(
                "{id}-{}",
                date.ordinal() + 1000 * date.year().unsigned_abs()
            )))
            .flex_1()
            .h(px(32.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .rounded_md()
            .hover(move |cell| cell.bg(rgb(palette.hover)))
            .on_click(cx.listener(move |this, _, _, cx| {
                if picker {
                    this.picker = None;
                }
                this.go_to(date, cx);
            }))
            .child(face)
            .into_any_element()
    }

    /// The line under the month, its chevron folding the month to the week.
    pub(super) fn fold_line(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let line = || div().flex_1().h(px(1.0)).bg(rgb(palette.border));
        div()
            .px_4()
            .py_1()
            .flex()
            .items_center()
            .gap_2()
            .child(line())
            .child(selected_by(
                "calendar-fold",
                icon_button(
                    "calendar-fold",
                    if self.toggles.on(Toggle::MiniFolded) {
                        Glyph::CHEVRON_DOWN
                    } else {
                        Glyph::CHEVRON_UP
                    },
                    if self.toggles.on(Toggle::MiniFolded) {
                        "Show the month"
                    } else {
                        "Show this week only"
                    },
                )
                .xsmall()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.toggles.flip(Toggle::MiniFolded);
                    cx.notify();
                })),
            ))
            .child(line())
    }

    /// Add calendar, My calendars with the account's calendar, Show all.
    pub(super) fn calendars_list(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let ticked = !self.toggles.on(Toggle::Hidden);
        let tick = move |selector: &'static str| calendar_tick(selector, ticked, palette);
        let toggle_tick = cx.listener(|this: &mut Self, _: &ClickEvent, _, cx| {
            this.toggles.flip(Toggle::Hidden);
            cx.notify();
        });
        let toggle_tick_too = cx.listener(|this: &mut Self, _: &ClickEvent, _, cx| {
            this.toggles.flip(Toggle::Hidden);
            cx.notify();
        });
        let row = |id: &'static str| calendar_row(id, palette);
        let add = row("calendar-add")
            .text_color(rgb(palette.brand))
            .child(Glyph::CALENDAR_ADD.icon().size(px(18.0)))
            .child("Add calendar")
            .on_click(cx.listener(|this, _, _, cx| this.say(ADD_CALENDAR_UNAVAILABLE, cx)));
        let open = !self.toggles.on(Toggle::MineFolded);
        let group = div()
            .flex()
            .items_center()
            .gap_1()
            .child(selected_by(
                "calendar-mine-fold",
                icon_button(
                    "calendar-mine-fold",
                    if open {
                        Glyph::CHEVRON_DOWN
                    } else {
                        Glyph::CHEVRON_RIGHT
                    },
                    if open { "Hide my calendars" } else { "Show my calendars" },
                )
                .small()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.toggles.flip(Toggle::MineFolded);
                    cx.notify();
                })),
            ))
            .child(
                row("calendar-mine")
                    .flex_1()
                    .px_1()
                    .child(tick("calendar-mine-tick"))
                    .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("My calendars"))
                    .on_click(toggle_tick),
            );
        let own = open.then(|| {
            row("calendar-own")
                .ml(px(30.0))
                .child(tick("calendar-own-tick"))
                .child("Calendar")
                .on_click(toggle_tick_too)
        });
        let others = (self.toggles.on(Toggle::ShowAll) && open).then(|| {
            div()
                .ml(px(30.0))
                .px_2()
                .py_1()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .child("No other calendars")
        });
        div()
            .px_3()
            .pt_1()
            .flex()
            .flex_col()
            .gap_2()
            .child(add)
            .child(group)
            .children(own)
            .children(others)
            .child(div().flex().child(self.show_all(palette, cx)))
    }

    /// Show all (or Show less) under the calendars.
    fn show_all(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        div()
            .debug_selector(|| "calendar-show-all".to_owned())
            .id("calendar-show-all")
            .ml(px(36.0))
            .mt_2()
            .px_2()
            .py_1()
            .w_auto()
            .rounded_md()
            .cursor_pointer()
            .text_color(rgb(palette.brand))
            .hover(move |link| link.bg(rgb(palette.hover)))
            .child(if self.toggles.on(Toggle::ShowAll) {
                "Show less"
            } else {
                "Show all"
            })
            .on_click(cx.listener(|this, _, _, cx| {
                this.toggles.flip(Toggle::ShowAll);
                cx.notify();
            }))
    }

    /// The toolbar over the grid.
    pub(super) fn toolbar(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let mode = self.mode;
        let pane_tip = if self.toggles.on(Toggle::PaneHidden) {
            "Show the calendar pane"
        } else {
            "Hide the calendar pane"
        };
        let today = Button::new("calendar-today")
            .icon(Glyph::CALENDAR_TODAY.icon())
            .label("Today")
            .ghost()
            .on_click(cx.listener(|this, _, _, cx| this.today(cx)));
        let previous = icon_button(
            "calendar-previous",
            Glyph::CHEVRON_LEFT,
            format!("Go to previous {}", mode.unit()),
        )
        .on_click(cx.listener(|this, _, _, cx| this.step(false, cx)));
        let next = icon_button(
            "calendar-next",
            Glyph::CHEVRON_RIGHT,
            format!("Go to next {}", mode.unit()),
        )
        .on_click(cx.listener(|this, _, _, cx| this.step(true, cx)));
        let dates = Button::new("calendar-dates")
            .ghost()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(calendar::title(self.anchor, mode))
                    .child(Glyph::CHEVRON_DOWN.icon().size(px(14.0))),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.picker = match this.picker {
                    Some(_) => None,
                    None => Some(this.anchor.with_day(1).unwrap_or(this.anchor)),
                };
                cx.notify();
            }));
        div()
            .debug_selector(|| "calendar-toolbar".to_owned())
            .h(px(56.0))
            .flex_none()
            .px_3()
            .flex()
            .items_center()
            .gap_1()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(selected_by(
                "calendar-pane-toggle",
                icon_button("calendar-pane-toggle", Glyph::PANEL_LEFT, pane_tip).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.toggles.flip(Toggle::PaneHidden);
                        cx.notify();
                    },
                )),
            ))
            .child(separator(palette))
            .child(selected_by("calendar-today", today))
            .child(selected_by("calendar-previous", previous))
            .child(selected_by("calendar-next", next))
            .child(selected_by("calendar-dates", dates))
            .child(div().flex_1())
            .child(self.filter_menu(palette, cx))
            .child(self.mode_switcher(cx))
            .child(Self::more_menu(cx))
            .child(separator(palette))
            .child(selected_by(
                "calendar-join-id",
                Button::new("calendar-join-id")
                    .icon(Glyph::HASH.icon())
                    .label("Join with an ID")
                    .ghost()
                    .on_click(cx.listener(|this, _, window, cx| this.open_join_id(window, cx))),
            ))
            .child(Self::new_button(palette, cx))
    }

    /// The view switcher: its icon, the view's name and a menu of the views.
    pub(super) fn mode_switcher(&self, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity().downgrade();
        let mode = self.mode;
        selected_by(
            "calendar-mode",
            Button::new("calendar-mode")
                .ghost()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(Glyph::CALENDAR_DAYS.icon().size(px(16.0)))
                        .child(mode.label())
                        .child(Glyph::CHEVRON_DOWN.icon().size(px(14.0))),
                )
                .dropdown_menu(move |menu, _, _| {
                    Mode::ALL.iter().fold(menu.min_w(px(180.0)), |menu, choice| {
                        let (view, choice) = (view.clone(), *choice);
                        let selector = format!("calendar-mode-{}", choice.key());
                        menu.item(
                            PopupMenuItem::element(move |_, _| {
                                let selector = selector.clone();
                                div().debug_selector(move || selector.clone()).child(choice.label())
                            })
                            .checked(choice == mode)
                            .on_click(move |_, _, cx: &mut App| {
                                let _ = view.update(cx, |view, cx| view.set_mode(choice, cx));
                            }),
                        )
                    })
                }),
        )
        .into_any_element()
    }

    /// "…": Refresh.
    pub(super) fn more_menu(cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity().downgrade();
        selected_by(
            "calendar-more",
            icon_button("calendar-more", Glyph::MORE, "More options").dropdown_menu(move |menu, _, _| {
                let view = view.clone();
                menu.min_w(px(180.0)).item(
                    PopupMenuItem::element(|_, _| {
                        div()
                            .debug_selector(|| "calendar-refresh".to_owned())
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(Glyph::REFRESH.icon().size(px(14.0)))
                            .child("Refresh")
                    })
                    .on_click(move |_, _, cx: &mut App| {
                        let _ = view.update(cx, |view, cx| view.read(true, cx));
                    }),
                )
            }),
        )
        .into_any_element()
    }

    /// The split New: the brand button and its menu's chevron.
    pub(super) fn new_button(palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let part = |id: &'static str| {
            div()
                .debug_selector(move || id.to_owned())
                .id(id)
                .h(px(32.0))
                .flex()
                .items_center()
                .gap_2()
                // the split button lit as one: the brand colour running into the accent across both parts
                .bg(crate::theme::gradient(
                    palette.brand,
                    crate::theme::mix(palette.brand, palette.accent, 0.6),
                    90.0,
                ))
                .text_color(rgb(palette.on_brand))
                .cursor_pointer()
                .hover(move |part| part.bg(rgb(palette.brand_hover)))
        };
        div()
            .ml_2()
            .flex()
            .items_center()
            .gap(px(1.0))
            .child(
                part("calendar-new")
                    .px_3()
                    .rounded_l_md()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(Glyph::CALENDAR_ADD.icon().size(px(16.0)))
                    .child("New event")
                    .on_click(cx.listener(|this, _, _, cx| {
                        let slot = this.next_slot();
                        this.new_event(slot, false, cx);
                    })),
            )
            .child(Self::new_menu(palette, cx))
            .into_any_element()
    }

    /// The split New's menu: a new event, or a new Teams meeting.
    fn new_menu(palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity().downgrade();
        selected_by(
            "calendar-new-more",
            Button::new("calendar-new-more")
                .h(px(32.0))
                .px_2()
                .rounded_r_md()
                .bg(crate::theme::gradient(
                    crate::theme::mix(palette.brand, palette.accent, 0.6),
                    palette.accent,
                    90.0,
                ))
                .text_color(rgb(palette.on_brand))
                .tooltip("More ways to schedule")
                .child(Glyph::CHEVRON_DOWN.icon().size(px(14.0)))
                .dropdown_menu(move |menu, _, _| {
                    menu.min_w(px(200.0))
                        .item(new_item(
                            view.clone(),
                            "calendar-new-event",
                            Glyph::CALENDAR,
                            "Event",
                            false,
                        ))
                        .item(new_item(
                            view.clone(),
                            "calendar-new-meeting",
                            Glyph::VIDEO,
                            "Teams meeting",
                            true,
                        ))
                }),
        )
        .into_any_element()
    }

    /// What a control that can't act yet says, with its close.
    pub(super) fn notice_bar(&self, palette: Palette, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let notice = self.notice?;
        Some(
            div()
                .debug_selector(|| "calendar-notice".to_owned())
                .mx_3()
                .mt_2()
                .px_3()
                .py_2()
                .flex()
                .items_center()
                .gap_2()
                .rounded_md()
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface))
                .child(Glyph::INFO.icon().size(px(16.0)).text_color(rgb(palette.brand)))
                .child(div().flex_1().text_sm().child(notice))
                .child(selected_by(
                    "calendar-notice-close",
                    icon_button("calendar-notice-close", Glyph::X, "Close")
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.notice = None;
                            cx.notify();
                        })),
                )),
        )
    }

    /// The month picker under the toolbar's dates.
    pub(super) fn picker_view(&self, palette: Palette, cx: &mut Context<Self>) -> Option<gpui::Stateful<gpui::Div>> {
        let month = self.picker?;
        let header = div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                div()
                    .flex_1()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(month.format("%B %Y").to_string()),
            )
            .child(selected_by(
                "calendar-picker-previous",
                icon_button("calendar-picker-previous", Glyph::ARROW_UP, "Previous month")
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.picker = this.picker.map(|month| calendar::add_months(month, -1));
                        cx.notify();
                    })),
            ))
            .child(selected_by(
                "calendar-picker-next",
                icon_button("calendar-picker-next", Glyph::ARROW_DOWN, "Next month")
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.picker = this.picker.map(|month| calendar::add_months(month, 1));
                        cx.notify();
                    })),
            ));
        Some(
            div()
                .debug_selector(|| "calendar-picker".to_owned())
                .id("calendar-picker")
                .occlude()
                .absolute()
                .top(px(52.0))
                .left(px(150.0))
                .w(px(280.0))
                .p_3()
                .flex()
                .flex_col()
                .gap_1()
                .rounded_lg()
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface))
                .shadow_lg()
                .child(header)
                .child(self.month_grid(month, true, palette, cx)),
        )
    }
}

/// A calendar's tick box: brand-filled with a check while its events are shown.
pub(super) fn calendar_tick(selector: &'static str, ticked: bool, palette: Palette) -> gpui::Div {
    div()
        .debug_selector(move || selector.to_owned())
        .size(px(18.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(if ticked {
            palette.brand
        } else {
            palette.muted_foreground
        }))
        .when(ticked, |tick| {
            tick.bg(rgb(palette.brand))
                .text_color(rgb(palette.on_brand))
                .child(Glyph::CHECK.icon().size(px(12.0)))
        })
}

/// A row of the calendars' list.
fn calendar_row(id: &'static str, palette: Palette) -> gpui::Stateful<gpui::Div> {
    div()
        .debug_selector(move || id.to_owned())
        .id(id)
        .h(px(36.0))
        .px_2()
        .flex()
        .items_center()
        .gap_3()
        .rounded_md()
        .cursor_pointer()
        .hover(move |row| row.bg(rgb(palette.hover)))
}

/// A row of New's menu: a new event, or a Teams meeting (`online`), at the next half hour.
fn new_item(
    view: gpui::WeakEntity<CalendarView>,
    id: &'static str,
    glyph: Glyph,
    label: &'static str,
    online: bool,
) -> PopupMenuItem {
    PopupMenuItem::element(move |_, _| {
        div()
            .debug_selector(move || id.to_owned())
            .flex()
            .items_center()
            .gap_2()
            .child(glyph.icon().size(px(14.0)))
            .child(label)
    })
    .on_click(move |_, _, cx: &mut App| {
        let _ = view.update(cx, |view, cx| {
            let slot = view.next_slot();
            view.new_event(slot, online, cx);
        });
    })
}
