//! The card editor's comboboxes, as Teams draws its Team and Channel fields: a text field showing the chosen name with
//! a chevron. Focusing it lists every choice with a check on the chosen one; typing filters the list to the names
//! holding the text, and "No results match your search" when none does. A click on a row (or Enter, for the first
//! match) chooses it. Leaving the field closes the list and shows the chosen name again.
use super::*;

/// Which of the card's two choices a combobox makes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Which {
    Team,
    Channel,
}

impl Which {
    fn selector(self) -> &'static str {
        match self {
            Self::Team => "workflow-simple-team",
            Self::Channel => "workflow-simple-channel",
        }
    }
}

/// One combobox: its text field, whether its list is open, and whether the text is a filter the user typed rather
/// than the chosen name.
pub(super) struct Combo {
    pub(super) input: Entity<InputState>,
    open: bool,
    typed: bool,
    /// The text to show once the view next has its window (the chosen name, or nothing when the choice was cleared).
    pending: Option<String>,
    _subscription: Subscription,
}

impl Combo {
    pub(super) fn new(
        which: Which,
        placeholder: &'static str,
        text: String,
        window: &mut Window,
        cx: &mut Context<WorkflowDialog>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(text));
        let subscription = cx.subscribe_in(&input, window, move |this, _, event: &InputEvent, window, cx| {
            this.combo_event(which, event, window, cx);
        });
        Self {
            input,
            open: false,
            typed: false,
            pending: None,
            _subscription: subscription,
        }
    }

    /// Shows `text` in the field once the window is at hand.
    pub(super) fn show(&mut self, text: String) {
        self.pending = Some(text);
        self.typed = false;
    }

    /// Applies a text waiting for the window.
    pub(super) fn sync(&mut self, window: &mut Window, cx: &mut App) {
        if let Some(text) = self.pending.take() {
            self.input.update(cx, |state, cx| state.set_value(text, window, cx));
        }
    }

    /// The choices the list shows: all of them, or those holding the typed text.
    fn visible<'a>(&self, options: &'a [(String, String)], cx: &App) -> Vec<&'a (String, String)> {
        let query = self.input.read(cx).value().to_lowercase();
        options
            .iter()
            .filter(|(_, name)| !self.typed || query.trim().is_empty() || name.to_lowercase().contains(query.trim()))
            .collect()
    }
}

impl WorkflowDialog {
    fn combo_mut(&mut self, which: Which) -> Option<&mut Combo> {
        let simple = self.simple.as_mut()?;
        Some(match which {
            Which::Team => &mut simple.team_combo,
            Which::Channel => &mut simple.channel_combo,
        })
    }

