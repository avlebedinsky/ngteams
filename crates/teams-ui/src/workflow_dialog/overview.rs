//! A workflow's Overview, as the Teams Workflows app shows it after Save and when a workflow is opened: its name,
//! description and state with Turn off/on, Edit and a "..." menu holding Delete, the webhook link to copy, how a
//! webhook workflow runs, and its run history. Delete asks first, over the Overview.
use super::*;
use crate::icons::Spinning as _;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};

impl WorkflowDialog {
    pub(super) fn detail(&self, card: &Card, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let webhook = runs::webhook_trigger(card).is_some();
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(heading("Overview"))
            .child(
                boxed(palette)
                    .gap_4()
                    .child(self.name_row(card, cx))
                    .child(
                        div()
                            .text_color(rgb(palette.muted_foreground))
                            .child(card.description.clone()),
                    )
                    .child(state_line(card, palette))
                    .when(!self.core.read_only(), |view| {
                        view.child(self.detail_controls(card, cx))
                    })
                    .children(self.schedule_line(card, palette))
                    .children(self.webhook_copy(card, palette, cx)),
            )
            .when(webhook, |view| {
                view.child(heading("How to run this workflow after setup"))
                    .child(how_to_run(palette))
            })
            .child(self.inline_history(card, palette, cx))
            .child(self.owners_section(palette, cx))
            .into_any_element()
    }

    /// The workflow's name with Rename beside it; while renaming, a field holding the name and Save, which an empty
    /// name keeps off.
    fn name_row(&self, card: &Card, cx: &Context<Self>) -> AnyElement {
        let row = div().flex().items_start().justify_between().gap_4();
        if let Some((field, _)) = &self.rename {
            let saving = self.busy && self.changing == Some(Change::Rename);
            let empty = field.read(cx).value().trim().is_empty();
            return row
                .child(
                    div()
                        .flex_1()
                        .child(selected_by("workflow-rename-field", Input::new(field).disabled(saving))),
                )
                .child(selected_by(
                    "workflow-rename-save",
                    Button::new("workflow-rename-save")
                        .primary()
                        .min_w(px(96.0))
                        .label(if saving { "Saving" } else { "Save" })
                        .spinning(saving)
                        .disabled(saving || empty || self.uncertain.is_some())
                        .on_click(cx.listener(|this, _, _, cx| this.save_rename(cx))),
                ))
                .into_any_element();
        }
        let renaming = card.clone();
        row.child(
            div()
                .flex_1()
                .min_w_0()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(card.name.clone()),
        )
        .when(!self.core.read_only(), |row| {
            row.child(selected_by(
                "workflow-rename",
                Button::new("workflow-rename")
                    .outline()
                    .min_w(px(96.0))
                    .label("Rename")
                    .disabled(self.busy || self.uncertain.is_some())
                    .on_click(cx.listener(move |this, _, window, cx| this.start_rename(&renaming, window, cx))),
            ))
        })
        .into_any_element()
    }

