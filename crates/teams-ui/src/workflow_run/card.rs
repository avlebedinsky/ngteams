//! A message trigger's input card (`inputsAdaptiveCard`), drawn natively as Teams draws it in the run dialog: each
//! `TextBlock` a line of text, each `Input.*` a labelled control. Inputs this build can't draw stay out; the run
//! still checks the answers against the freshly read card before it goes.
use chrono::{NaiveDate, NaiveTime, Timelike as _};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::date_picker::{DatePicker, DatePickerEvent, DatePickerState};
use gpui_component::input::{Input as TextInput, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{Disableable as _, Sizable as _};
use serde_json::Value;
use teams_proto::TeamPerson;

use super::WorkflowRun;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::theme::{self, Palette};

/// One element of the card.
pub(super) enum Element {
    /// A line of text: bold and larger as the card's `weight` and `size` say.
    Text {
        text: String,
        bold: bool,
        large: bool,
    },
    Input(Box<Input>),
}

/// One question.
pub(super) struct Input {
    pub(super) id: String,
    label: Option<String>,
    required: bool,
    /// The card's own words for an unanswered required question.
    error_message: String,
    /// Submit found it unanswered (or its answer unreadable): its message shows under it.
    missing: bool,
    control: Control,
    _subscriptions: Vec<Subscription>,
}

/// A question's control.
enum Control {
    Text(Entity<InputState>),
    Multiline(Entity<TextareaState>),
    Date {
        state: Entity<DatePickerState>,
        picked: Option<NaiveDate>,
    },
    Time(Entity<InputState>),
    Toggle {
        title: String,
        on: bool,
        value_on: String,
        value_off: String,
    },
    Choice {
        choices: Vec<(String, String)>,
        multi: bool,
        chosen: Vec<String>,
    },
    /// People from the directory (a card's `graph.microsoft.com/users` choices): a search and the chosen.
    People {
        search: Entity<InputState>,
        found: Vec<TeamPerson>,
        chosen: Vec<TeamPerson>,
        asked: u64,
    },
}

/// The card's elements, with their controls made: its body's, and those inside its containers (a `Container`'s
/// `items`, a `ColumnSet`'s `columns`, a `Column`'s `items`) in their order, as the run's check finds its inputs
/// ([`teams_proto::workflow::card_inputs`]).
pub(super) fn elements(card: Option<&Value>, window: &mut Window, cx: &mut Context<WorkflowRun>) -> Vec<Element> {
    let mut elements = Vec::new();
    if let Some(body) = card.and_then(|card| card.get("body")) {
        collect(body, &mut elements, window, cx);
    }
    elements
}

/// Whether every required question of `card` has a control among `elements`: one this build can't draw would leave
/// the card unanswerable, as the run's check requires it.
pub(super) fn answerable(card: Option<&Value>, elements: &[Element]) -> bool {
    let Some(card) = card else {
        return true;
    };
    teams_proto::workflow::card_inputs(card)
        .into_iter()
        .filter(|input| input.get("isRequired").and_then(Value::as_bool) == Some(true))
        .filter_map(|input| input.get("id").and_then(Value::as_str))
        .all(|id| {
            elements
                .iter()
                .any(|element| matches!(element, Element::Input(input) if input.id == id))
        })
}

/// The elements of `items` (one element or a list), containers' contents included.
fn collect(items: &Value, elements: &mut Vec<Element>, window: &mut Window, cx: &mut Context<WorkflowRun>) {
    let items: Vec<&Value> = match items {
        Value::Array(items) => items.iter().collect(),
        Value::Object(_) => vec![items],
        _ => Vec::new(),
    };
    for item in items {
        let kind = item.get("type").and_then(Value::as_str).unwrap_or_default();
        if !kind.starts_with("Input.") && kind != "TextBlock" {
            for key in ["items", "columns"] {
                if let Some(inner) = item.get(key) {
                    collect(inner, elements, window, cx);
                }
            }
            continue;
        }
        if kind == "TextBlock" {
            let text = item.get("text").and_then(Value::as_str).unwrap_or_default();
            if !text.is_empty() {
                elements.push(Element::Text {
                    text: text.to_owned(),
                    bold: item
                        .get("weight")
                        .and_then(Value::as_str)
                        .is_some_and(|weight| weight.eq_ignore_ascii_case("bolder")),
                    large: item.get("size").and_then(Value::as_str).is_some_and(|size| {
                        matches!(size.to_ascii_lowercase().as_str(), "medium" | "large" | "extralarge")
                    }),
                });
            }
            continue;
        }
        let index = elements.len();
        if let Some(input) = input(item, index, window, cx) {
            elements.push(Element::Input(Box::new(input)));
        }
    }
}

/// One `Input.*` element's question, when this build draws its kind.
fn input(item: &Value, index: usize, window: &mut Window, cx: &mut Context<WorkflowRun>) -> Option<Input> {
    let id = item.get("id").and_then(Value::as_str)?.to_owned();
    let placeholder = item
        .get("placeholder")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let mut subscriptions = Vec::new();
    let control = match item.get("type").and_then(Value::as_str)? {
        "Input.Text" if item.get("isMultiline").and_then(Value::as_bool) == Some(true) => {
            Control::Multiline(cx.new(|cx| TextareaState::new(window, cx).placeholder(placeholder)))
        }
        "Input.Text" | "Input.Number" => {
            Control::Text(cx.new(|cx| InputState::new(window, cx).placeholder(placeholder)))
        }
        "Input.Date" => {
            let state = cx.new(|cx| DatePickerState::new(window, cx).date_format("%B %-d, %Y"));
            subscriptions.push(cx.subscribe(&state, move |this, _, event: &DatePickerEvent, cx| {
                let DatePickerEvent::Change(picked) = event;
                if let Some(Element::Input(input)) = this.elements.get_mut(index)
                    && let Control::Date { picked: slot, .. } = &mut input.control
                {
                    *slot = picked.start().map(|time| time.date());
                    input.missing = false;
                    cx.notify();
                }
            }));
            Control::Date { state, picked: None }
        }
        "Input.Time" => Control::Time(cx.new(|cx| InputState::new(window, cx).placeholder("Select a time..."))),
        "Input.Toggle" => toggle(item),
        "Input.ChoiceSet"
            if item.pointer("/choices.data/dataset").and_then(Value::as_str) == Some("graph.microsoft.com/users") =>
        {
            let search = cx.new(|cx| InputState::new(window, cx).placeholder("Select people"));
            subscriptions.push(cx.subscribe(&search, move |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.search_people(index, cx);
                }
            }));
            Control::People {
                search,
                found: Vec::new(),
                chosen: Vec::new(),
                asked: 0,
            }
        }
        "Input.ChoiceSet" => choice_set(item),
        _ => return None,
    };
    Some(Input {
        id,
        label: item.get("label").and_then(Value::as_str).map(str::to_owned),
        required: item.get("isRequired").and_then(Value::as_bool) == Some(true),
        error_message: item
            .get("errorMessage")
            .and_then(Value::as_str)
            .unwrap_or("This is required.")
            .to_owned(),
        missing: false,
        control,
        _subscriptions: subscriptions,
    })
}

