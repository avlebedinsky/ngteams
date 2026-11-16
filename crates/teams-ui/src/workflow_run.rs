//! Running a workflow on a message, as Teams does from a message's "More options" (or "More actions"): a dialog
//! titled with the workflow's name over "Workflows via Power Automate", showing the questions its trigger asks (the
//! trigger's own input card: text, a date, a time, a switch, a choice or people), each required one marked with a red
//! star and its own error message under it when Submit finds it empty. Submit runs the workflow once on that message,
//! through the engine's outbox; Teams then closes the dialog and the workflow carries on in the service (a reminder in
//! the Workflows chat, a reply posted later, a Planner task). A failure keeps the dialog open and says what happened.
//!
//! The card is the workflow's own, read again when the dialog opens; its answers and the message go to the workflow's
//! Logic Flows runtime as the Workflows bot sends them there (`teams_proto::MessageRun`). No account credential goes
//! to the bot or any app.
mod card;
pub mod menu;

use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AsyncApp, Context, EventEmitter, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, Task, WeakEntity, Window, div, px, rgb,
};
use gpui_component::FocusTrapElement as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Disableable as _, Sizable as _};
use serde_json::{Map, Value};
use teams_proto::{MessageRun, MessageSender, WorkflowFailure, WorkflowRequest};

use crate::bridge::Core;
use crate::icons::{Glyph, Spinning as _, icon_button, selected_by};
use crate::team_dialogs::TeamDialogEvent;
use crate::theme::{self, Palette};
use crate::workflow_dialog::FlowRef;
use card::Element;

/// The message a workflow runs on: its conversation and id, the post a channel reply belongs to (a post's own id for
/// a post), whether it is a channel's, and its team's thread and group (which the shell fills in from the teams it
/// holds).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageRef {
    pub conversation: String,
    pub id: String,
    pub root: Option<String>,
    pub in_channel: bool,
    pub team: Option<(String, String)>,
}

/// Where the dialog stands.
enum Stage {
    /// Reading the workflow and the message.
    Loading,
    /// The card's questions, and the trigger to run.
    Ready { trigger: String },
    /// The workflow or the message couldn't be read.
    Failed(String),
}

/// The run dialog.
pub struct WorkflowRun {
    core: Arc<dyn Core>,
    flow: FlowRef,
    message: MessageRef,
    focus: FocusHandle,
    stage: Stage,
    elements: Vec<Element>,
    context: Option<MessageRun>,
    busy: bool,
    error: Option<String>,
    tasks: Vec<Task<()>>,
}

impl EventEmitter<TeamDialogEvent> for WorkflowRun {}

impl WorkflowRun {
    /// Opens on `flow` and `message`: the workflow is read again for its questions, and the message for what the run
    /// tells the workflow about it.
    pub fn new(
        core: Arc<dyn Core>,
        flow: FlowRef,
        message: MessageRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let mut this = Self {
            core,
            flow,
            message,
            focus,
            stage: Stage::Loading,
            elements: Vec::new(),
            context: None,
            busy: false,
            error: None,
            tasks: Vec::new(),
        };
        this.load(window, cx);
        this
    }

    /// The workflow it runs.
    #[must_use]
    pub fn flow(&self) -> &FlowRef {
        &self.flow
    }