    fn detail_controls(&self, card: &Card, cx: &Context<Self>) -> AnyElement {
        let (editing, changing, deleting) = (card.clone(), card.clone(), card.clone());
        let locked = self.busy || self.uncertain.is_some();
        let weak = cx.entity().downgrade();
        div()
            .flex()
            .gap_2()
            .child(selected_by(
                "workflow-toggle",
                Button::new("workflow-toggle")
                    .outline()
                    .icon(Glyph::POWER.icon())
                    .label(if card.enabled() { "Turn off" } else { "Turn on" })
                    .loading(self.busy && self.changing == Some(Change::Toggle))
                    .disabled(locked)
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(&changing, cx))),
            ))
            .children(Self::run_button(card, locked, cx))
            .child(selected_by(
                "workflow-edit",
                Button::new("workflow-edit")
                    .outline()
                    .icon(Glyph::PENCIL.icon())
                    .label("Edit")
                    .disabled(locked)
                    .on_click(cx.listener(move |this, _, window, cx| this.edit(editing.clone(), true, window, cx))),
            ))
            .child(selected_by(
                "workflow-more",
                icon_button("workflow-more", Glyph::ELLIPSIS, "More options")
                    .outline()
                    .disabled(locked)
                    .dropdown_menu(move |menu, _, _| {
                        let (weak, deleting) = (weak.clone(), deleting.clone());
                        menu.item(
                            PopupMenuItem::element(|_, _| {
                                div()
                                    .debug_selector(|| "workflow-delete".to_owned())
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(Glyph::TRASH.icon().size(px(14.0)))
                                    .child("Delete")
                            })
                            .on_click(move |_, _, cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    this.screen = Screen::Delete(deleting.clone());
                                    cx.notify();
                                });
                            }),
                        )
                    }),
            ))
            .into_any_element()
    }

    fn inline_history(&self, card: &Card, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let refreshing = card.clone();
        let mut runs = boxed(palette).gap_2();
        if self.history.is_empty() {
            runs = runs.child("Your flow hasn't run yet.");
        }
        for run in &self.history {
            let status = run
                .pointer("/properties/status")
                .and_then(Value::as_str)
                .unwrap_or("Unknown");
            let started = run
                .pointer("/properties/startTime")
                .and_then(Value::as_str)
                .unwrap_or("Time unavailable");
            let id = run.get("name").and_then(Value::as_str).unwrap_or_default();
            let (card, run) = (card.clone(), run.clone());
            runs = runs.child(selected_by(
                format!("workflow-run-{id}"),
                Button::new(format!("workflow-history-{id}"))
                    .ghost()
                    .w_full()
                    .justify_start()
                    .label(format!("{status} · {started}"))
                    .on_click(cx.listener(move |this, _, _, cx| this.run_details(card.clone(), run.clone(), cx))),
            ));
        }
        if let Some(next) = &self.history_next {
            let (card, next) = (card.clone(), next.clone());
            runs = runs.child(selected_by(
                "workflow-more-history",
                Button::new("workflow-more-history")
                    .ghost()
                    .label("Load more runs")
                    .disabled(self.busy)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.history_read(card.clone(), Some(next.clone()), cx)),
                    ),
            ));
        }
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(heading("Run history"))
                    .child(selected_by(
                        "workflow-history",
                        Button::new("workflow-history")
                            .ghost()
                            .icon(Glyph::REFRESH.icon())
                            .label("Refresh")
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, _, cx| this.history(refreshing.clone(), cx))),
                    )),
            )
            .child(runs)
            .into_any_element()
    }

    /// Teams' delete question, over the whole dialog and the Overview (or, from a row's menu, the list) it was asked
    /// from; Delete spins while it goes.
    pub(super) fn delete_prompt(&self, card: &Card, listed: bool, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let (chosen, kept) = (card.clone(), card.clone());
        div()
            .absolute()
            .inset_0()
            .rounded_lg()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::rgba(0x0000_0066))
            .occlude()
            .child(
                div().child(
                    div()
                        .debug_selector(|| "workflow-delete-dialog".to_owned())
                        .w(px(600.0))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .p_6()
                        .rounded_lg()
                        .shadow_lg()
                        .bg(rgb(palette.surface))
                        .child(
                            div()
                                .text_xl()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Delete automation"),
                        )
                        .child("Would you like to delete your automation? Alternatively, you can turn it off.")
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(selected_by(
                                    "workflow-delete-confirm",
                                    Button::new("workflow-delete-confirm")
                                        .primary()
                                        .min_w(px(96.0))
                                        .label("Delete")
                                        .spinning(self.busy)
                                        .disabled(self.busy)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if let Some(environment) = this.environment.clone() {
                                                this.mutate_then(
                                                    WorkflowRequest::Delete {
                                                        environment,
                                                        flow: chosen.id.clone(),
                                                    },
                                                    if listed {
                                                        Outcome::List
                                                    } else {
                                                        Outcome::Notice("Workflow deleted")
                                                    },
                                                    cx,
                                                );
                                            }
                                        })),
                                ))
                                .child(selected_by(
                                    "workflow-delete-cancel",
                                    Button::new("workflow-delete-cancel")
                                        .outline()
                                        .min_w(px(96.0))
                                        .label("Cancel")
                                        .disabled(self.busy)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.screen = if listed {
                                                this.list_screen()
                                            } else {
                                                Screen::Detail(kept.clone())
                                            };
                                            cx.notify();
                                        })),
                                )),
                        ),
                ),
            )
            .into_any_element()
    }
}

