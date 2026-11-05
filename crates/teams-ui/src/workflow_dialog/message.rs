//! Workflows run from a message, in the Workflows dialog: a template with no choices to make (Follow up on a message,
//! Schedule a reply) shows Teams' instructions card, a picture and how to start it from a message, with Save; the
//! Overview of such a workflow adds "How to run this workflow after setup". Running one is
//! [`crate::workflow_run`]'s.
use super::*;

/// Teams' three steps for starting a workflow named `name` from a message.
fn steps(name: &str, palette: Palette) -> gpui::Div {
    let step = |number: usize, parts: Vec<(String, bool)>| {
        div()
            .flex()
            .gap_2()
            .child(div().flex_none().w(px(16.0)).child(format!("{number}.")))
            .child(bold_runs(parts, palette))
    };
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(step(
            1,
            vec![("Go to the message you want to run the workflow on.".to_owned(), false)],
        ))
        .child(step(
            2,
            vec![
                ("Select ".to_owned(), false),
                ("More options".to_owned(), true),
                ("(···).".to_owned(), false),
            ],
        ))
        .child(step(
            3,
            vec![
                ("Select ".to_owned(), false),
                (name.to_owned(), true),
                (", or find it under ".to_owned(), false),
                ("More actions".to_owned(), true),
                (".".to_owned(), false),
            ],
        ))
}

/// Text whose marked parts are semibold, wrapped as one paragraph.
fn bold_runs(parts: Vec<(String, bool)>, palette: Palette) -> gpui::StyledText {
    let mut text = String::new();
    let mut highlights = Vec::new();
    for (part, bold) in parts {
        let start = text.len();
        text.push_str(&part);
        if bold {
            highlights.push((
                start..text.len(),
                gpui::HighlightStyle {
                    font_weight: Some(gpui::FontWeight::SEMIBOLD),
                    color: Some(rgb(palette.foreground).into()),
                    ..gpui::HighlightStyle::default()
                },
            ));
        }
    }
    gpui::StyledText::new(text).with_highlights(highlights)
}

/// Teams' picture of a message's menu with a workflow in it.
fn picture(width: f32) -> gpui::Img {
    gpui::img(art::message_action())
        .w(px(width))
        .h(px(width * 120.0 / 280.0))
}

/// The instructions card's body: the picture, then "After you create this workflow, start it from a message." and
/// the three steps.
pub(super) fn instructions(name: &str, palette: Palette) -> AnyElement {
    div()
        .debug_selector(|| "workflow-message-instructions".to_owned())
        .flex()
        .flex_col()
        .gap_3()
        .max_w(px(470.0))
        .child(div().rounded_md().overflow_hidden().child(picture(466.0)))
        .child(
            div()
                .pt_3()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("After you create this workflow, start it from a message."),
        )
        .child(div().max_w(px(380.0)).child(steps(name, palette)))
        .into_any_element()
}

/// The Overview's "How to run this workflow after setup": the picture beside "Start this workflow directly from a
/// message" and the steps naming the workflow.
pub(super) fn how_to_run(name: &str, palette: Palette) -> AnyElement {
    div()
        .debug_selector(|| "workflow-how-to-run".to_owned())
        .flex()
        .flex_col()
        .gap_4()
        .child(
            div()
                .px_2()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("How to run this workflow after setup"),
        )
        .child(
            div()
                .flex()
                .items_start()
                .gap_6()
                .p_5()
                .rounded_lg()
                .border_1()
                .border_color(rgb(palette.border))
                .child(div().flex_none().rounded_md().overflow_hidden().child(picture(280.0)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child("Start this workflow directly from a message")
                        .child(steps(name, palette)),
                ),
        )
        .into_any_element()
}

/// Whether a saved workflow is one run from a message (its one trigger is Teams' "For a selected message").
pub(super) fn runs_from_message(card: &Card) -> bool {
    card.properties
        .get("definition")
        .and_then(teams_proto::workflow::message_trigger)
        .is_some()
}

/// Whether a workflow run from a message has nothing to edit: made from a template with no choices (its kept markup
/// names none), as Teams keeps Edit off for it.
pub(super) fn nothing_to_edit(card: &Card) -> bool {
    runs_from_message(card)
        && card
            .properties
            .pointer("/definition/actions/Do_Not_Remove_FlowIL/metadata/userIntentMarkup")
            .and_then(Value::as_str)
            .and_then(|kept| serde_json::from_str::<Value>(kept).ok())
            .is_none_or(|markup| markup::params(&markup).is_empty())
}

/// A workflow run from a message, as a message's menu lists it: its environment, id and name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlowRef {
    pub environment: String,
    pub id: String,
    pub name: String,
}

/// The longest run of list pages read for a message's menu.
const MENU_PAGES: usize = 10;

/// The account's workflows that run from a message and are on, as a message's menu lists them: Teams asks the
/// service for the flows whose trigger is "For a selected message"; the classic list names each flow's trigger in its
/// summary, so its pages are read and kept to those. The environment is the one the Workflows dialog uses.
///
/// # Errors
/// The service's failure to list the environments or the flows.
pub async fn message_workflows(core: &Arc<dyn Core>) -> Result<Vec<FlowRef>, WorkflowFailure> {
    let environments = core
        .workflow_request(WorkflowRequest::Environments)
        .await
        .unwrap_or(Err(WorkflowFailure::Connection))?;
    let names: Vec<String> = model::rows(&environments)?
        .iter()
        .filter_map(|row| row.get("name").and_then(Value::as_str).map(str::to_owned))
        .collect();
    let Some(environment) = names
        .iter()
        .find(|name| name.starts_with("Default-"))
        .or_else(|| names.first())
        .cloned()
    else {
        return Ok(Vec::new());
    };
    let mut flows = Vec::new();
    let mut next: Option<String> = None;
    for _ in 0..MENU_PAGES {
        let request = match next.take() {
            Some(next) => WorkflowRequest::Page {
                environment: environment.clone(),
                next,
            },
            None => WorkflowRequest::List {
                environment: environment.clone(),
            },
        };
        let answer = core
            .workflow_request(request)
            .await
            .unwrap_or(Err(WorkflowFailure::Connection))?;
        flows.extend(model::rows(&answer)?.iter().filter_map(|row| {
            let properties = row.get("properties")?;
            let on = properties.get("state").and_then(Value::as_str) == Some("Started");
            (on && teams_proto::workflow::summary_runs_from_message(properties)).then(|| FlowRef {
                environment: environment.clone(),
                id: row.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
                name: properties
                    .get("displayName")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            })
        }));
        next = model::next(&answer);
        if next.is_none() {
            break;
        }
    }
    flows.retain(|flow| !flow.id.is_empty());
    Ok(flows)
}
