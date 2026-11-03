//! The Teams layout: template discovery above the channel's managed workflows, with one scrollable body.
use super::*;

impl WorkflowDialog {
    fn header(cx: &Context<Self>) -> AnyElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(Glyph::WORKFLOW.icon().size(px(26.0)))
                    .child(div().text_xl().font_weight(gpui::FontWeight::BOLD).child("Workflows")),
            )
            .child(
                icon_button("workflow-close", Glyph::X, "Close workflows")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(TeamDialogEvent::Closed))),
            )
            .into_any_element()
    }

    fn template_card(card: &Card, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let selected = card.clone();
        let id = format!("workflow-template-{}", card.id);
        selected_by(
            id.clone(),
            Button::new(id)
                .ghost()
                .w_full()
                .min_w_0()
                .p_0()
                .h(px(175.0))
                .child(
                    div()
                        .size_full()
                        .min_w_0()
                        .overflow_hidden()
                        .flex()
                        .flex_col()
                        .items_start()
                        .gap_3()
                        .p_4()
                        .rounded_lg()
                        .border_1()
                        .border_color(rgb(palette.border))
                        .bg(rgb(palette.background))
                        .child(
                            div()
                                .flex()
                                .w_full()
                                .min_w_0()
                                .gap_3()
                                .items_start()
                                .child(Glyph::BELL.icon().size(px(24.0)))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .whitespace_normal()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(card.name.clone()),
                                ),
                        )
                        .child(
                            div()
                                .whitespace_normal()
                                .text_sm()
                                .text_color(rgb(palette.muted_foreground))
                                .child(card.description.clone()),
                        ),
                )
                .on_click(
                    cx.listener(move |this, _, window, cx| this.configure_template(selected.clone(), window, cx)),
                ),
        )
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .into_any_element()
    }

    fn search_row(&self, cx: &Context<Self>) -> AnyElement {
        div()
            .flex()
            .gap_4()
            .child(div().flex_1().child(selected_by(
                "workflow-search",
                Input::new(&self.search).prefix(Glyph::SEARCH.icon()),
            )))
            .when(!self.core.read_only(), |row| {
                row.child(selected_by(
                    "workflow-build",
                    Button::new("workflow-build")
                        .label("Build from scratch")
                        .icon(Glyph::PLUS.icon())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.screen = Screen::Build;
                            cx.notify();
                        })),
                ))
            })
            .into_any_element()
    }

    fn catalog_heading(&self, cx: &Context<Self>) -> AnyElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Create a workflow"))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(selected_by(
                        "workflow-all-templates",
                        Button::new("workflow-all-templates")
                            .ghost()
                            .label(if self.all_templates {
                                "Recommended templates"
                            } else {
                                "All templates"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.all_templates = !this.all_templates;
                                this.offset = 0;
                                cx.notify();
                            })),
                    ))
                    .child(
                        icon_button("workflow-previous", Glyph::CHEVRON_LEFT, "Previous templates")
                            .disabled(self.offset == 0)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.offset = this.offset.saturating_sub(6);
                                cx.notify();
                            })),
                    )
                    .child(
                        icon_button("workflow-next", Glyph::CHEVRON_RIGHT, "Next templates")
                            .disabled(self.offset + 6 >= self.catalog.len())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.offset += 6;
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }

    fn catalog(&self, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let query = self.search.read(cx).value();
        let cards: Vec<_> = self.catalog.iter().filter(|card| card.matches(&query)).collect();
        let visible: Vec<_> = if self.all_templates || !query.is_empty() {
            cards
        } else {
            cards.into_iter().skip(self.offset).take(6).collect()
        };
        let mut section = div()
            .flex()
            .flex_col()
            .gap_4()
            .child(self.search_row(cx))
            .child(self.catalog_heading(cx));
        if self.loading.catalog {
            section = section.child(state("Loading templates…", "", palette));
        } else if let Some(error) = &self.catalog_error {
            section = section.child(state("Templates could not be loaded", model::problem(error), palette));
        } else if visible.is_empty() {
            section = section.child(state(
                "No matching templates",
                "Try another search or build a workflow from scratch.",
                palette,
            ));
        }
        for row in visible.chunks(3) {
            section = section.child(
                div()
                    .flex()
                    .gap_4()
                    .children(row.iter().map(|card| Self::template_card(card, palette, cx))),
            );
        }
        section.into_any_element()
    }

    fn tabs(&self, cx: &Context<Self>) -> AnyElement {
        let tabs = [
            (Tab::Channel, "Available in this channel", "workflow-tab-channel"),
            (Tab::Workflows, "Workflows", "workflow-tab-all"),
            (Tab::Ai, "AI Workflows", "workflow-tab-ai"),
        ];
        div()
            .flex()
            .items_center()
            .justify_between()
            .child(div().flex().gap_2().children(tabs.map(|(tab, label, id)| {
                selected_by(
                    id,
                    Button::new(id)
                        .ghost()
                        .label(label)
                        .selected(self.tab == tab)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.tab = tab;
                            cx.notify();
                        })),
                )
            })))
            .child(
                icon_button("workflow-refresh", Glyph::REFRESH, "Refresh workflows")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
            .into_any_element()
    }

    fn workflow_list(&self, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let mut list = div()
            .flex()
            .flex_col()
            .gap_4()
            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Your workflows"))
            .child(self.tabs(cx));
        if self.loading.workflows {
            return list
                .child(state("Loading your workflows…", "", palette))
                .into_any_element();
        }
        if let Some(error) = &self.workflows_error {
            return list
                .child(state(
                    "Your workflows could not be loaded",
                    model::problem(error),
                    palette,
                ))
                .into_any_element();
        }
        if self.tab == Tab::Ai {
            return list
                .child(state(
                    "AI workflows could not be read",
                    "The Microsoft Workflows app session required for this list is not connected.",
                    palette,
                ))
                .into_any_element();
        }
        let cards: Vec<_> = self
            .workflows
            .iter()
            .filter(|card| match self.tab {
                Tab::Channel => {
                    card.in_channel(self.team.group_id.as_deref().unwrap_or(&self.team.id), &self.channel.id)
                }
                Tab::Workflows => true,
                Tab::Ai => false,
            })
            .collect();
        if cards.is_empty() {
            list = list.child(state(
                "Create an automation for your channel",
                "Pick a template above or build your own workflow.",
                palette,
            ));
        }
        for card in cards {
            let chosen = card.clone();
            let id = format!("workflow-open-{}", card.id);
            list = list.child(selected_by(
                id.clone(),
                Button::new(id)
                    .ghost()
                    .w_full()
                    .label(format!("{} · {}", card.name, if card.enabled() { "On" } else { "Off" }))
                    .on_click(cx.listener(move |this, _, _, cx| this.open_flow(chosen.clone(), cx))),
            ));
        }
        if self.continuation.is_some() {
            list = list.child(selected_by(
                "workflow-load-more",
                Button::new("workflow-load-more")
                    .label("Load more workflows")
                    .on_click(cx.listener(|this, _, _, cx| {
                        let next = this.continuation.clone();
                        this.read_workflows(next, cx);
                    })),
            ));
        }
        list.into_any_element()
    }

    fn body(&self, palette: Palette, cx: &Context<Self>) -> AnyElement {
        match &self.screen {
            Screen::Home => self.home(palette, cx),
            Screen::Build => Self::builder_choices(cx),
            Screen::Browse => div()
                .flex()
                .flex_col()
                .gap_8()
                .child(self.catalog(palette, cx))
                .child(self.workflow_list(palette, cx))
                .into_any_element(),
            Screen::Edit { .. } => self.editor(palette, cx),
            Screen::Detail(card) => self.detail(card, palette, cx),
            Screen::History(card) => self.history_view(card, palette),
            Screen::Delete(card) => self.delete_view(card, cx),
        }
    }

    fn builder_choices(cx: &Context<Self>) -> AnyElement {
        let choices = [
            (
                BuildKind::Manual,
                "workflow-build-manual",
                "When I run it",
                "Post a message to this channel when you run the workflow.",
            ),
            (
                BuildKind::Scheduled,
                "workflow-build-scheduled",
                "On a schedule",
                "Choose when to post a recurring channel message.",
            ),
            (
                BuildKind::Webhook,
                "workflow-build-webhook",
                "When a webhook arrives",
                "Receive a webhook and post its card to this channel.",
            ),
        ];
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Build a workflow"),
            )
            .child("Choose a trigger, then configure its channel action and connected accounts.")
            .children(choices.map(|(kind, id, name, description)| {
                selected_by(
                    id,
                    Button::new(id)
                        .outline()
                        .w_full()
                        .h(px(90.0))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(name)
                                .child(div().text_sm().child(description)),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| this.build(kind, window, cx))),
                )
            }))
            .into_any_element()
    }

    fn home(&self, palette: Palette, cx: &Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Workflows Home"),
            )
            .child(self.environment_picker(cx))
            .child(self.workflow_list(palette, cx))
            .into_any_element()
    }

    fn environment_picker(&self, cx: &Context<Self>) -> AnyElement {
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        let environments = self.environments.clone();
        let current = self.environment.clone();
        let weak = cx.entity().downgrade();
        let label = environments
            .iter()
            .find(|(id, _)| Some(id) == current.as_ref())
            .map_or("Choose environment", |(_, name)| name.as_str())
            .to_owned();
        selected_by(
            "workflow-environment",
            Button::new("workflow-environment")
                .outline()
                .label(label)
                .icon(Glyph::CHEVRON_DOWN.icon())
                .dropdown_menu(move |menu, _, _| {
                    environments
                        .iter()
                        .fold(menu.scrollable(true).max_h(px(260.0)), |menu, (id, name)| {
                            let (id, name, weak) = (id.clone(), name.clone(), weak.clone());
                            menu.item(
                                PopupMenuItem::element(move |_, _| div().child(name.clone()))
                                    .checked(current.as_ref() == Some(&id))
                                    .on_click(move |_, _, cx| {
                                        let _ = weak.update(cx, |this, cx| this.choose_environment(id.clone(), cx));
                                    }),
                            )
                        })
                }),
        )
        .into_any_element()
    }

    fn choose_environment(&mut self, environment: String, cx: &mut Context<Self>) {
        if self.busy || self.environment.as_deref() == Some(&environment) {
            return;
        }
        self.generation += 1;
        self.tasks.clear();
        self.environment = Some(environment);
        self.workflows.clear();
        self.connections.clear();
        self.continuation = None;
        self.load_workflows(cx);
        self.load_connections(cx);
        cx.notify();
    }

    fn footer(&self, palette: Palette, cx: &Context<Self>) -> AnyElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .p_4()
            .border_t_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child(format!("{} / {}", self.team.name, self.channel.name)),
            )
            .child(selected_by(
                "workflow-home",
                Button::new("workflow-home")
                    .primary()
                    .label(if matches!(self.screen, Screen::Browse) {
                        "Workflows Home"
                    } else {
                        "Back to workflows"
                    })
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.screen = if matches!(this.screen, Screen::Browse) {
                            Screen::Home
                        } else {
                            Screen::Browse
                        };
                        this.tab = Tab::Workflows;
                        this.error = None;
                        cx.notify();
                    })),
            ))
            .into_any_element()
    }
}

