//! The Overview's Owners, as Teams shows them below Run history: what an owner may do, each owner with their initial
//! and name ("Owner" beside the one who made the workflow, Remove beside a co-owner), then "Add a co-owner" with a
//! search box. Typing searches the organization's directory and lists who it finds above the box (name and address),
//! or "<text> is not in your organization's directory."; picking someone makes them a co-owner at once.
use std::time::Duration;

use super::*;
use crate::icons::Spinning as _;
use teams_proto::{Principal, TeamPerson};

/// One owner of the open workflow: their directory object id, their name once known, and whether they made it.
#[derive(Clone)]
pub(super) struct Owner {
    id: String,
    name: Option<String>,
    creator: bool,
}

/// What the co-owner search found for its text.
pub(super) struct Found {
    query: String,
    people: Vec<TeamPerson>,
}

impl WorkflowDialog {
    /// Reads the open workflow's owners, then each one's name.
    pub(super) fn load_owners(&mut self, card: &Card, cx: &mut Context<Self>) {
        let Some(environment) = self.environment.clone() else {
            return;
        };
        let generation = self.generation;
        let creator = card
            .properties
            .pointer("/creator/objectId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let flow = card.id.clone();
        let core = Arc::clone(&self.core);
        self.tasks
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let answer = core
                    .workflow_request(WorkflowRequest::Owners {
                        environment,
                        flow: flow.clone(),
                    })
                    .await
                    .unwrap_or(Err(WorkflowFailure::Connection))
                    .and_then(|value| model::rows(&value).map(principals));
                let owners = match answer {
                    Ok(ids) => {
                        let mut owners = Vec::new();
                        for id in ids {
                            let name = owner_name(&core, &id, cx).await;
                            owners.push(Owner {
                                creator: creator.as_deref() == Some(id.as_str()),
                                id,
                                name,
                            });
                        }
                        Ok(owners)
                    }
                    Err(error) => Err(error),
                };
                let _ = this.update(cx, |this, cx| {
                    if this.generation == generation && this.open_flow_id() == Some(flow.as_str()) {
                        this.owners = Some(owners);
                        cx.notify();
                    }
                });
            }));
    }

    fn open_flow_id(&self) -> Option<&str> {
        match &self.screen {
            Screen::Detail(card) | Screen::Delete(card) => Some(&card.id),
            _ => None,
        }
    }

    /// Searches the directory for the co-owner box's text; a later text's answer replaces an earlier one's.
    pub(super) fn search_owners(&mut self, cx: &mut Context<Self>) {
        let query = self.owner_search.read(cx).value().trim().to_owned();
        self.owner_query = self.owner_query.wrapping_add(1);
        if query.is_empty() {
            self.owner_found = None;
            cx.notify();
            return;
        }
        let asked = self.owner_query;
        let reply = self.core.addable_people(query.clone());
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let people = starting_with(reply.await.unwrap_or_default(), &query);
            let _ = this.update(cx, |this, cx| {
                if this.owner_query == asked {
                    this.owner_found = Some(Found { query, people });
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn add_owner(&mut self, person: &TeamPerson, window: &mut Window, cx: &mut Context<Self>) {
        // a pick while another change goes keeps the box as it is: nothing is sent for it
        if self.busy || self.uncertain.is_some() {
            return;
        }
        let (Some(environment), Some(flow), Some(id)) = (
            self.environment.clone(),
            self.open_flow_id().map(str::to_owned),
            teams_proto::object_id(&person.mri),
        ) else {
            return;
        };
        let owner = Principal {
            id: id.to_owned(),
            name: person.name.clone(),
            email: person.email.clone(),
        };
        self.owner_found = None;
        self.owner_search
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.change_owners(
            WorkflowRequest::AddOwner {
                environment,
                flow,
                owner,
            },
            cx,
        );
    }

    fn remove_owner(&mut self, id: String, cx: &mut Context<Self>) {
        let (Some(environment), Some(flow)) = (self.environment.clone(), self.open_flow_id().map(str::to_owned)) else {
            return;
        };
        self.change_owners(
            WorkflowRequest::RemoveOwner {
                environment,
                flow,
                owner: id,
            },
            cx,
        );
    }

    /// Adds or removes an owner, the owners' Remove buttons showing it working until it is over.
    fn change_owners(&mut self, request: WorkflowRequest, cx: &mut Context<Self>) {
        self.changing = Some(Change::Owners);
        self.mutate_then(request, Outcome::Owners, cx);
        if !self.busy {
            self.changing = None;
        }
    }

    pub(super) fn owners_section(&self, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let mut list = div()
            .flex()
            .flex_col()
            .p_5()
            .gap_4()
            .rounded_md()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .child(learn_more(palette));
        match &self.owners {
            None => list = list.child(div().text_color(rgb(palette.muted_foreground)).child("Loading owners…")),
            Some(Err(error)) => {
                list = list.child(
                    div()
                        .text_color(rgb(theme::DANGER))
                        .child(format!("Owners could not be loaded. {}", model::problem(error))),
                );
            }
            Some(Ok(owners)) => {
                let first_is_owner = !owners.iter().any(|owner| owner.creator);
                for (at, owner) in owners.iter().enumerate() {
                    list = list.child(self.owner_row(owner, owner.creator || (first_is_owner && at == 0), palette, cx));
                }
            }
        }
        if !self.core.read_only() {
            list = list.child(self.add_owner_box(palette, cx));
        }
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(div().px_2().font_weight(gpui::FontWeight::SEMIBOLD).child("Owners"))
            .child(list)
            .into_any_element()
    }

    fn owner_row(&self, owner: &Owner, main: bool, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let name = owner.name.clone().unwrap_or_else(|| "Unknown person".to_owned());
        let id = owner.id.clone();
        div()
            .debug_selector({
                let selector = format!("workflow-owner-{}", owner.id);
                move || selector.clone()
            })
            .flex()
            .items_center()
            .gap_3()
            .pb_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(initial(&name, palette))
            .child(div().flex_1().min_w_0().truncate().child(name))
            .child(if main || self.core.read_only() {
                div()
                    .px_4()
                    .child(if main { "Owner" } else { "Co-owner" })
                    .into_any_element()
            } else {
                selected_by(
                    format!("workflow-owner-remove-{}", owner.id),
                    Button::new(SharedString::from(format!("workflow-owner-remove-{}", owner.id)))
                        .outline()
                        .label("Remove")
                        .spinning(self.busy && self.changing == Some(Change::Owners))
                        .disabled(self.busy || self.uncertain.is_some())
                        .on_click(cx.listener(move |this, _, _, cx| this.remove_owner(id.clone(), cx))),
                )
                .into_any_element()
            })
            .into_any_element()
    }

    fn add_owner_box(&self, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let typed = !self.owner_search.read(cx).value().trim().is_empty();
        let field = div()
            .relative()
            .w(px(480.0))
            .max_w_full()
            .child(selected_by(
                "workflow-owner-search",
                Input::new(&self.owner_search)
                    .prefix(Glyph::SEARCH.icon().size(px(16.0)))
                    .cleanable(typed)
                    .disabled(self.busy),
            ))
            .when_some(self.owner_found.as_ref().filter(|_| typed), |field, found| {
                field.child(
                    gpui::deferred(
                        div()
                            .absolute()
                            .bottom(px(40.0))
                            .left_0()
                            .right_0()
                            .child(Self::found_list(found, palette, cx)),
                    )
                    .with_priority(1),
                )
            });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(Glyph::USER_PLUS.icon().size(px(20.0)))
                    .child("Add a co-owner"),
            )
            .child(div().text_sm().child("Enter names, email or user groups"))
            .child(field)
            .into_any_element()
    }

    fn found_list(found: &Found, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let list = div()
            .debug_selector(|| "workflow-owner-found".to_owned())
            .p_1()
            .flex()
            .flex_col()
            .rounded_md()
            .bg(rgb(palette.surface))
            .border_1()
            .border_color(rgb(palette.border))
            .shadow_lg()
            .occlude()
            .on_mouse_down(gpui::MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            });
        if found.people.is_empty() {
            return list
                .child(
                    div()
                        .px_6()
                        .py_3()
                        .child(format!("{} is not in your organization's directory.", found.query)),
                )
                .into_any_element();
        }
        list.children(found.people.iter().take(10).map(|person| {
            let selector = format!("workflow-owner-pick-{}", person.mri);
            let chosen = person.clone();
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .flex()
                .items_center()
                .gap_3()
                .px_3()
                .py_1p5()
                .rounded_md()
                .cursor_pointer()
                .hover(|row| row.bg(rgb(palette.hover)))
                .child(initial(&person.name, palette))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(div().truncate().child(person.name.clone()))
                        .children(person.email.clone().map(|email| {
                            div()
                                .text_xs()
                                .text_color(rgb(palette.muted_foreground))
                                .truncate()
                                .child(email)
                        })),
                )
                .on_click(cx.listener(move |this, _, window, cx| this.add_owner(&chosen, window, cx)))
        }))
        .into_any_element()
    }
}