    fn combo_event(&mut self, which: Which, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            InputEvent::Focus => {
                if let Some(combo) = self.combo_mut(which) {
                    combo.open = true;
                    combo.typed = false;
                }
            }
            InputEvent::Change => {
                if let Some(combo) = self.combo_mut(which) {
                    combo.open = true;
                    combo.typed = true;
                }
            }
            InputEvent::PressEnter { .. } => {
                let first = self.combo_options(which).and_then(|options| {
                    let combo = self.combo(which)?;
                    combo.visible(&options, cx).first().map(|(value, _)| value.clone())
                });
                if let Some(value) = first {
                    self.choose(which, value, window, cx);
                }
            }
            InputEvent::Blur => self.close_combo(which),
        }
        if let Some(simple) = &mut self.simple {
            simple.team_combo.sync(window, cx);
            simple.channel_combo.sync(window, cx);
        }
        cx.notify();
    }

    fn combo(&self, which: Which) -> Option<&Combo> {
        let simple = self.simple.as_ref()?;
        Some(match which {
            Which::Team => &simple.team_combo,
            Which::Channel => &simple.channel_combo,
        })
    }

    /// Closes the list and shows the chosen name again in place of a filter.
    fn close_combo(&mut self, which: Which) {
        let name = self.chosen_name(which).unwrap_or_default();
        if let Some(combo) = self.combo_mut(which) {
            combo.open = false;
            if combo.typed {
                combo.show(name);
            }
        }
    }

    /// The choices of `which`: the account's teams, or the chosen team's channels, as (service id, name).
    pub(super) fn combo_options(&self, which: Which) -> Option<Vec<(String, String)>> {
        let simple = self.simple.as_ref()?;
        Some(match which {
            Which::Team => self
                .teams
                .iter()
                .filter(|team| !team.archived)
                .map(|team| {
                    (
                        team.group_id.clone().unwrap_or_else(|| team.id.clone()),
                        team.name.clone(),
                    )
                })
                .collect(),
            Which::Channel => simple
                .team
                .as_deref()
                .and_then(|group| self.team_by_group(group))
                .map(|team| {
                    team.channels
                        .iter()
                        .map(|channel| (channel.id.clone(), channel.name.clone()))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    fn chosen_name(&self, which: Which) -> Option<String> {
        let simple = self.simple.as_ref()?;
        let chosen = match which {
            Which::Team => simple.team.as_ref(),
            Which::Channel => simple.channel.as_ref(),
        }?;
        self.combo_options(which)?
            .into_iter()
            .find(|(value, _)| value == chosen)
            .map(|(_, name)| name)
    }

    /// Makes a choice. A team other than the chosen one clears the channel, as Teams does.
    pub(super) fn choose(&mut self, which: Which, value: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(simple) = &mut self.simple else {
            return;
        };
        match which {
            Which::Team if simple.team.as_deref() != Some(&value) => {
                simple.team = Some(value);
                simple.channel = None;
                simple.channel_combo.show(String::new());
            }
            Which::Team => {}
            Which::Channel => simple.channel = Some(value),
        }
        let name = self.chosen_name(which).unwrap_or_default();
        if let Some(combo) = self.combo_mut(which) {
            combo.open = false;
            combo.show(name);
        }
        if let Some(simple) = &mut self.simple {
            simple.team_combo.sync(window, cx);
            simple.channel_combo.sync(window, cx);
        }
        cx.notify();
    }

    /// A required combobox as Teams draws it: its label with a red star, the whole-width field, and its list while open.
    pub(super) fn combo_field(
        &self,
        which: Which,
        label: &'static str,
        palette: Palette,
        cx: &Context<Self>,
    ) -> AnyElement {
        let (Some(combo), Some(options)) = (self.combo(which), self.combo_options(which)) else {
            return div().into_any_element();
        };
        let chosen = match which {
            Which::Team => self.simple.as_ref().and_then(|simple| simple.team.clone()),
            Which::Channel => self.simple.as_ref().and_then(|simple| simple.channel.clone()),
        };
        let input = combo.input.clone();
        let field = div()
            .relative()
            // A press in the field opens its list at once (its focus follows).
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    if let Some(combo) = this.combo_mut(which)
                        && !combo.open
                    {
                        combo.open = true;
                        combo.typed = false;
                        cx.notify();
                    }
                }),
            )
            .child(selected_by(
                which.selector(),
                Input::new(&combo.input).suffix(
                    div()
                        .id(SharedString::from(format!("{}-chevron", which.selector())))
                        .cursor_pointer()
                        .child(
                            Glyph::CHEVRON_DOWN
                                .icon()
                                .size(px(16.0))
                                .text_color(rgb(palette.muted_foreground)),
                        )
                        .on_click(move |_, window, cx| input.update(cx, |state, cx| state.focus(window, cx))),
                ),
            ))
            .when(combo.open, |field| {
                field.child(
                    gpui::deferred(
                        div()
                            .absolute()
                            .top(px(38.0))
                            .left_0()
                            .right_0()
                            .child(Self::combo_list(
                                which,
                                &combo.visible(&options, cx),
                                chosen.as_ref(),
                                palette,
                                cx,
                            )),
                    )
                    .with_priority(1),
                )
            });
        div()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(
                div()
                    .flex()
                    .gap_1()
                    .child(label)
                    .child(div().text_color(rgb(theme::DANGER)).child("*")),
            )
            .child(field)
            .into_any_element()
    }

    fn combo_list(
        which: Which,
        rows: &[&(String, String)],
        chosen: Option<&String>,
        palette: Palette,
        cx: &Context<Self>,
    ) -> AnyElement {
        let list = div()
            .id(SharedString::from(format!("{}-list", which.selector())))
            .max_h(px(280.0))
            .overflow_y_scroll()
            .py_1()
            .flex()
            .flex_col()
            .rounded_md()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .shadow_lg()
            .occlude()
            // A click in the list keeps the field's keyboard, so the field doesn't close the list before the row
            // takes its click.
            .on_mouse_down(gpui::MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            });
        if rows.is_empty() {
            return list
                .child(div().px_3().py_2().pl(px(38.0)).child("No results match your search"))
                .into_any_element();
        }
        list.children(rows.iter().map(|(value, name)| {
            let selector = format!("{}-{value}", which.selector());
            let checked = chosen == Some(value);
            let value = value.clone();
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .cursor_pointer()
                .hover(|row| row.bg(rgb(palette.hover)))
                .child(
                    div()
                        .w(px(16.0))
                        .when(checked, |mark| mark.child(Glyph::CHECK.icon().size(px(14.0)))),
                )
                .child(div().flex_1().min_w_0().truncate().child(name.clone()))
                .on_click(cx.listener(move |this, _, window, cx| this.choose(which, value.clone(), window, cx)))
        }))
        .into_any_element()
    }
}
