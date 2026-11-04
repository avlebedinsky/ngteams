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
                    .child(
                        // Teams' header: the Workflows app's icon on a small white tile.
                        div()
                            .debug_selector(|| "workflow-app-icon".to_owned())
                            .size(px(32.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(0x00e0_e0e0))
                            .bg(rgb(0x00ff_ffff))
                            .child(gpui::img(art::workflows()).size(px(22.0))),
                    )
                    .child(div().text_xl().font_weight(gpui::FontWeight::BOLD).child("Workflows")),
            )
            .child(
                icon_button("workflow-close", Glyph::X, "Close workflows")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(TeamDialogEvent::Closed))),
            )
            .into_any_element()
    }

    pub(super) fn template_card(card: &Card, height: f32, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let selected = card.clone();
        let id = format!("workflow-template-{}", card.id);
        selected_by(
            id.clone(),
            Button::new(id)
                .ghost()
                .w_full()
                .min_w_0()
                .p_0()
                .h(px(height))
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
                        .bg(rgb(palette.surface))
                        .shadow_sm()
                        .child(
                            div()
                                .flex()
                                .w_full()
                                .min_w_0()
                                .gap_3()
                                .items_start()
                                .child(Self::kind_mark(card))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .whitespace_normal()
                                        .line_clamp(2)
                                        .text_ellipsis()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(card.name.clone()),
                                ),
                        )
                        .child(
                            // Teams shows two lines of the description, cut with an ellipsis
                            div()
                                .flex_1()
                                .w_full()
                                .min_w_0()
                                .min_h_0()
                                .whitespace_normal()
                                .line_clamp(2)
                                .text_ellipsis()
                                .text_sm()
                                .text_color(rgb(palette.muted_foreground))
                                .child(card.description.clone()),
                        )
                        .child(Self::template_apps(card)),
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

    /// The template's kind as Teams' cards mark it: a schedule's repeat on blue, an update's bell on green, an
    /// action's pen on rose.
    pub(super) fn kind_mark(card: &Card) -> gpui::Div {
        let (glyph, tint, ink) = match card.kind() {
            model::TemplateKind::Schedule => (Glyph::REPEAT, 0x00cf_e4fa, 0x0011_5ea3),
            model::TemplateKind::GetUpdates => (Glyph::BELL, 0x00c9_eac5, 0x0011_5c1d),
            model::TemplateKind::TakeAction => (Glyph::PENCIL, 0x00fb_d9d3, 0x00a1_2f12),
        };
        div()
            .flex_none()
            .size(px(34.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .bg(rgb(tint))
            .text_color(rgb(ink))
            .child(glyph.icon().size(px(18.0)))
    }

    /// The template's apps as Teams' cards show them: each connector's icon, at the card's bottom right.
    fn template_apps(card: &Card) -> gpui::Div {
        div().flex().w_full().justify_end().gap_2().children(
            card.connectors()
                .into_iter()
                .map(|api| gpui::img(art::connector(api)).size(px(16.0))),
        )
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
                        .on_click(cx.listener(|this, _, _, cx| this.start_build(cx))),
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
                    .children(row.iter().map(|card| Self::template_card(card, 175.0, palette, cx))),
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
        // The channel's dialog lists only the workflows that are on, as Teams' own reads do.
        let cards: Vec<_> = self
            .workflows
            .iter()
            .filter(|card| card.enabled())
            .filter(|card| match (self.tab, &self.place) {
                (Tab::Channel, Some((team, channel))) => {
                    card.in_channel(team.group_id.as_deref().unwrap_or(&team.id), &channel.id)
                }
                (Tab::Channel, None) => false,
                (Tab::Workflows, _) => !card.is_ai(),
                (Tab::Ai, _) => card.is_ai(),
            })
            .collect();
        let mut list = div()
            .flex()
            .flex_col()
            .gap_4()
            .child(Self::workflows_heading(
                !cards.is_empty() && !self.loading.workflows,
                cx,
            ))
            .child(self.tabs(cx));
        // a list read again keeps its rows (a row's change spins in its row until then)
        if self.loading.workflows && self.workflows.is_empty() {
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
            if let Some(message) = self.ai_access_state(palette) {
                return list.child(message).into_any_element();
            }
            list = list.child(div().text_sm().text_color(rgb(palette.muted_foreground)).child(
                "Cloud AI workflows are shown here. Copilot actions could not be loaded from the AI workflow service.",
            ));
        }
        if cards.is_empty() {
            list = list.child(Self::no_workflows(palette, cx));
        }
        list = list.child(
            div()
                .flex()
                .flex_col()
                .children(cards.into_iter().map(|card| self.workflow_row(card, palette, cx))),
        );
        list.children(self.load_more(cx)).into_any_element()
    }

    /// "Load more workflows", while the service holds more pages of the list.
    pub(super) fn load_more(&self, cx: &Context<Self>) -> Option<AnyElement> {
        self.continuation.as_ref()?;
        Some(
            selected_by(
                "workflow-load-more",
                Button::new("workflow-load-more")
                    .label("Load more workflows")
                    .disabled(self.loading.workflows)
                    .on_click(cx.listener(|this, _, _, cx| {
                        let next = this.continuation.clone();
                        this.read_workflows(next, cx);
                    })),
            )
            .into_any_element(),
        )
    }

    /// "Your workflows", with Teams' "See all" while the channel lists any.
    fn workflows_heading(see_all: bool, cx: &Context<Self>) -> AnyElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Your workflows"))
            .when(see_all, |heading| {
                heading.child(selected_by(
                    "workflow-see-all",
                    Button::new("workflow-see-all")
                        .ghost()
                        .label("See all")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(TeamDialogEvent::Workflows { see_all: true }))),
                ))
            })
            .into_any_element()
    }

    /// Teams' empty list: what to do, and "View templates" back up to them.
    fn no_workflows(palette: Palette, cx: &Context<Self>) -> AnyElement {
        state(
            "Create an automation for your channel",
            "Pick one of our ready-to-go templates or build your own from scratch",
            palette,
        )
        .child(selected_by(
            "workflow-view-templates",
            Button::new("workflow-view-templates")
                .outline()
                .label("View templates")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.screen = Screen::Browse;
                    this.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
                    cx.notify();
                })),
        ))
        .into_any_element()
    }

    fn ai_access_state(&self, palette: Palette) -> Option<gpui::Div> {
        match &self.copilot_eligibility {
            None => Some(state("Checking AI workflow access…", "", palette)),
            Some(Err(error)) => Some(state(
                "AI workflow access could not be checked",
                model::problem(error),
                palette,
            )),
            Some(Ok(false)) => Some(state(
                "AI workflows require Copilot access",
                "Microsoft 365 Copilot access is not enabled for this account. Ask your administrator to check its license and policy.",
                palette,
            )),
            Some(Ok(true)) => None,
        }
    }

    pub(super) fn body(&self, palette: Palette, cx: &Context<Self>) -> AnyElement {
        match &self.screen {
            Screen::Home => self.app_home(palette, cx),
            Screen::Templates => self.app_templates(palette, cx),
            Screen::AllWorkflows => self.app_all_workflows(palette, cx),
            Screen::DeleteListed(_) if self.app.is_some() => match self.listing {
                Listing::All => self.app_all_workflows(palette, cx),
                _ => self.app_home(palette, cx),
            },
            Screen::Build { .. } => self.build_view(palette, cx),
            Screen::Browse | Screen::DeleteListed(_) => div()
                .flex()
                .flex_col()
                .gap_8()
                .child(self.catalog(palette, cx))
                .child(self.workflow_list(palette, cx))
                .into_any_element(),
            Screen::Edit { .. } => match &self.simple {
                Some(simple) => self.simple_editor(simple, palette, cx),
                None => self.editor(palette, cx),
            },
            Screen::Detail(card) | Screen::Delete(card) => self.detail(card, palette, cx),
            Screen::RunDetails { card, run, actions } => Self::run_view(card, run, actions, palette),
            Screen::RunInputs { card, .. } => self.run_inputs_view(card, palette, cx),
        }
    }

    fn reconcile_button(cx: &Context<Self>) -> AnyElement {
        selected_by(
            "workflow-reconcile",
            Button::new("workflow-reconcile")
                .label("Check current status")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.screen = this.start_screen();
                    this.refresh(cx);
                })),
        )
        .into_any_element()
    }

    /// Teams' footer: Save and Back under the card editor, Back and Workflows Home under an Overview and its delete
    /// question, Workflows Home on the workflow list, and "Back to workflows" (to the list) on every other screen.
    /// Workflows Home leaves the dialog for the Workflows app in the main window.
    pub(super) fn footer(&self, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let footer = div()
            .flex()
            .items_center()
            .justify_end()
            .gap_2()
            .px_8()
            .py_6()
            .border_t_1()
            .border_color(rgb(palette.border));
        if matches!(self.screen, Screen::Edit { .. }) && self.simple.is_some() {
            return footer.child(self.simple_footer(cx)).into_any_element();
        }
        // Teams' Workflows Home leaves the channel's dialog for the Workflows app's own Home, hence its
        // external-link icon.
        let home = matches!(
            self.screen,
            Screen::Browse | Screen::Detail(_) | Screen::Delete(_) | Screen::DeleteListed(_)
        );
        footer
            .when(matches!(self.screen, Screen::Detail(_) | Screen::Delete(_)), |footer| {
                footer.child(selected_by(
                    "workflow-detail-back",
                    Button::new("workflow-detail-back")
                        .outline()
                        .min_w(px(96.0))
                        .label("Back")
                        .disabled(self.busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.screen = this.start_screen();
                            this.error = None;
                            cx.notify();
                        })),
                ))
            })
            .child(selected_by(
                "workflow-home",
                Button::new("workflow-home")
                    .primary()
                    .when(home, |button| button.icon(Glyph::EXTERNAL_LINK.icon()))
                    .label(if home { "Workflows Home" } else { "Back to workflows" })
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.error = None;
                        if home && this.app.is_none() {
                            this.show_home(cx);
                            return;
                        }
                        this.screen = this.start_screen();
                        cx.notify();
                    })),
            ))
            .into_any_element()
    }
}