    fn load(&mut self, window: &Window, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        let (flow, message) = (self.flow.clone(), self.message.clone());
        let window = window.window_handle();
        self.tasks
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let read = core
                    .workflow_request(WorkflowRequest::Get {
                        environment: flow.environment.clone(),
                        flow: flow.id.clone(),
                    })
                    .await
                    .unwrap_or(Err(WorkflowFailure::Connection));
                let context = message_context(&core, &flow.environment, &message).await;
                let _ = window.update(cx, |_, window, cx| {
                    this.update(cx, |this, cx| this.loaded(read, context, window, cx))
                });
            }));
    }

    fn loaded(
        &mut self,
        read: Result<Value, WorkflowFailure>,
        context: Option<MessageRun>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let trigger = read.as_ref().ok().and_then(|flow| {
            let definition = flow.pointer("/properties/definition")?;
            let (name, trigger) = teams_proto::workflow::message_trigger(definition)?;
            Some((name.to_owned(), teams_proto::workflow::input_card(trigger)))
        });
        self.stage = match (read, trigger, &context) {
            (Err(error), ..) => Stage::Failed(problem(&error).to_owned()),
            (Ok(_), None, _) => Stage::Failed("This workflow doesn't run from a message.".to_owned()),
            (Ok(_), Some(_), None) => {
                Stage::Failed("The message couldn't be read. Open the chat again and try once more.".to_owned())
            }
            (Ok(_), Some((name, card)), Some(_)) => {
                self.elements = card::elements(card.as_ref(), window, cx);
                if card::answerable(card.as_ref(), &self.elements) {
                    Stage::Ready { trigger: name }
                } else {
                    // a required question the app can't draw: the run's check would refuse every Submit
                    self.elements.clear();
                    Stage::Failed(
                        "This workflow asks something the app can't show yet. Run it from Teams on the web.".to_owned(),
                    )
                }
            }
        };
        self.context = context;
        cx.notify();
    }

    /// Submit: the required questions must be answered (each unanswered one shows its own message); then the run goes
    /// out once, and the dialog closes when the service took it.
    pub fn submit(&mut self, cx: &mut Context<Self>) {
        let Stage::Ready { trigger } = &self.stage else {
            return;
        };
        if self.busy || self.core.read_only() {
            return;
        }
        let Some(context) = self.context.clone() else {
            return;
        };
        let mut answers = Map::new();
        let mut complete = true;
        for element in &mut self.elements {
            if let Element::Input(input) = element {
                match input.answer(cx) {
                    Ok(Some(value)) => {
                        answers.insert(input.id.clone(), Value::String(value));
                    }
                    Ok(None) => {}
                    Err(()) => complete = false,
                }
            }
        }
        cx.notify();
        if !complete {
            return;
        }
        self.busy = true;
        self.error = None;
        let reply = self.core.workflow_request(WorkflowRequest::RunOnMessage {
            environment: self.flow.environment.clone(),
            flow: self.flow.id.clone(),
            trigger: trigger.clone(),
            answers,
            message: Box::new(context),
        });
        self.tasks
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let answer = reply.await.unwrap_or(Err(WorkflowFailure::Connection));
                let _ = this.update(cx, |this, cx| {
                    this.busy = false;
                    match answer {
                        Ok(_) => cx.emit(TeamDialogEvent::Closed),
                        Err(error) => this.error = Some(run_problem(&error).to_owned()),
                    }
                    cx.notify();
                });
            }));
        cx.notify();
    }

    fn header(&self, palette: Palette, cx: &Context<Self>) -> AnyElement {
        div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex_none()
                    .size(px(32.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(0x00e0_e0e0))
                    .bg(rgb(0x00ff_ffff))
                    .child(gpui::img(crate::workflow_dialog::workflows_icon()).size(px(22.0))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .debug_selector(|| "workflow-run-title".to_owned())
                            .truncate()
                            .text_sm()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(self.flow.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette.muted_foreground))
                            .child("Workflows via Power Automate"),
                    ),
            )
            .child(selected_by(
                "workflow-run-close",
                icon_button("workflow-run-close", Glyph::X, "Close")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(TeamDialogEvent::Closed))),
            ))
            .into_any_element()
    }

    fn body(&self, palette: Palette, cx: &Context<Self>) -> AnyElement {
        match &self.stage {
            Stage::Loading => div()
                .debug_selector(|| "workflow-run-loading".to_owned())
                .flex()
                .justify_center()
                .py_4()
                .child(Button::new("workflow-run-spinner").ghost().large().spinning(true))
                .min_h(px(180.0))
                .into_any_element(),
            Stage::Failed(words) => div()
                .debug_selector(|| "workflow-run-failed".to_owned())
                .py_4()
                .text_color(rgb(theme::DANGER))
                .child(words.clone())
                .into_any_element(),
            Stage::Ready { .. } => div()
                .flex()
                .flex_col()
                .gap_2()
                .children(
                    self.elements
                        .iter()
                        .enumerate()
                        .map(|(index, element)| card::view(element, index, self.busy, palette, cx)),
                )
                .into_any_element(),
        }
    }

    fn footer(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if !matches!(self.stage, Stage::Ready { .. }) || self.core.read_only() {
            return None;
        }
        Some(
            div()
                .flex()
                .justify_end()
                .child(selected_by(
                    "workflow-run-submit",
                    Button::new("workflow-run-submit")
                        .outline()
                        .label("Submit")
                        .disabled(self.busy)
                        .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                ))
                .into_any_element(),
        )
    }
}