/// A switch, with the values the card names for on and off (`true`, `false` by default).
fn toggle(item: &Value) -> Control {
    let word = |key: &str, default: &str| item.get(key).and_then(Value::as_str).unwrap_or(default).to_owned();
    let value_on = word("valueOn", "true");
    Control::Toggle {
        title: word("title", ""),
        on: item.get("value").and_then(Value::as_str) == Some(value_on.as_str()),
        value_on,
        value_off: word("valueOff", "false"),
    }
}

/// A choice from the card's own list, one or several.
fn choice_set(item: &Value) -> Control {
    Control::Choice {
        choices: item
            .get("choices")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|choice| {
                Some((
                    choice.get("title")?.as_str()?.to_owned(),
                    choice.get("value")?.as_str()?.to_owned(),
                ))
            })
            .collect(),
        multi: item.get("isMultiSelect").and_then(Value::as_bool) == Some(true),
        chosen: item
            .get("value")
            .and_then(Value::as_str)
            .map(|value| value.split(',').map(str::to_owned).collect())
            .unwrap_or_default(),
    }
}

impl Input {
    /// Its answer as the card sends it (a date `2026-11-05`, a time `08:00`, a switch's value, choices joined by
    /// commas); `None` when it was left empty and may be; an error, marking it, when a required one is empty or the
    /// time typed can't be read.
    pub(super) fn answer(&mut self, cx: &gpui::App) -> Result<Option<String>, ()> {
        let value = match &self.control {
            Control::Text(state) | Control::Time(state) => state.read(cx).value().trim().to_owned(),
            Control::Multiline(state) => state.read(cx).value().trim().to_owned(),
            Control::Date { picked, .. } => picked.map(|day| day.format("%Y-%m-%d").to_string()).unwrap_or_default(),
            Control::Toggle {
                on,
                value_on,
                value_off,
                ..
            } => (if *on { value_on } else { value_off }).clone(),
            Control::Choice { chosen, .. } => chosen.join(","),
            Control::People { chosen, .. } => chosen
                .iter()
                .filter_map(|person| teams_proto::object_id(&person.mri))
                .collect::<Vec<_>>()
                .join(","),
        };
        let value = match &self.control {
            Control::Time(_) if !value.is_empty() => {
                let Some(time) = parse_time(&value) else {
                    self.missing = true;
                    return Err(());
                };
                format!("{:02}:{:02}", time.hour(), time.minute())
            }
            _ => value,
        };
        self.missing = self.required && value.is_empty();
        if self.missing {
            return Err(());
        }
        Ok((!value.is_empty()).then_some(value))
    }
}