impl WorkflowDialog {
    /// Another screen starts at its top; the delete question keeps the Overview where it was.
    fn top_of_a_new_screen(&mut self) {
        let kind = match &self.screen {
            Screen::Browse | Screen::DeleteListed(_) => 0,
            Screen::Home => 1,
            Screen::Templates => 7,
            Screen::AllWorkflows => 8,
            Screen::Build { .. } => 2,
            Screen::Edit { .. } => 3,
            Screen::Detail(_) | Screen::Delete(_) => 4,
            Screen::RunDetails { .. } => 5,
            Screen::RunInputs { .. } => 6,
        };
        if self.scrolled_for != Some(kind) {
            self.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
            self.scrolled_for = Some(kind);
        }
    }

    /// What went wrong, the check after an unknown outcome and notices, in sight above the footer however far down a
    /// long Overview is scrolled.
    pub(super) fn status(&self, cx: &Context<Self>) -> gpui::Div {
        div()
            .px_6()
            .when_some(self.error.as_ref(), |view, error| {
                view.child(
                    div()
                        .debug_selector(|| "workflow-error".to_owned())
                        .p_3()
                        .text_color(rgb(theme::DANGER))
                        .child(error.clone()),
                )
            })
            .when(self.uncertain.is_some(), |view| view.child(Self::reconcile_button(cx)))
            .when_some(self.notice.as_ref(), |view, notice| {
                view.child(div().p_3().child(notice.clone()))
            })
    }
}

