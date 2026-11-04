//! The card editor's comboboxes, as Teams draws its Team, Channel, Time, Time zone and connector choices: a text field
//! showing the chosen name with a chevron. Focusing it lists every choice with a check on the chosen one; typing
//! filters the list to the names holding the text, and "No results match your search" when none does ("No options
//! available" when there is nothing to choose). A click on a row (or Enter, for the first match) chooses it. Leaving
//! the field closes the list and shows the chosen name again.
use super::markup::Kind;
use super::simple::Choices;
use super::*;

/// One combobox: its text field, whether its list is open, and whether the text is a filter the user typed rather
/// than the chosen name.
pub(super) struct Combo {
    pub(super) input: Entity<InputState>,
    pub(super) selector: String,
    open: bool,
    typed: bool,
    /// The text to show once the view next has its window (the chosen name, or nothing when the choice was cleared).
    pending: Option<String>,
    _subscription: Subscription,
}

impl Combo {
    pub(super) fn new(
        index: usize,
        selector: String,
        placeholder: &'static str,
        window: &mut Window,
        cx: &mut Context<WorkflowDialog>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subscription = cx.subscribe_in(&input, window, move |this, _, event: &InputEvent, window, cx| {
            this.combo_event(index, event, window, cx);
        });
        Self {
            input,
            selector,
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

    /// The text typed as a filter, if the user typed one.
    fn filter(&self, cx: &App) -> Option<String> {
        let query = self.input.read(cx).value().trim().to_lowercase();
        (self.typed && !query.is_empty()).then_some(query)
    }
}

impl WorkflowDialog {
    pub(super) fn combo_mut(&mut self, index: usize) -> Option<&mut Combo> {
        self.simple.as_mut()?.fields.get_mut(index)?.combo.as_mut()
    }

    fn combo(&self, index: usize) -> Option<&Combo> {
        self.simple.as_ref()?.fields.get(index)?.combo.as_ref()
    }

    fn combo_event(&mut self, index: usize, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            InputEvent::Focus => {
                if let Some(combo) = self.combo_mut(index) {
                    combo.open = true;
                    combo.typed = false;
                }
            }
            InputEvent::Change => {
                if let Some(combo) = self.combo_mut(index) {
                    combo.open = true;
                    combo.typed = true;
                }
            }
            InputEvent::PressEnter { .. } => self.combo_enter(index, window, cx),
            InputEvent::Blur => self.close_combo(index),
        }
        self.sync_combos(window, cx);
        cx.notify();
    }

    /// Enter chooses the first choice the text matches; a site field also takes a typed site address.
    fn combo_enter(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self
            .combo(index)
            .map(|combo| combo.input.read(cx).value().trim().to_owned())
            .unwrap_or_default();
        let first = self
            .combo_options(index)
            .into_iter()
            .find(|(_, name)| typed.is_empty() || name.to_lowercase().contains(&typed.to_lowercase()));
        if let Some((value, name)) = first {
            self.choose(index, value, name, window, cx);
        } else if self.is_site(index) && site_address(&typed) {
            self.choose(index, typed.clone(), typed, window, cx);
        }
    }

    fn is_site(&self, index: usize) -> bool {
        self.simple
            .as_ref()
            .and_then(|simple| simple.params.get(index))
            .is_some_and(|param| param.type_name == "SharePoint.Site")
    }

    pub(super) fn sync_combos(&mut self, window: &mut Window, cx: &mut App) {
        if let Some(simple) = &mut self.simple {
            for field in &mut simple.fields {
                if let Some(combo) = &mut field.combo {
                    combo.sync(window, cx);
                }
            }
        }
    }

    /// Closes the list and shows the chosen name again in place of a filter.
    fn close_combo(&mut self, index: usize) {
        let name = self.chosen_name(index).unwrap_or_default();
        if let Some(combo) = self.combo_mut(index) {
            combo.open = false;
            if combo.typed {
                combo.show(name);
            }
        }
    }

    /// The choices of a field, as (value, name): the account's teams, the chosen team's channels, the times of day,
    /// the time zones, or what the connector listed.
    pub(super) fn combo_options(&self, index: usize) -> Vec<(String, String)> {
        let Some(simple) = &self.simple else {
            return Vec::new();
        };
        let Some(param) = simple.params.get(index) else {
            return Vec::new();
        };
        match param.kind {
            Kind::TeamsGroup => self
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
            Kind::TeamsChannel => simple
                .team_of(param)
                .and_then(|group| self.team_by_group(&group))
                .map(|team| {
                    team.channels
                        .iter()
                        .map(|channel| (channel.id.clone(), channel.name.clone()))
                        .collect()
                })
                .unwrap_or_default(),
            Kind::TimeZone => zones::ZONES
                .iter()
                .map(|(id, name)| ((*id).to_owned(), (*name).to_owned()))
                .collect(),
            Kind::TimeOfDay => (0..48)
                .map(|slot| {
                    let (hour, minute) = (slot / 2, slot % 2 * 30);
                    (format!("{hour:02}:{minute:02}"), markup::clock_name(hour, minute))
                })
                .collect(),
            _ => match simple.fields.get(index).map(|field| &field.choices) {
                Some(Choices::Ready(choices)) => choices
                    .iter()
                    .map(|choice| (choice.value.clone(), choice.name.clone()))
                    .collect(),
                _ => Vec::new(),
            },
        }
    }

    fn chosen_name(&self, index: usize) -> Option<String> {
        let param = self.simple.as_ref()?.params.get(index)?;
        if param.empty() {
            return None;
        }
        param.display.clone().or_else(|| param.text().map(str::to_owned))
    }

    /// Makes a choice; the fields that depend on it are cleared, as Teams clears the channel for a new team.
    pub(super) fn choose(
        &mut self,
        index: usize,
        value: String,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(simple) = &mut self.simple else {
            return;
        };
        let Some(param) = simple.params.get_mut(index) else {
            return;
        };
        let changed = param.text() != Some(value.as_str());
        param.value = Value::String(value);
        param.display = Some(name.clone());
        if let Some(combo) = simple.fields.get_mut(index).and_then(|field| field.combo.as_mut()) {
            combo.open = false;
            combo.show(name);
        }
        if changed {
            self.dependents_changed(index, cx);
        }
        self.sync_combos(window, cx);
        cx.notify();
    }

    /// A required combobox as Teams draws it: its label with a red star, the whole-width field, and its list while open.
    pub(super) fn combo_field(&self, index: usize, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let Some(combo) = self.combo(index) else {
            return div().into_any_element();
        };
        let options = self.combo_options(index);
        let chosen = self
            .simple
            .as_ref()
            .and_then(|simple| simple.params.get(index))
            .and_then(|param| param.text().map(str::to_owned));
        let filter = combo.filter(cx);
        let rows: Vec<&(String, String)> = options
            .iter()
            .filter(|(_, name)| filter.as_ref().is_none_or(|query| name.to_lowercase().contains(query)))
            .collect();
        let empty = if options.is_empty() {
            "No options available"
        } else {
            "No results match your search"
        };
        let input = combo.input.clone();
        let selector = combo.selector.clone();
        div()
            .relative()
            // A press in the field opens its list at once (its focus follows).
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    if let Some(combo) = this.combo_mut(index)
                        && !combo.open
                    {
                        combo.open = true;
                        combo.typed = false;
                        cx.notify();
                    }
                }),
            )
            .child(selected_by(
                selector.clone(),
                Input::new(&combo.input).suffix(
                    div()
                        .id(SharedString::from(format!("{selector}-chevron")))
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
                    gpui::deferred(div().absolute().top(px(38.0)).left_0().right_0().child(self.combo_list(
                        index,
                        &selector,
                        &rows,
                        chosen.as_ref(),
                        empty,
                        palette,
                        cx,
                    )))
                    .with_priority(1),
                )
            })
            .into_any_element()
    }

    #[expect(clippy::too_many_arguments, reason = "one list's parts, drawn together")]
    fn combo_list(
        &self,
        index: usize,
        selector: &str,
        rows: &[&(String, String)],
        chosen: Option<&String>,
        empty: &'static str,
        palette: Palette,
        cx: &Context<Self>,
    ) -> AnyElement {
        let list = div()
            .id(SharedString::from(format!("{selector}-list")))
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
        let loading = matches!(
            self.simple
                .as_ref()
                .and_then(|simple| simple.fields.get(index))
                .map(|field| &field.choices),
            Some(Choices::Loading)
        );
        let list = if loading {
            list.child(div().px_3().py_2().pl(px(38.0)).child("Loading…"))
        } else if rows.is_empty() {
            list.child(div().px_3().py_2().pl(px(38.0)).child(empty))
        } else {
            list.children(rows.iter().map(|(value, name)| {
                let row = format!("{selector}-{value}");
                let checked = chosen == Some(value);
                let (value, name) = (value.clone(), name.clone());
                div()
                    .id(SharedString::from(row.clone()))
                    .debug_selector(move || row.clone())
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
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.choose(index, value.clone(), name.clone(), window, cx);
                    }))
            }))
        };
        // Teams' site field also takes an address typed in: its list ends with "Enter a site URL".
        let input = self
            .combo(index)
            .filter(|_| self.is_site(index))
            .map(|combo| combo.input.clone());
        list.children(input.map(|input| site_row(input, selector, palette)))
            .into_any_element()
    }
}

/// The site list's last row, "Enter a site URL": it starts the field's text as an address to type.
fn site_row(input: Entity<InputState>, selector: &str, palette: Palette) -> gpui::Stateful<gpui::Div> {
    let start = move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
        input.update(cx, |state, cx| {
            state.set_value("https://", window, cx);
            state.focus(window, cx);
        });
    };
    let row = format!("{selector}-address");
    div()
        .id(SharedString::from(row.clone()))
        .debug_selector(move || row.clone())
        .px_3()
        .py_2()
        .pl(px(38.0))
        .cursor_pointer()
        .text_color(rgb(palette.brand))
        .child("Enter a site URL")
        .on_click(start)
}

/// Whether typed text is a site's address the SharePoint connector takes: `https` with a host.
fn site_address(text: &str) -> bool {
    text.strip_prefix("https://")
        .and_then(|rest| rest.split('/').next())
        .is_some_and(|host| host.contains('.') && !host.contains(char::is_whitespace))
}