impl WorkflowDialog {
    /// A schedule's line under the Overview's buttons: "Active: Next run: <when>" and "Last modified: <when>".
    fn schedule_line(&self, card: &Card, palette: Palette) -> Option<AnyElement> {
        card.properties.get("definition").and_then(markup::recurrence_trigger)?;
        let modified = card
            .properties
            .get("lastModifiedTime")
            .and_then(Value::as_str)
            .and_then(rows::local_time);
        let next = self.next_runs.get(&card.id).filter(|_| card.enabled());
        Some(
            div()
                .debug_selector(|| "workflow-schedule".to_owned())
                .flex()
                .flex_wrap()
                .gap_x_6()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .children(next.map(|next| format!("Active: Next run: {next}")))
                .children(modified.map(|modified| format!("Last modified: {modified}")))
                .into_any_element(),
        )
    }

    /// Teams' Run, after Turn off, for a workflow that can run now: a schedule runs at once; one started by hand asks
    /// for its inputs first.
    fn run_button(card: &Card, locked: bool, cx: &Context<Self>) -> Option<AnyElement> {
        if !Self::runnable(card) {
            return None;
        }
        let scheduled = card.properties.get("definition").and_then(markup::recurrence_trigger);
        let running = card.clone();
        Some(
            selected_by(
                "workflow-run",
                Button::new("workflow-run")
                    .outline()
                    .icon(Glyph::PLAY.icon())
                    .label("Run")
                    .disabled(locked)
                    .on_click(cx.listener(move |this, _, window, cx| match &scheduled {
                        Some(trigger) => this.run_now(&running, trigger.clone(), cx),
                        None => this.prepare_run(&running, window, cx),
                    })),
            )
            .into_any_element(),
        )
    }

    /// Runs a schedule now, from its Overview, and reads its runs again.
    fn run_now(&mut self, card: &Card, trigger: String, cx: &mut Context<Self>) {
        let Some(environment) = self.environment.clone() else {
            return;
        };
        self.mutate_then(
            WorkflowRequest::Run {
                environment,
                flow: card.id.clone(),
                trigger,
            },
            Outcome::Stay,
            cx,
        );
    }
}

fn heading(text: &'static str) -> gpui::Div {
    div().px_2().font_weight(gpui::FontWeight::SEMIBOLD).child(text)
}

fn boxed(palette: Palette) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .p_5()
        .rounded_md()
        .border_1()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.surface))
}

/// Teams' state line: a brand-coloured cycle and "Active" while on; a filled grey circle with a bar and "Inactive"
/// while off.
fn state_line(card: &Card, palette: Palette) -> gpui::Div {
    let line = div()
        .debug_selector(|| "workflow-state".to_owned())
        .flex()
        .items_center()
        .gap_2()
        .text_sm()
        .font_weight(gpui::FontWeight::SEMIBOLD);
    if card.enabled() {
        return line
            .child(Glyph::REFRESH.icon().size(px(16.0)).text_color(rgb(palette.brand)))
            .child("Active");
    }
    line.child(
        div()
            .size(px(14.0))
            .mx(px(1.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(rgb(palette.muted_foreground))
            .child(div().w(px(8.0)).h(px(2.0)).rounded_full().bg(rgb(palette.surface))),
    )
    .child("Inactive")
}

/// What Teams says about running a webhook workflow.
fn how_to_run(palette: Palette) -> gpui::Div {
    let point = |text: &'static str| {
        div()
            .flex()
            .gap_2()
            .child(div().text_color(rgb(palette.muted_foreground)).child("•"))
            .child(div().flex_1().child(text))
    };
    boxed(palette)
        .gap_3()
        .child("This automated workflow starts when a card is sent to the webhook link provided above.")
        .child(point(
            "The message must include either an adaptive card or message card formatted payload to show information or ask for input.",
        ))
        .child(point("After the card is sent, Workflows automatically runs the steps you set up."))
        .child(point("Only POST requests are accepted. The max payload is 256 KB."))
}
