//! Run and webhook details stay service reads. Copying an address never invokes it, and run action content URLs are
//! never followed. A returned URI cannot become a place to send the user's bearer token.
use super::*;

impl WorkflowDialog {
    pub(super) fn webhook_copy(&self, card: &Card, cx: &Context<Self>) -> Option<AnyElement> {
        let triggers = card.properties.pointer("/definition/triggers")?.as_object()?;
        let (trigger, _) = triggers.iter().find(|(_, value)| {
            value
                .get("kind")
                .and_then(Value::as_str)
                .is_some_and(|kind| matches!(kind, "TeamsWebhook" | "TeamsWebhookV2"))
        })?;
        let (flow, trigger) = (card.id.clone(), trigger.clone());
        Some(
            selected_by(
                "workflow-copy-webhook",
                Button::new("workflow-copy-webhook")
                    .label("Copy webhook URL")
                    .icon(Glyph::COPY.icon())
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, _, cx| this.copy_webhook(flow.clone(), trigger.clone(), cx))),
            )
            .into_any_element(),
        )
    }

    fn copy_webhook(&mut self, flow: String, trigger: String, cx: &mut Context<Self>) {
        let Some(environment) = self.environment.clone() else {
            return;
        };
        let generation = self.generation;
        let reply = self.core.workflow_request(WorkflowRequest::WebhookAddress {
            environment,
            flow,
            trigger,
        });
        self.busy = true;
        self.tasks
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let answer = reply.await.unwrap_or(Err(WorkflowFailure::Connection));
                let _ =
                    this.update(cx, |this, cx| {
                        if this.generation != generation {
                            return;
                        }
                        this.busy = false;
                        match answer {
                            Ok(value) => match value
                                .get("value")
                                .or_else(|| value.pointer("/response/value"))
                                .and_then(Value::as_str)
                                .filter(|link| link.starts_with("https://"))
                            {
                                Some(link) => {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(link.to_owned()));
                                    this.notice = Some("Webhook URL copied".to_owned());
                                }
                                None => this.error = Some(
                                    "Power Automate did not return a webhook URL. Refresh the workflow and try again."
                                        .to_owned(),
                                ),
                            },
                            Err(error) => this.error = Some(model::problem(&error).to_owned()),
                        }
                        cx.notify();
                    });
            }));
        cx.notify();
    }

    pub(super) fn run_details(&mut self, card: Card, run: Value, cx: &mut Context<Self>) {
        let (Some(environment), Some(id)) = (self.environment.clone(), run.get("name").and_then(Value::as_str)) else {
            return;
        };
        let generation = self.generation;
        let reply = self.core.workflow_request(WorkflowRequest::RunActions {
            environment,
            flow: card.id.clone(),
            run: id.to_owned(),
        });
        self.busy = true;
        self.tasks
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let answer = reply.await.unwrap_or(Err(WorkflowFailure::Connection));
                let _ = this.update(cx, |this, cx| {
                    if generation != this.generation {
                        return;
                    }
                    this.busy = false;
                    match answer.and_then(|value| model::rows(&value).map(<[Value]>::to_vec)) {
                        Ok(actions) => this.screen = Screen::RunDetails { card, run, actions },
                        Err(error) => this.error = Some(model::problem(&error).to_owned()),
                    }
                    cx.notify();
                });
            }));
        cx.notify();
    }

    pub(super) fn run_view(card: &Card, run: &Value, actions: &[Value], palette: Palette) -> AnyElement {
        let mut view = div().flex().flex_col().gap_3().child(
            div()
                .text_lg()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(format!("{} · Run details", card.name)),
        );
        view = view.child(format!(
            "Status: {}",
            run.pointer("/properties/status")
                .and_then(Value::as_str)
                .unwrap_or("Unknown")
        ));
        for action in actions {
            let name = action
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("Action")
                .replace('_', " ");
            let status = action
                .pointer("/properties/status")
                .and_then(Value::as_str)
                .unwrap_or("Unknown");
            let code = action
                .pointer("/properties/error/code")
                .and_then(Value::as_str)
                .unwrap_or_default();
            view = view.child(
                div()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(palette.border))
                    .child(format!("{name} · {status} {code}")),
            );
        }
        view.into_any_element()
    }
}
