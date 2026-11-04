//! Run and webhook details stay service reads. Copying an address never invokes it, and run action content URLs are
//! never followed. A returned URI cannot become a place to send the user's bearer token.
use super::*;

impl WorkflowDialog {
    /// Teams' "Copy webhook link": a link-coloured action whose tooltip shows the address once it has been read.
    pub(super) fn webhook_copy(&self, card: &Card, palette: Palette, cx: &Context<Self>) -> Option<AnyElement> {
        let trigger = webhook_trigger(card)?;
        let flow = card.id.clone();
        let shown = self.webhook_link.clone();
        Some(
            div()
                .id("workflow-copy-webhook")
                .debug_selector(|| "workflow-copy-webhook".to_owned())
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .text_color(rgb(palette.brand))
                .hover(gpui::Styled::underline)
                .child(Glyph::COPY.icon().size(px(16.0)))
                .child("Copy webhook link")
                .when_some(shown, |link, shown| {
                    link.tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(shown.clone()).build(window, cx)
                    })
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.busy {
                        this.copy_webhook(flow.clone(), trigger.clone(), cx);
                    }
                }))
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
                                    this.webhook_link = Some(link.to_owned());
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

/// The name of the workflow's Teams webhook trigger, if it has one.
pub(super) fn webhook_trigger(card: &Card) -> Option<String> {
    let triggers = card.properties.pointer("/definition/triggers")?.as_object()?;
    triggers
        .iter()
        .find(|(_, value)| {
            value
                .get("kind")
                .and_then(Value::as_str)
                .is_some_and(|kind| matches!(kind, "TeamsWebhook" | "TeamsWebhookV2"))
        })
        .map(|(trigger, _)| trigger.clone())
}