/// A time as typed or chosen: `8:00 AM`, `8 am`, `20:00`.
pub(super) fn parse_time(text: &str) -> Option<NaiveTime> {
    let mut text = text.trim().to_uppercase().replace('.', "");
    // an hour alone ("8 pm") is on the hour
    if !text.contains(':') {
        let digits = text.chars().take_while(char::is_ascii_digit).count();
        text.insert_str(digits, ":00");
    }
    ["%I:%M %p", "%I:%M%p", "%H:%M"]
        .iter()
        .find_map(|format| NaiveTime::parse_from_str(&text, format).ok())
}

/// A half hour as the time list names it: `8:30 AM`.
fn clock(minutes: u32) -> String {
    let (hour, minute) = (minutes / 60, minutes % 60);
    let shown = if hour % 12 == 0 { 12 } else { hour % 12 };
    format!("{shown}:{minute:02} {}", if hour < 12 { "AM" } else { "PM" })
}

impl WorkflowRun {
    /// A people question's search: the directory, by name or address start, as Teams' people pickers ask it.
    pub(super) fn search_people(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(Element::Input(input)) = self.elements.get_mut(index) else {
            return;
        };
        let Control::People {
            search, found, asked, ..
        } = &mut input.control
        else {
            return;
        };
        let query = search.read(cx).value().trim().to_owned();
        *asked = asked.wrapping_add(1);
        let ask = *asked;
        if query.is_empty() {
            found.clear();
            cx.notify();
            return;
        }
        let reply = self.core.addable_people(query.clone());
        cx.spawn(async move |this, cx| {
            let people = crate::workflow_dialog::starting_with(reply.await.unwrap_or_default(), &query);
            let _ = this.update(cx, |this, cx| {
                if let Some(Element::Input(input)) = this.elements.get_mut(index)
                    && let Control::People { found, asked, .. } = &mut input.control
                    && *asked == ask
                {
                    *found = people;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn pick_person(&mut self, index: usize, person: &TeamPerson, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Element::Input(input)) = self.elements.get_mut(index)
            && let Control::People {
                search, found, chosen, ..
            } = &mut input.control
        {
            if !chosen.iter().any(|known| known.mri == person.mri) {
                chosen.push(person.clone());
            }
            found.clear();
            search.update(cx, |state, cx| state.set_value("", window, cx));
            input.missing = false;
        }
        cx.notify();
    }

    fn unpick_person(&mut self, index: usize, mri: &str, cx: &mut Context<Self>) {
        if let Some(Element::Input(input)) = self.elements.get_mut(index)
            && let Control::People { chosen, .. } = &mut input.control
        {
            chosen.retain(|person| person.mri != mri);
        }
        cx.notify();
    }

    fn flip(&mut self, index: usize, checked: bool, cx: &mut Context<Self>) {
        if let Some(Element::Input(input)) = self.elements.get_mut(index)
            && let Control::Toggle { on, .. } = &mut input.control
        {
            *on = checked;
        }
        cx.notify();
    }

    fn choose(&mut self, index: usize, value: &str, cx: &mut Context<Self>) {
        if let Some(Element::Input(input)) = self.elements.get_mut(index)
            && let Control::Choice { chosen, multi, .. } = &mut input.control
        {
            if *multi {
                if chosen.iter().any(|known| known == value) {
                    chosen.retain(|known| known != value);
                } else {
                    chosen.push(value.to_owned());
                }
            } else {
                *chosen = vec![value.to_owned()];
            }
            input.missing = false;
        }
        cx.notify();
    }
}

/// One element, drawn.
pub(super) fn view(
    element: &Element,
    index: usize,
    busy: bool,
    palette: Palette,
    cx: &Context<WorkflowRun>,
) -> AnyElement {
    match element {
        Element::Text { text, bold, large } => div()
            .when(*bold, |line| line.font_weight(gpui::FontWeight::BOLD))
            .when(*large, gpui::Styled::text_base)
            .when(!*large, gpui::Styled::text_sm)
            .child(text.clone())
            .into_any_element(),
        Element::Input(input) => {
            let control = control(input, index, busy, palette, cx);
            div()
                .flex()
                .flex_col()
                .gap_1()
                .when_some(input.label.clone(), |field, label| {
                    field.child(
                        div()
                            .flex()
                            .gap_1()
                            .text_sm()
                            .child(label)
                            .when(input.required, |label| {
                                label.child(div().text_color(rgb(theme::DANGER)).child("*"))
                            }),
                    )
                })
                .child(control)
                .when(input.missing, |field| {
                    field.child(
                        div()
                            .debug_selector({
                                let id = input.id.clone();
                                move || format!("workflow-run-missing-{id}")
                            })
                            .text_sm()
                            .text_color(rgb(theme::DANGER))
                            .child(input.error_message.clone()),
                    )
                })
                .into_any_element()
        }
    }
}

fn control(input: &Input, index: usize, busy: bool, palette: Palette, cx: &Context<WorkflowRun>) -> AnyElement {
    let selector = format!("workflow-run-input-{}", input.id);
    match &input.control {
        Control::Text(state) => selected_by(selector, TextInput::new(state).disabled(busy)).into_any_element(),
        Control::Multiline(state) => {
            selected_by(selector, Textarea::new(state).h(px(88.0)).disabled(busy)).into_any_element()
        }
        Control::Date { state, .. } => selected_by(
            selector,
            DatePicker::new(state)
                .placeholder("Select a date...")
                .w_full()
                .disabled(busy),
        )
        .into_any_element(),
        Control::Time(state) => time_field(state, &selector, busy, cx),
        Control::Toggle { title, on, .. } => selected_by(
            selector.clone(),
            Checkbox::new(gpui::SharedString::from(selector))
                .label(title.clone())
                .checked(*on)
                .disabled(busy)
                .on_click(cx.listener(move |this, checked: &bool, _, cx| this.flip(index, *checked, cx))),
        )
        .into_any_element(),
        Control::Choice { choices, chosen, .. } => div()
            .flex()
            .flex_wrap()
            .gap_2()
            .children(choices.iter().map(|(title, value)| {
                let picked = chosen.contains(value);
                let value = value.clone();
                selected_by(
                    format!("{selector}-{value}"),
                    Button::new(gpui::SharedString::from(format!("{selector}-{value}")))
                        .outline()
                        .small()
                        .label(title.clone())
                        .when(picked, gpui_component::button::ButtonVariants::primary)
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, _, cx| this.choose(index, &value, cx))),
                )
            }))
            .into_any_element(),
        Control::People {
            search, found, chosen, ..
        } => people_field(search, found, chosen, index, &selector, busy, palette, cx),
    }
}

/// A time: typed, or chosen from the half hours under its chevron, as Teams' time picker offers them.
fn time_field(state: &Entity<InputState>, selector: &str, busy: bool, cx: &Context<WorkflowRun>) -> AnyElement {
    let target = state.clone();
    let weak = cx.entity().downgrade();
    selected_by(
        selector.to_owned(),
        TextInput::new(state).disabled(busy).suffix(
            icon_button(
                gpui::SharedString::from(format!("{selector}-times")),
                Glyph::CHEVRON_DOWN,
                "Choose a time",
            )
            .ghost()
            .xsmall()
            .disabled(busy)
            .dropdown_menu(move |mut menu, _, _| {
                for half in 0..48_u32 {
                    let shown = clock(half * 30);
                    let (target, weak, words) = (target.clone(), weak.clone(), shown.clone());
                    menu = menu.item(
                        PopupMenuItem::element(move |_, _| {
                            let key = format!("workflow-run-time-{}", shown.replace([' ', ':'], ""));
                            div().debug_selector(move || key.clone()).child(shown.clone())
                        })
                        .on_click(move |_, window, cx| {
                            target.update(cx, |state, cx| state.set_value(words.clone(), window, cx));
                            let _ = weak.update(cx, |_, cx| cx.notify());
                        }),
                    );
                }
                menu.scrollable(true).max_h(px(240.0))
            }),
        ),
    )
    .into_any_element()
}

#[expect(clippy::too_many_arguments, reason = "one field's parts, drawn together")]
fn people_field(
    search: &Entity<InputState>,
    found: &[TeamPerson],
    chosen: &[TeamPerson],
    index: usize,
    selector: &str,
    busy: bool,
    palette: Palette,
    cx: &Context<WorkflowRun>,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .when(!chosen.is_empty(), |field| {
            field.child(div().flex().flex_wrap().gap_1().children(chosen.iter().map(|person| {
                let mri = person.mri.clone();
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .pl_2()
                    .rounded_full()
                    .bg(rgb(palette.hover))
                    .text_sm()
                    .child(person.name.clone())
                    .child(
                        icon_button(
                            gpui::SharedString::from(format!("{selector}-remove-{}", person.mri)),
                            Glyph::X,
                            "Remove",
                        )
                        .ghost()
                        .xsmall()
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, _, cx| this.unpick_person(index, &mri, cx))),
                    )
            })))
        })
        .child(selected_by(selector.to_owned(), TextInput::new(search).disabled(busy)))
        .when(!found.is_empty(), |field| {
            field.child(
                div()
                    .flex()
                    .flex_col()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(palette.border))
                    .children(found.iter().take(8).map(|person| {
                        let picked = person.clone();
                        div()
                            .id(gpui::SharedString::from(format!("{selector}-found-{}", person.mri)))
                            .px_3()
                            .py_1p5()
                            .cursor_pointer()
                            .hover(move |row| row.bg(rgb(palette.hover)))
                            .child(div().text_sm().child(person.name.clone()))
                            .when_some(person.email.clone(), |row, email| {
                                row.child(div().text_xs().text_color(rgb(palette.muted_foreground)).child(email))
                            })
                            .on_click(
                                cx.listener(move |this, _, window, cx| this.pick_person(index, &picked, window, cx)),
                            )
                    })),
            )
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{clock, parse_time};

    #[test]
    fn times_read_as_typed_or_chosen() {
        assert_eq!(
            parse_time("8:00 AM").map(|time| time.to_string()),
            Some("08:00:00".to_owned())
        );
        assert_eq!(
            parse_time("8 pm").map(|time| time.to_string()),
            Some("20:00:00".to_owned())
        );
        assert_eq!(
            parse_time("20:30").map(|time| time.to_string()),
            Some("20:30:00".to_owned())
        );
        assert_eq!(parse_time("soon"), None);
        assert_eq!(clock(0), "12:00 AM");
        assert_eq!(clock(13 * 60 + 30), "1:30 PM");
    }
}