/// The owners' ids, in the service's order.
fn principals(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .filter_map(|row| row.pointer("/properties/principal/id").and_then(Value::as_str))
        .map(str::to_owned)
        .collect()
}

/// An owner's name as the engine knows it, looked up when it doesn't yet (asked again once the lookup had time).
async fn owner_name(core: &Arc<dyn Core>, id: &str, cx: &mut AsyncApp) -> Option<String> {
    let mri = format!("8:orgid:{id}");
    for _ in 0..3 {
        let person = core.person(mri.clone()).await.ok()?;
        if person.name.is_some() || !person.looking {
            return person.name;
        }
        cx.background_executor().timer(Duration::from_millis(700)).await;
    }
    None
}

/// Teams' owner picture when it has none: their first letter in a grey circle.
pub(super) fn initial(name: &str, palette: Palette) -> gpui::Div {
    div()
        .size(px(32.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(rgb(palette.hover))
        .text_sm()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(
            name.chars()
                .next()
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_default(),
        )
}

/// What owners may do, with Teams' "Learn more" link at its end, in the same run of text.
fn learn_more(palette: Palette) -> AnyElement {
    const TEXT: &str = concat!(
        "Adding an owner gives them full control of this flow. They'll be able to add or remove other users as ",
        "owners, access the run history, and can update, edit or delete this flow. ",
    );
    let link = TEXT.len()..TEXT.len() + "Learn more".len();
    let style = gpui::HighlightStyle {
        color: Some(rgb(palette.brand).into()),
        ..gpui::HighlightStyle::default()
    };
    div()
        .debug_selector(|| "workflow-owners-learn-more".to_owned())
        .child(
            gpui::InteractiveText::new(
                "workflow-owners-learn-more",
                gpui::StyledText::new(format!("{TEXT}Learn more")).with_highlights([(link.clone(), style)]),
            )
            .on_click(vec![link], |_, _, cx| cx.open_url(LEARN_MORE)),
        )
        .into_any_element()
}

/// Where Teams' "Learn more" about owners leads: Microsoft's page on sharing a flow with co-owners.
const LEARN_MORE: &str = "https://go.microsoft.com/fwlink/?linkid=839912";

/// The people a directory search found whose name or address starts with the text, as Teams' own search asks (Graph's
/// `startswith` on the display name, mail and sign-in name): "ai2" finds AI2, not AI3.
pub(crate) fn starting_with(people: Vec<TeamPerson>, query: &str) -> Vec<TeamPerson> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return people;
    }
    people
        .into_iter()
        .filter(|person| {
            let name = person.name.to_lowercase();
            name.starts_with(&query)
                || person
                    .email
                    .as_deref()
                    .is_some_and(|email| email.to_lowercase().starts_with(&query))
        })
        .collect()
}