impl Render for WorkflowRun {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        div()
            .id("workflow-run")
            .debug_selector(|| "workflow-run".to_owned())
            .occlude()
            .w(px(600.0))
            .max_w_full()
            .flex()
            .flex_col()
            .gap_6()
            .p_8()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .text_color(rgb(palette.foreground))
            .shadow_lg()
            .key_context("WorkflowRun")
            .track_focus(&self.focus)
            .focus_trap("workflow-run-trap", &self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && !this.busy {
                    cx.stop_propagation();
                    cx.emit(TeamDialogEvent::Closed);
                }
            }))
            .child(self.header(palette, cx))
            .child(
                div()
                    .id("workflow-run-scroll")
                    .max_h(px(560.0))
                    .overflow_y_scroll()
                    .child(self.body(palette, cx)),
            )
            .when_some(self.error.as_ref(), |view, error| {
                view.child(
                    div()
                        .debug_selector(|| "workflow-run-error".to_owned())
                        .text_sm()
                        .text_color(rgb(theme::DANGER))
                        .child(error.clone()),
                )
            })
            .children(self.footer(cx))
    }
}

/// What the run tells the workflow about the message and who runs it, read from the store and the engine's profiles.
async fn message_context(core: &Arc<dyn Core>, environment: &str, message: &MessageRef) -> Option<MessageRun> {
    let stored = core
        .message(message.conversation.clone(), message.id.clone())
        .await
        .ok()
        .flatten()?;
    let me = core.me().to_owned();
    let actor_name = core.person(me.clone()).await.ok().and_then(|person| person.name);
    let sender = match stored.sender.as_deref() {
        Some(mri) => {
            let named = core.person(mri.to_owned()).await.ok().and_then(|person| person.name);
            let name = named.or_else(|| stored.sender_name.clone()).unwrap_or_default();
            match (teams_proto::object_id(mri), mri.strip_prefix("28:")) {
                (Some(id), _) => MessageSender::User {
                    id: id.to_owned(),
                    name,
                },
                (None, Some(app)) => MessageSender::App {
                    id: app.to_owned(),
                    name,
                },
                (None, None) => MessageSender::Unknown,
            }
        }
        None => MessageSender::Unknown,
    };
    // a plain-text message is text whatever it holds ("a <b> tag"), as the chat shows it; its HTML is it escaped
    let (html, text) = if stored.message_type == teams_proto::chat::MessageType::RichTextHtml.as_str() {
        let text = teams_proto::chat::parse_html(&stored.content)
            .plain_text()
            .trim()
            .to_owned();
        (stored.content, text)
    } else {
        (
            crate::notify::escape_markup(&stored.content),
            stored.content.trim().to_owned(),
        )
    };
    Some(MessageRun {
        id: message.id.clone(),
        reply_to: message.root.clone(),
        sender,
        link: MessageRun::link_to(&message.conversation, &message.id, message.root.as_deref()),
        html,
        text,
        conversation: message.conversation.clone(),
        team: message.team.clone(),
        tenant: environment.strip_prefix("Default-").map(str::to_owned),
        actor_object_id: teams_proto::object_id(&me).unwrap_or_default().to_owned(),
        actor: me,
        actor_name: actor_name.unwrap_or_default(),
        time_zone: crate::workflow_dialog::local_time_zone().to_owned(),
    })
}

/// Why the workflow couldn't be read, in the user's words.
fn problem(error: &WorkflowFailure) -> &'static str {
    match error {
        WorkflowFailure::NotFound => "This workflow no longer exists.",
        WorkflowFailure::Authentication => "Sign in again to use Workflows.",
        WorkflowFailure::Forbidden => "You don't have access to this workflow.",
        WorkflowFailure::ReadOnly => "Workflows can't run in read-only mode.",
        _ => "The workflow couldn't be loaded. Try again.",
    }
}

/// Why the run didn't go, in the user's words. An unknown outcome is said as one: the run is never sent again by
/// itself.
fn run_problem(error: &WorkflowFailure) -> &'static str {
    match error {
        WorkflowFailure::Connection => {
            "The connection dropped before Workflows answered, so the workflow may have started. Check its run history before you submit again."
        }
        // a gateway's 5xx may come after the run was taken
        WorkflowFailure::Service(500..=599) => {
            "Workflows didn't say whether the workflow started. Check its run history before you submit again."
        }
        WorkflowFailure::InvalidInput => "Check your answers and submit again.",
        WorkflowFailure::Changed => "This workflow changed. Close this and run it again.",
        WorkflowFailure::Throttled => "Workflows is busy. Wait a moment and submit again.",
        WorkflowFailure::ReadOnly => "Workflows can't run in read-only mode.",
        WorkflowFailure::Service(_) | WorkflowFailure::InvalidResponse => {
            "The workflow couldn't start. Try again, or check it in Workflows."
        }
        other => problem(other),
    }
}
