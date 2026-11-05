//! The workflow list's rows, as Teams shows them under "Your workflows": the workflow's name over its state ("Active",
//! or "Active: Next run: <when>" for a schedule), a schedule's recurrence mark and Run, and a "..." menu: Run (when it
//! can be run now), Edit, Manage (the workflow's page in Power Automate, in the browser), Turn off or Turn on, Details
//! (its Overview) and Delete (Teams' question over the list). A click on the row opens its Overview. Edit, Turn off/on
//! and Delete from the list come back to the list.
use super::*;
use gpui_component::Sizable as _;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};

/// Power Automate's page of a workflow, where Teams' Manage leads.
pub(super) fn manage_url(environment: &str, flow: &str) -> String {
    format!("https://make.powerautomate.com/environments/{environment}/flows/{flow}/details")
}

/// `11/9/2026 8:05:00 PM`, a service time as Teams shows it, in the local time zone.
pub(super) fn local_time(stamp: &str) -> Option<String> {
    let time = chrono::DateTime::parse_from_rfc3339(stamp).ok()?;
    Some(
        time.with_timezone(&chrono::Local)
            .format("%-m/%-d/%Y %-I:%M:%S %p")
            .to_string(),
    )
}

impl WorkflowDialog {
    /// Reads the next run of each scheduled workflow listed, as Teams' list shows it: from `from` on, the rows a page
    /// added ("Load more" reads only its own rows' next runs).
    pub(super) fn load_next_runs(&mut self, from: usize, cx: &mut Context<Self>) {
        let cards: Vec<Card> = self.workflows.iter().skip(from).cloned().collect();
        self.next_runs_of(&cards, cx);
    }