impl Render for WorkflowDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        div()
            .id("workflow-dialog")
            .debug_selector(|| "workflow-dialog".to_owned())
            .occlude()
            .w(px(1120.0))
            .max_w_full()
            .h(window.viewport_size().height - px(48.0))
            .max_h(px(900.0))
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .text_color(rgb(palette.foreground))
            .shadow_lg()
            .key_context("WorkflowDialog")
            .track_focus(&self.focus)
            .focus_trap("workflow-dialog-trap", &self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    if matches!(this.screen, Screen::Browse) {
                        cx.emit(TeamDialogEvent::Closed);
                    } else if !this.busy {
                        this.screen = Screen::Browse;
                        this.error = None;
                        cx.notify();
                    }
                }
            }))
            .child(div().p_6().child(Self::header(cx)))
            .child(
                div()
                    .id("workflow-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_6()
                    .pb_6()
                    .child(self.body(palette, cx))
                    .when_some(self.error.as_ref(), |view, error| {
                        view.child(
                            div()
                                .debug_selector(|| "workflow-error".to_owned())
                                .p_3()
                                .text_color(rgb(theme::DANGER))
                                .child(error.clone()),
                        )
                    })
                    .when_some(self.notice.as_ref(), |view, notice| {
                        view.child(div().p_3().child(notice.clone()))
                    }),
            )
            .child(self.footer(palette, cx))
    }
}