impl Render for WorkflowDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        self.top_of_a_new_screen();
        if self.app.is_some() {
            return self.app_page(palette, cx);
        }
        div()
            .id("workflow-dialog")
            .debug_selector(|| "workflow-dialog".to_owned())
            .relative()
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
                        this.simple = None;
                        this.screen = this.start_screen();
                        this.error = None;
                        cx.notify();
                    }
                }
            }))
            .child(div().p_6().child(Self::header(cx)))
            .child(
                div()
                    .id("workflow-scroll")
                    .debug_selector(|| "workflow-scroll".to_owned())
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .px_6()
                    .pb_6()
                    .child(self.body(palette, cx)),
            )
            .child(self.status(cx))
            .child(self.footer(palette, cx))
            .children(self.prompt(palette, cx))
            .into_any_element()
    }
}

impl WorkflowDialog {
    /// The question over the page: deleting a workflow, or building in Power Automate.
    pub(super) fn prompt(&self, palette: Palette, cx: &Context<Self>) -> Option<AnyElement> {
        match &self.screen {
            Screen::Delete(card) => Some(self.delete_prompt(card, false, palette, cx)),
            Screen::DeleteListed(card) => Some(self.delete_prompt(card, true, palette, cx)),
            Screen::Build { asking: true } => Some(self.build_prompt(palette, cx)),
            _ => None,
        }
    }
}