    /// Reads the next run of each of `cards` that is a schedule and on.
    pub(super) fn next_runs_of(&mut self, cards: &[Card], cx: &mut Context<Self>) {
        let Some(environment) = self.environment.clone() else {
            return;
        };
        let generation = self.generation;
        for card in cards {
            let scheduled = card
                .properties
                .get("definition")
                .and_then(markup::recurrence_trigger)
                .is_some();
            if !scheduled || !card.enabled() {
                continue;
            }
            let flow = card.id.clone();
            let reply = self.core.workflow_request(WorkflowRequest::Triggers {
                environment: environment.clone(),
                flow: flow.clone(),
            });
            self.tasks
                .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                    let next = reply
                        .await
                        .unwrap_or(Err(WorkflowFailure::Connection))
                        .ok()
                        .and_then(|value| next_run(&value));
                    let _ = this.update(cx, |this, cx| this.next_run_read(generation, flow, next, cx));
                }));
        }
    }

    /// A schedule's next run as read; a read that failed or named none takes back the one shown before, which may
    /// no longer hold.
    fn next_run_read(&mut self, generation: u64, flow: String, next: Option<String>, cx: &mut Context<Self>) {
        if self.generation != generation {
            return;
        }
        match next {
            Some(next) => {
                self.next_runs.insert(flow, next);
            }
            None => {
                self.next_runs.remove(&flow);
            }
        }
        cx.notify();
    }

    /// One workflow in the list.
    pub(super) fn workflow_row(&self, card: &Card, palette: Palette, cx: &Context<Self>) -> AnyElement {
        if self.app.is_some() {
            return self.app_row(card, palette, cx);
        }
        let scheduled = card.properties.get("definition").and_then(markup::recurrence_trigger);
        let state = match (card.enabled(), self.next_runs.get(&card.id)) {
            (true, Some(next)) => format!("Active: Next run: {next}"),
            (true, None) => "Active".to_owned(),
            (false, _) => "Inactive".to_owned(),
        };
        let acting = self.acting.as_deref() == Some(card.id.as_str());
        let opened = card.clone();
        let id = format!("workflow-open-{}", card.id);
        div()
            .id(SharedString::from(id.clone()))
            .debug_selector(move || id.clone())
            .flex()
            .items_center()
            .gap_3()
            .px_5()
            .py_4()
            .border_b_1()
            .border_color(rgb(palette.border))
            .cursor_pointer()
            .hover(|row| row.bg(rgb(palette.hover)))
            .on_click(cx.listener(move |this, _, _, cx| this.open_flow(opened.clone(), cx)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .truncate()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.muted_foreground))
                            .child(card.name.clone()),
                    )
                    .child(div().text_sm().text_color(rgb(palette.muted_foreground)).child(state)),
            )
            .when(scheduled.is_some() && card.enabled(), |row| {
                row.child(
                    div()
                        .px_2()
                        .py_0p5()
                        .rounded_full()
                        .bg(rgb(palette.hover))
                        .child(Glyph::REPEAT.icon().size(px(14.0))),
                )
            })
            .when(acting, |row| row.child(div().px_2().child(Self::row_spinner(palette))))
            .when(
                !acting
                    && (scheduled.is_some() || self.runs_on_message(card))
                    && card.enabled()
                    && !self.core.read_only(),
                |row| {
                    let running = card.clone();
                    row.child(selected_by(
                        format!("workflow-row-run-{}", card.id),
                        icon_button(
                            SharedString::from(format!("workflow-row-run-{}", card.id)),
                            Glyph::PLAY,
                            "Run",
                        )
                        .ghost()
                        .disabled(self.busy || self.uncertain.is_some())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.run_listed(&running, window, cx);
                        })),
                    ))
                },
            )
            .child(self.row_menu(card, cx))
            .into_any_element()
    }

    /// A row of the Workflows app's lists: the name on the left, its state (with a mark, or a spinner while it is
    /// turned off or on) and its "..." menu on the right; a click opens its Overview.
    fn app_row(&self, card: &Card, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let acting = self.acting.as_deref() == Some(card.id.as_str());
        let opened = card.clone();
        let id = format!("workflow-open-{}", card.id);
        let state = if card.enabled() {
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .child(Glyph::REFRESH.icon().size(px(14.0)).text_color(rgb(palette.brand)))
                .child("Active")
        } else {
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .child(Glyph::CIRCLE_MINUS.icon().size(px(14.0)))
                .child("Inactive")
        };
        div()
            .id(SharedString::from(id.clone()))
            .debug_selector(move || id.clone())
            .flex()
            .items_center()
            .gap_3()
            .px_5()
            .min_h(px(88.0))
            .border_b_1()
            .border_color(rgb(palette.border))
            .cursor_pointer()
            .hover(|row| row.bg(rgb(palette.hover)))
            .on_click(cx.listener(move |this, _, _, cx| this.open_flow(opened.clone(), cx)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.muted_foreground))
                    .child(card.name.clone()),
            )
            .child(
                div()
                    .debug_selector(|| "workflow-row-state".to_owned())
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.muted_foreground))
                    .map(|cell| {
                        if acting {
                            cell.child(Self::row_spinner(palette))
                        } else {
                            cell.child(state)
                        }
                    }),
            )
            .child(self.row_menu(card, cx))
            .into_any_element()
    }

    /// The spinner a row shows while it is turned off or on: the kit's, drawn as Teams' ring (`AppAssets`).
    fn row_spinner(palette: Palette) -> AnyElement {
        div()
            .debug_selector(|| "workflow-row-spinner".to_owned())
            .text_color(rgb(palette.brand))
            .child(gpui_component::spinner::Spinner::new().small())
            .into_any_element()
    }

    fn row_menu(&self, card: &Card, cx: &Context<Self>) -> AnyElement {
        let weak = cx.entity().downgrade();
        let card = card.clone();
        let runnable = Self::runnable(&card) || self.runs_on_message(&card);
        let editable = !message::nothing_to_edit(&card);
        let read_only = self.core.read_only();
        // the Workflows app's menu has no Manage
        let manage = self.app.is_none();
        let id = format!("workflow-row-more-{}", card.id);
        selected_by(
            id.clone(),
            icon_button(SharedString::from(id), Glyph::ELLIPSIS, "More options")
                .ghost()
                .disabled(self.busy || self.uncertain.is_some())
                .dropdown_menu(move |menu, _, _| {
                    let item = |key: &'static str, glyph: Glyph, label: &'static str| {
                        let selector = format!("workflow-row-{key}");
                        PopupMenuItem::element(move |_, _| {
                            let selector = selector.clone();
                            div()
                                .debug_selector(move || selector.clone())
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(glyph.icon().size(px(14.0)))
                                .child(label)
                        })
                    };
                    let act = |action: RowAction| {
                        let (weak, card) = (weak.clone(), card.clone());
                        move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
                            let _ = weak.update(cx, |this, cx| this.row_action(action, &card, window, cx));
                        }
                    };
                    let mut menu = menu;
                    if !read_only {
                        if runnable {
                            menu = menu.item(item("run", Glyph::PLAY, "Run").on_click(act(RowAction::Run)));
                        }
                        menu = menu.item(
                            item("edit", Glyph::PENCIL, "Edit")
                                .disabled(!editable)
                                .on_click(act(RowAction::Edit)),
                        );
                        if manage {
                            menu =
                                menu.item(item("manage", Glyph::OPEN_OUT, "Manage").on_click(act(RowAction::Manage)));
                        }
                        menu = menu
                            .item(
                                item(
                                    "toggle",
                                    Glyph::POWER,
                                    if card.enabled() { "Turn off" } else { "Turn on" },
                                )
                                .on_click(act(RowAction::Toggle)),
                            )
                            .separator();
                    }
                    menu = menu.item(item("details", Glyph::LIST, "Details").on_click(act(RowAction::Details)));
                    if !read_only {
                        menu = menu
                            .separator()
                            .item(item("delete", Glyph::TRASH, "Delete").on_click(act(RowAction::Delete)));
                    }
                    menu
                }),
        )
        .into_any_element()
    }

    /// Whether a workflow runs from a message and the dialog has a message to run it on.
    pub(super) fn runs_on_message(&self, card: &Card) -> bool {
        self.message.is_some() && card.enabled() && message::runs_from_message(card)
    }

    /// A workflow run from a message, on the message the dialog was opened from: its run dialog takes this one's
    /// place.
    pub(super) fn run_on_message(&mut self, card: &Card, cx: &mut Context<Self>) {
        let (Some(message), Some(environment)) = (self.message.clone(), self.environment.clone()) else {
            return;
        };
        if self.busy || self.core.read_only() {
            return;
        }
        cx.emit(TeamDialogEvent::RunWorkflow {
            flow: FlowRef {
                environment,
                id: card.id.clone(),
                name: card.name.clone(),
            },
            message,
        });
    }

    /// Whether Teams offers to run a workflow now: a schedule, or one started by hand.
    pub(super) fn runnable(card: &Card) -> bool {
        let triggers = card
            .properties
            .pointer("/definition/triggers")
            .and_then(Value::as_object);
        card.enabled()
            && triggers.is_some_and(|triggers| {
                triggers.values().any(|trigger| {
                    trigger.get("type").and_then(Value::as_str) == Some("Recurrence")
                        || matches!(
                            trigger.get("kind").and_then(Value::as_str),
                            Some("Button" | "PowerApp" | "PowerAppV2" | "Skills" | "PowerPages")
                        )
                })
            })
    }

    fn row_action(&mut self, action: RowAction, card: &Card, window: &mut Window, cx: &mut Context<Self>) {
        self.note_listing();
        match action {
            RowAction::Run => self.run_listed(card, window, cx),
            RowAction::Edit => self.edit_from(card.clone(), true, true, window, cx),
            RowAction::Manage => {
                if let Some(environment) = &self.environment {
                    cx.open_url(&manage_url(environment, &card.id));
                }
            }
            RowAction::Toggle => self.toggle_listed(card, cx),
            RowAction::Details => self.open_flow(card.clone(), cx),
            RowAction::Delete => {
                self.screen = Screen::DeleteListed(card.clone());
                cx.notify();
            }
        }
    }

    /// Runs a workflow from the list: a schedule runs now; one with inputs asks for them first.
    fn run_listed(&mut self, card: &Card, window: &mut Window, cx: &mut Context<Self>) {
        self.note_listing();
        let Some(environment) = self.environment.clone() else {
            return;
        };
        match card.properties.get("definition").and_then(markup::recurrence_trigger) {
            Some(trigger) => {
                self.acting = Some(card.id.clone());
                self.mutate_then(
                    WorkflowRequest::Run {
                        environment,
                        flow: card.id.clone(),
                        trigger,
                    },
                    Outcome::List,
                    cx,
                );
                if !self.busy {
                    self.acting = None;
                }
            }
            None if self.runs_on_message(card) => self.run_on_message(card, cx),
            None => self.prepare_run(card, window, cx),
        }
    }

    /// Turns a workflow off or on from the list, which then reads the workflows again: Teams' channel list shows only
    /// the workflows that are on.
    fn toggle_listed(&mut self, card: &Card, cx: &mut Context<Self>) {
        let Some(environment) = self.environment.clone() else {
            return;
        };
        self.acting = Some(card.id.clone());
        self.mutate_then(
            WorkflowRequest::SetEnabled {
                environment,
                flow: card.id.clone(),
                enabled: !card.enabled(),
            },
            Outcome::List,
            cx,
        );
        if !self.busy {
            self.acting = None;
        }
    }
}

impl WorkflowDialog {
    /// Notes which list a row is acted on in, for the change to return to it.
    fn note_listing(&mut self) {
        self.listing = match self.screen {
            Screen::Home => Listing::Home,
            Screen::AllWorkflows => Listing::All,
            _ => Listing::Channel,
        };
    }
}

#[derive(Clone, Copy)]
enum RowAction {
    Run,
    Edit,
    Manage,
    Toggle,
    Details,
    Delete,
}

/// The next run a workflow's triggers name.
fn next_run(triggers: &Value) -> Option<String> {
    model::rows(triggers)
        .ok()?
        .iter()
        .find_map(|trigger| trigger.pointer("/properties/nextExecutionTime").and_then(Value::as_str))
        .and_then(local_time)
}
